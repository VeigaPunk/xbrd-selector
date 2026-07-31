//! ufo-cli — pure Rust local rover (mailbox substrate = JSONL file)
//! Shell pilots intentionally use POSIX `sh`; the implementation stays Rust-only.

use anyhow::{bail, Context, Result};
use auth::sanitize_terminal;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use ufo_auth as auth;
use uuid::Uuid;

const APP_DIR: &str = ".ufo";
const ROVERS_FILE: &str = "rovers.json";
const MAILBOX_FILE: &str = "mailbox.jsonl";
const LOCAL_MODEL_TIMEOUT_SECS: u64 = 8;
const LOCAL_MODEL_MAX_RESPONSE_BYTES: usize = 64 * 1024;
const LOCAL_MODEL_MAX_PROMPT_CHARS: usize = 16_384;
const LOCAL_MODEL_MAX_MODEL_CHARS: usize = 128;

#[derive(Parser, Debug)]
#[command(
    name = "ufo",
    about = "UFO local rover CLI (pure Rust, local JSONL mailbox)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Enroll a local rover (stores identity under ~/.ufo)
    Enroll {
        #[arg(long, default_value = "local")]
        name: String,
        #[arg(long, default_value_t = 1)]
        units: u32,
        #[arg(long)]
        tags: Vec<String>,
    },
    /// Start the rover loop: pull ops from local mailbox, execute in worktrees
    Start {
        #[arg(long)]
        headless: bool,
        #[arg(long, default_value_t = 2)]
        poll_secs: u64,
    },
    /// Push a synthetic operation into the local mailbox (for testing)
    Push {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "echo 'hello from pilot'")]
        pilot_cmd: String,
    },
    /// List mailbox contents
    Mailbox,
    /// Auth (cloned from OpenCode auth connection)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Send one prompt to a strictly local OpenAI-compatible model endpoint
    LocalModel {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        prompt: String,
    },
    /// Model operations
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
}

#[derive(Subcommand, Debug)]
enum ModelAction {
    /// Send one prompt to a strictly local OpenAI-compatible model endpoint
    Prompt {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        prompt: String,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// List providers from OpenCode ~/.local/share/opencode/auth.json or ~/.ufo/auth.json
    List,
    /// List provider policy and OAuth state
    Providers,
    /// Delegate OAuth login to opencode
    Login {
        #[arg(long)]
        provider: OAuthProviderId,
    },
    /// Delegate OAuth logout to opencode
    Logout {
        #[arg(long)]
        provider: OAuthProviderId,
    },
    /// Show which store is active
    Status,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum OAuthProviderId {
    #[value(name = "openai")]
    OpenAI,
    #[value(name = "github-copilot")]
    GithubCopilot,
}

impl OAuthProviderId {
    fn as_str(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::GithubCopilot => "github-copilot",
        }
    }
}

#[derive(Debug, Clone)]
struct OpencodeAuthProcess {
    program: PathBuf,
}

impl Default for OpencodeAuthProcess {
    fn default() -> Self {
        Self {
            program: PathBuf::from("opencode"),
        }
    }
}

impl OpencodeAuthProcess {
    async fn login(&self, provider: OAuthProviderId) -> Result<()> {
        let args = ["auth", "login", "--pure", "--provider", provider.as_str()];
        self.run(args.as_slice()).await
    }

    async fn logout(&self, provider: OAuthProviderId) -> Result<()> {
        let args = ["auth", "logout", "--provider", provider.as_str()];
        self.run(args.as_slice()).await
    }

    async fn run(&self, args: &[&str]) -> Result<()> {
        let status = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .context("run opencode auth command")?;
        if status.success() {
            Ok(())
        } else {
            bail!("opencode auth command exited {:?}", status.code())
        }
    }
}

fn format_provider_summary(item: &auth::ProviderSummary) -> String {
    let oauth_state = item
        .oauth
        .as_ref()
        .map(|oauth| oauth.expiry_state.to_string())
        .unwrap_or_else(|| "-".to_string());
    let account_id = item
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.account_id.as_deref())
        .map(sanitize_terminal)
        .unwrap_or_else(|| "-".to_string());
    let enterprise_url = item
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.enterprise_url.as_deref())
        .map(sanitize_terminal)
        .unwrap_or_else(|| "-".to_string());
    format!(
        "{}  source={} policy={} oauth={} account={} enterprise={} metadata={}",
        sanitize_terminal(&item.provider_id),
        item.source,
        item.policy,
        oauth_state,
        account_id,
        enterprise_url,
        if item.metadata_present {
            "present"
        } else {
            "-"
        }
    )
}

fn verify_login_summary(
    snapshot: &auth::AuthSnapshot,
    provider: OAuthProviderId,
) -> Result<auth::ProviderSummary> {
    let summary = snapshot
        .store
        .oauth_summary(provider.as_str(), snapshot.source.clone())
        .with_context(|| format!("missing OAuth entry for {}", provider.as_str()))?;
    if summary.policy != auth::ProviderPolicy::Usable {
        bail!("OAuth provider {} is ignored", provider.as_str());
    }
    let oauth = summary.oauth.as_ref().context("missing OAuth state")?;
    if oauth.expiry_state != auth::ExpiryState::Valid {
        bail!("OAuth provider {} is expired", provider.as_str());
    }
    Ok(summary)
}

fn verify_logout_summary(
    snapshot: &auth::AuthSnapshot,
    provider: OAuthProviderId,
) -> Result<Option<auth::ProviderSummary>> {
    match snapshot
        .store
        .oauth_summary(provider.as_str(), snapshot.source.clone())
    {
        Some(summary) if summary.policy == auth::ProviderPolicy::Usable => {
            let oauth = summary.oauth.as_ref().context("missing OAuth state")?;
            if oauth.expiry_state == auth::ExpiryState::Valid {
                bail!(
                    "OAuth provider {} still usable after logout",
                    provider.as_str()
                );
            }
            Ok(Some(summary))
        }
        other => Ok(other),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoverEntry {
    id: String,
    name: String,
    units: u32,
    tags: Vec<String>,
    #[serde(with = "rfc3339_datetime")]
    enrolled_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Operation {
    id: String,
    title: String,
    pilot_cmd: String,
    status: String,
    #[serde(with = "rfc3339_datetime")]
    created_at: DateTime<Utc>,
    #[serde(with = "rfc3339_datetime::option")]
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessageRequest<'a>>,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessageRequest<'a> {
    role: &'a str,
    content: &'a str,
}

struct MailboxLock(File);

impl Drop for MailboxLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn set_private_mode(path: &Path, dir: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = if dir { 0o700 } else { 0o600 };
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms)?;
    }
    let _ = (path, dir);
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    set_private_mode(path, true)?;
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<()> {
    if path.exists() {
        set_private_mode(path, false)?;
    }
    Ok(())
}

fn app_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    let d = home.join(APP_DIR);
    ensure_no_symlink_components(&d)?;
    ensure_private_dir(&d)?;
    Ok(d)
}

fn ensure_no_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => bail!("unsafe path component in {:?}", path),
            Component::Normal(seg) => current.push(seg),
        }
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                bail!("unsafe symlink component in {:?}", path);
            }
        }
    }
    Ok(())
}

fn rovers_path() -> Result<PathBuf> {
    let path = app_dir()?.join(ROVERS_FILE);
    ensure_no_symlink_components(&path)?;
    Ok(path)
}

fn mailbox_path() -> Result<PathBuf> {
    let path = app_dir()?.join(MAILBOX_FILE);
    ensure_no_symlink_components(&path)?;
    Ok(path)
}

fn mailbox_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

fn acquire_mailbox_lock(path: &Path, shared: bool) -> Result<MailboxLock> {
    let lock_path = mailbox_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    if shared {
        file.lock_shared()?;
    } else {
        file.lock_exclusive()?;
    }
    Ok(MailboxLock(file))
}

mod rfc3339_datetime {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(
        value: &DateTime<Utc>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_rfc3339())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = DateTime<Utc>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an RFC3339 string or unix timestamp")
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                DateTime::<Utc>::from_timestamp(value, 0)
                    .ok_or_else(|| E::custom("invalid unix timestamp"))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::visit_i64(self, value as i64)
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                DateTime::parse_from_rfc3339(value)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(E::custom)
            }
        }

        deserializer.deserialize_any(Visitor)
    }

    pub mod option {
        use super::*;
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S>(
            value: &Option<DateTime<Utc>>,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match value {
                Some(dt) => serializer.serialize_some(&dt.to_rfc3339()),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D>(
            deserializer: D,
        ) -> std::result::Result<Option<DateTime<Utc>>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
            match raw {
                None => Ok(None),
                Some(serde_json::Value::String(s)) => DateTime::parse_from_rfc3339(&s)
                    .map(|dt| Some(dt.with_timezone(&Utc)))
                    .map_err(serde::de::Error::custom),
                Some(serde_json::Value::Number(n)) => n
                    .as_i64()
                    .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
                    .map(Some)
                    .ok_or_else(|| serde::de::Error::custom("invalid unix timestamp")),
                Some(other) => Err(serde::de::Error::custom(format!(
                    "unexpected datetime value: {other}"
                ))),
            }
        }
    }
}

struct TempWrite {
    path: PathBuf,
    committed: bool,
}

impl Drop for TempWrite {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn atomic_write_string(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("missing parent directory")?;
    fs::create_dir_all(parent)?;

    let tmp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(OsStr::to_str).unwrap_or("ufo"),
        Uuid::new_v4()
    ));
    let mut cleanup = TempWrite {
        path: tmp_path.clone(),
        committed: false,
    };

    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
    }
    set_private_mode(&tmp_path, false)?;
    fs::rename(&tmp_path, path)?;

    let dir = OpenOptions::new().read(true).open(parent)?;
    dir.sync_all()?;
    ensure_private_file(path)?;
    cleanup.committed = true;
    Ok(())
}

fn load_rovers() -> Result<Vec<RoverEntry>> {
    let p = rovers_path()?;
    if !p.exists() {
        return Ok(vec![]);
    }
    let s = fs::read_to_string(&p)?;
    Ok(serde_json::from_str(&s).unwrap_or_default())
}

fn save_rovers(rovers: &[RoverEntry]) -> Result<()> {
    atomic_write_string(&rovers_path()?, &serde_json::to_string_pretty(rovers)?)
}

fn read_mailbox(path: &Path) -> Result<Vec<Operation>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut s = String::new();
    File::open(path)?.read_to_string(&mut s)?;
    let mut ops = Vec::new();
    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(op) = serde_json::from_str::<Operation>(line) {
            ops.push(op);
        }
    }
    Ok(ops)
}

fn load_mailbox_from(path: &Path) -> Result<Vec<Operation>> {
    let _lock = acquire_mailbox_lock(path, true)?;
    read_mailbox(path)
}

fn append_op_to(path: &Path, op: &Operation) -> Result<()> {
    let _lock = acquire_mailbox_lock(path, false)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(op)?)?;
    file.flush()?;
    file.sync_all()?;
    ensure_private_file(path)?;
    Ok(())
}

fn write_mailbox_locked(path: &Path, ops: &[Operation]) -> Result<()> {
    let mut body = String::new();
    for op in ops {
        body.push_str(&serde_json::to_string(op)?);
        body.push('\n');
    }
    atomic_write_string(path, &body)
}

fn claim_next_mailbox_op(path: &Path) -> Result<Option<Operation>> {
    let _lock = acquire_mailbox_lock(path, false)?;
    let mut ops = read_mailbox(path)?;
    let Some(index) = ops.iter().position(|op| op.status == "queued") else {
        return Ok(None);
    };
    if !is_safe_path_component(&ops[index].id) {
        bail!("unsafe op id: {}", ops[index].id);
    }
    ops[index].status = "running".to_string();
    let claimed = ops[index].clone();
    write_mailbox_locked(path, &ops)?;
    Ok(Some(claimed))
}

fn finalize_mailbox_op(
    path: &Path,
    op_id: &str,
    status: &str,
    finished_at: Option<DateTime<Utc>>,
) -> Result<bool> {
    let _lock = acquire_mailbox_lock(path, false)?;
    let mut ops = read_mailbox(path)?;
    let mut changed = false;
    for op in &mut ops {
        if op.id == op_id {
            op.status = status.to_string();
            op.finished_at = finished_at;
            changed = true;
            break;
        }
    }
    if changed {
        write_mailbox_locked(path, &ops)?;
    }
    Ok(changed)
}

fn load_mailbox() -> Result<Vec<Operation>> {
    let path = mailbox_path()?;
    load_mailbox_from(&path)
}

fn append_op(op: &Operation) -> Result<()> {
    let path = mailbox_path()?;
    append_op_to(&path, op)
}

async fn execute_op(op: &Operation, work_root: &Path) -> Result<()> {
    ensure_safe_path_component(&op.id)?;
    let op_dir = work_root.join(&op.id);
    ensure_no_symlink_components(&op_dir)?;
    fs::create_dir_all(&op_dir)?;
    set_private_mode(&op_dir, true)?;
    println!("[ufo] running op {} in {:?}", op.id, op_dir);

    let status = Command::new("sh")
        .arg("-c")
        .arg(&op.pilot_cmd)
        .current_dir(&op_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("pilot spawn failed")?;

    if status.success() {
        println!("[ufo] op {} done", op.id);
        Ok(())
    } else {
        bail!("pilot exited {:?}", status.code())
    }
}

async fn rover_loop(poll_secs: u64) -> Result<()> {
    let work_root = app_dir()?.join("work");
    ensure_no_symlink_components(&work_root)?;
    fs::create_dir_all(&work_root)?;
    set_private_mode(&work_root, true)?;
    println!(
        "[ufo] rover loop started, mailbox={:?}, poll={}s",
        mailbox_path()?,
        poll_secs
    );

    loop {
        let mailbox = mailbox_path()?;
        match claim_next_mailbox_op(&mailbox)? {
            Some(op) => {
                let outcome = match execute_op(&op, &work_root).await {
                    Ok(()) => "done",
                    Err(e) => {
                        eprintln!("[ufo] op {} failed: {e:#}", op.id);
                        "failed"
                    }
                };
                let finished_at = Some(Utc::now());
                if !finalize_mailbox_op(&mailbox, &op.id, outcome, finished_at)? {
                    eprintln!("[ufo] finalize lost op {}", op.id);
                }
            }
            None => sleep(Duration::from_secs(poll_secs)).await,
        }
    }
}

fn is_safe_path_component(id: &str) -> bool {
    if id.is_empty() || id == "." || id == ".." {
        return false;
    }
    let path = Path::new(id);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(seg)), None) => seg == OsStr::new(id),
        _ => false,
    }
}

fn ensure_safe_path_component(id: &str) -> Result<()> {
    if is_safe_path_component(id) {
        Ok(())
    } else {
        bail!("unsafe op id: {id}")
    }
}

fn validate_local_model_endpoint(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("parse endpoint URL")?;
    if url.scheme() != "http" {
        bail!("endpoint must use http");
    }
    if url.username() != "" || url.password().is_some() {
        bail!("endpoint userinfo is not allowed");
    }
    if url.query().is_some() {
        bail!("endpoint query strings are not allowed");
    }
    if url.fragment().is_some() {
        bail!("endpoint fragments are not allowed");
    }
    if url.path() != "/v1" && url.path() != "/v1/" {
        bail!("endpoint path must be /v1");
    }
    match url.port() {
        Some(port) if port != 0 => {}
        _ => bail!("endpoint must include an explicit nonzero port"),
    }
    let host = url.host_str().context("endpoint must include a host")?;
    let ip = host
        .parse::<std::net::IpAddr>()
        .context("endpoint host must be a literal loopback IP address")?;
    match ip {
        std::net::IpAddr::V4(ipv4) => {
            if !ipv4.is_loopback() || ipv4.is_unspecified() || ipv4.is_broadcast() {
                bail!("endpoint must target a local loopback address");
            }
        }
        std::net::IpAddr::V6(ipv6) => {
            if ipv6 != std::net::Ipv6Addr::LOCALHOST {
                bail!("endpoint must target a local loopback address");
            }
        }
    }
    Ok(url)
}

fn validate_model_id(raw: &str) -> Result<&str> {
    let model = raw.trim();
    if model.is_empty() {
        bail!("model id must not be empty");
    }
    if model.chars().count() > LOCAL_MODEL_MAX_MODEL_CHARS {
        bail!("model id too long");
    }
    if model.chars().any(|ch| ch.is_control()) {
        bail!("model id contains control characters");
    }
    Ok(model)
}

fn local_model_chat_url(endpoint: &Url) -> Result<Url> {
    let mut endpoint = endpoint.clone();
    if endpoint.path() == "/v1" {
        endpoint.set_path("/v1/");
    }
    endpoint
        .join("chat/completions")
        .context("build chat/completions URL")
}

async fn post_local_model_prompt(endpoint: &str, model: &str, prompt: &str) -> Result<String> {
    if prompt.chars().count() > LOCAL_MODEL_MAX_PROMPT_CHARS {
        bail!("prompt too large");
    }
    let endpoint = validate_local_model_endpoint(endpoint)?;
    let model = validate_model_id(model)?;
    let url = local_model_chat_url(&endpoint)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(LOCAL_MODEL_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(2))
        .no_proxy()
        .redirect(Policy::none())
        .build()
        .context("build HTTP client")?;
    let request = ChatCompletionsRequest {
        model,
        messages: vec![ChatMessageRequest {
            role: "user",
            content: prompt,
        }],
        temperature: 0.0,
        stream: false,
    };
    let response = client
        .post(url)
        .json(&request)
        .send()
        .await
        .context("send local model request")?;
    if !response.status().is_success() {
        bail!("local model request failed: {}", response.status());
    }

    let mut size = 0usize;
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("read local model response")?
    {
        size += chunk.len();
        if size > LOCAL_MODEL_MAX_RESPONSE_BYTES {
            bail!("local model response exceeded size limit");
        }
        body.extend_from_slice(&chunk);
    }

    let parsed: ChatCompletionsResponse =
        serde_json::from_slice(&body).context("parse local model response")?;
    let content = parsed
        .choices
        .into_iter()
        .find_map(|choice| match choice.message.role.as_deref() {
            Some("assistant") => choice.message.content,
            None if choice.message.content.is_some() => choice.message.content,
            _ => None,
        })
        .context("local model response missing content")?;
    Ok(sanitize_terminal(&content))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::Enroll { name, units, tags } => {
            let mut rovers = load_rovers()?;
            let entry = RoverEntry {
                id: Uuid::new_v4().to_string(),
                name,
                units,
                tags,
                enrolled_at: Utc::now(),
            };
            println!("[ufo] enrolled rover id={} name={}", entry.id, entry.name);
            rovers.push(entry);
            save_rovers(&rovers)?;
        }
        Commands::Start {
            headless: _,
            poll_secs,
        } => {
            let rovers = load_rovers()?;
            if rovers.is_empty() {
                bail!("no rovers enrolled — run `ufo enroll` first");
            }
            println!("[ufo] {} rover(s) loaded", rovers.len());
            rover_loop(poll_secs).await?;
        }
        Commands::Push { title, pilot_cmd } => {
            let op = Operation {
                id: Uuid::new_v4().to_string(),
                title,
                pilot_cmd,
                status: "queued".into(),
                created_at: Utc::now(),
                finished_at: None,
            };
            append_op(&op)?;
            println!("[ufo] pushed op id={} title={}", op.id, op.title);
        }
        Commands::Mailbox => {
            let ops = load_mailbox()?;
            for op in ops {
                println!(
                    "{} | {} | {} | {}",
                    op.id, op.status, op.title, op.pilot_cmd
                );
            }
        }
        Commands::Auth { action } => match action {
            AuthAction::List => {
                let snapshot = auth::load_auth()?;
                let list = snapshot.store.summaries(snapshot.source.clone());
                if list.is_empty() {
                    println!(
                        "[ufo] no providers (OpenCode auth: env, XDG_DATA_HOME/opencode/auth.json, then ~/.local/share/opencode/auth.json)"
                    );
                } else {
                    for item in list {
                        println!("{}", format_provider_summary(&item));
                    }
                    if snapshot.malformed_entries > 0 {
                        println!(
                            "[ufo] skipped malformed entries: {}",
                            snapshot.malformed_entries
                        );
                    }
                }
            }
            AuthAction::Providers => {
                let snapshot = auth::load_auth()?;
                let list = snapshot.store.summaries(snapshot.source.clone());
                if list.is_empty() {
                    println!(
                        "[ufo] no providers (OpenCode auth: env, XDG_DATA_HOME/opencode/auth.json, then ~/.local/share/opencode/auth.json)"
                    );
                } else {
                    for item in list {
                        println!("{}", format_provider_summary(&item));
                    }
                    if snapshot.malformed_entries > 0 {
                        println!(
                            "[ufo] skipped malformed entries: {}",
                            snapshot.malformed_entries
                        );
                    }
                }
            }
            AuthAction::Login { provider } => {
                let process = OpencodeAuthProcess::default();
                process.login(provider).await?;
                let snapshot = auth::load_auth()?;
                let summary = verify_login_summary(&snapshot, provider)?;
                println!("[ufo] oauth login {}", format_provider_summary(&summary));
            }
            AuthAction::Logout { provider } => {
                let process = OpencodeAuthProcess::default();
                process.logout(provider).await?;
                let snapshot = auth::load_auth()?;
                match verify_logout_summary(&snapshot, provider)? {
                    Some(summary) => println!("[ufo] oauth logout {}", format_provider_summary(&summary)),
                    None => println!(
                        "[ufo] oauth logout {}  source={} policy=ignored oauth=missing account=- enterprise=- metadata=-",
                        provider.as_str(),
                        snapshot.source,
                    ),
                }
            }
            AuthAction::Status => {
                let snapshot = auth::load_auth()?;
                println!("OpenCode source: {}", snapshot.source);
                println!("OpenCode file: {:?}", snapshot.resolved_path);
                println!("loaded providers: {}", snapshot.store.len());
                println!("usable providers: {}", snapshot.store.usable_count());
                println!("skipped malformed entries: {}", snapshot.malformed_entries);
            }
        },
        Commands::LocalModel {
            endpoint,
            model,
            prompt,
        } => {
            let content = post_local_model_prompt(&endpoint, &model, &prompt).await?;
            println!("{}", content);
        }
        Commands::Model { action } => match action {
            ModelAction::Prompt {
                endpoint,
                model,
                prompt,
            } => {
                let content = post_local_model_prompt(&endpoint, &model, &prompt).await?;
                println!("{}", content);
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Barrier};
    use std::sync::{Mutex, OnceLock};
    use std::thread;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ufo-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_op(id: &str, title: &str) -> Operation {
        Operation {
            id: id.to_string(),
            title: title.to_string(),
            pilot_cmd: "true".to_string(),
            status: "queued".to_string(),
            created_at: Utc::now(),
            finished_at: None,
        }
    }

    fn start_fixture_server<F>(handler: F) -> (String, thread::JoinHandle<()>)
    where
        F: FnOnce(String, Vec<(String, String)>, Vec<u8>) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            let header_end;
            let content_length;
            loop {
                let n = stream.read(&mut tmp).unwrap();
                assert!(n > 0, "request ended before headers");
                buf.extend_from_slice(tmp.get(..n).unwrap());
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = pos + 4;
                    let headers = String::from_utf8(buf[..header_end].to_vec()).unwrap();
                    content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                Some(value.trim().parse::<usize>().unwrap())
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if buf.len() >= header_end + content_length {
                        break;
                    }
                    while buf.len() < header_end + content_length {
                        let n = stream.read(&mut tmp).unwrap();
                        assert!(n > 0, "request ended before body");
                        buf.extend_from_slice(tmp.get(..n).unwrap());
                    }
                    break;
                }
            }

            let header_text = String::from_utf8(buf[..header_end].to_vec()).unwrap();
            let mut lines = header_text.lines();
            let request_line = lines.next().unwrap().to_string();
            let headers = lines
                .filter_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    Some((name.trim().to_string(), value.trim().to_string()))
                })
                .collect::<Vec<_>>();
            let body = buf[header_end..header_end + content_length].to_vec();
            let (status, response_body) = handler(request_line, headers, body);
            let response = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        (endpoint, handle)
    }

    #[test]
    fn legacy_rfc3339_operation_timestamp_parses() {
        let op: Operation = serde_json::from_str(
            r#"{"id":"op-1","title":"legacy","pilot_cmd":"true","status":"queued","created_at":"2024-01-02T03:04:05Z","finished_at":null}"#,
        )
        .unwrap();
        assert_eq!(op.created_at.to_rfc3339(), "2024-01-02T03:04:05+00:00");
    }

    #[test]
    fn local_model_endpoint_rejects_untrusted_targets() {
        for bad in [
            "https://127.0.0.1:8080/v1",
            "http://8.8.8.8:8080/v1",
            "http://169.254.169.254:8080/v1",
            "http://0.0.0.0:8080/v1",
            "http://[::]:8080/v1",
            "http://localhost:8080/v1",
            "http://user@127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/v1?x=1",
            "http://127.0.0.1:8080/v1#frag",
            "http://127.0.0.1/v1",
        ] {
            assert!(
                validate_local_model_endpoint(bad).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn local_model_one_prompt() {
        let _guard = env_guard();
        let (endpoint, handle) = start_fixture_server(|request_line, headers, body| {
            assert_eq!(request_line, "POST /v1/chat/completions HTTP/1.1");
            assert!(headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("content-type")
                    && value == "application/json"));
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["model"], "fixture-model");
            assert_eq!(json["temperature"], 0.0);
            assert_eq!(json["stream"], false);
            assert_eq!(json["messages"][0]["role"], "user");
            assert_eq!(json["messages"][0]["content"], "ping");
            (
                200,
                serde_json::json!({
                    "choices": [
                        {"message": {"role": "assistant", "content": "pong"}}
                    ]
                })
                .to_string(),
            )
        });

        let saved = [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ]
        .into_iter()
        .map(|key| (key, env::var_os(key)))
        .collect::<Vec<_>>();
        for key in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            env::set_var(key, "http://127.0.0.1:9");
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let content = rt
            .block_on(post_local_model_prompt(&endpoint, "fixture-model", "ping"))
            .unwrap();
        assert_eq!(content, "pong");

        for (key, value) in saved {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }

        handle.join().unwrap();
    }

    #[test]
    fn safe_path_component_rejects_traversal() {
        assert!(is_safe_path_component("issue-123"));
        assert!(!is_safe_path_component("../issue"));
        assert!(!is_safe_path_component("a/b"));
        assert!(!is_safe_path_component(".."));
    }

    #[test]
    fn mailbox_append_is_atomic_under_concurrency() {
        let dir = temp_dir();
        let path = dir.join("mailbox.jsonl");
        let barrier = Arc::new(Barrier::new(2));
        let op_a = sample_op("a", "alpha");
        let op_b = sample_op("b", "bravo");

        let t1_path = path.clone();
        let t1_barrier = barrier.clone();
        let t1_op = op_a.clone();
        let t1 = thread::spawn(move || {
            t1_barrier.wait();
            append_op_to(&t1_path, &t1_op)
        });

        let t2_path = path.clone();
        let t2_barrier = barrier.clone();
        let t2_op = op_b.clone();
        let t2 = thread::spawn(move || {
            t2_barrier.wait();
            append_op_to(&t2_path, &t2_op)
        });

        t1.join().unwrap().unwrap();
        t2.join().unwrap().unwrap();

        let ops = load_mailbox_from(&path).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().any(|op| op.id == "a"));
        assert!(ops.iter().any(|op| op.id == "b"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn claim_next_is_exactly_once_and_finalize_preserves_appends() {
        let dir = temp_dir();
        let path = dir.join("mailbox.jsonl");
        let first = sample_op("first", "alpha");
        let second = sample_op("second", "bravo");
        let ops = vec![first.clone(), second.clone()];
        write_mailbox_locked(&path, &ops).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let left_path = path.clone();
        let left_barrier = barrier.clone();
        let left = thread::spawn(move || {
            left_barrier.wait();
            claim_next_mailbox_op(&left_path).unwrap()
        });

        let right_path = path.clone();
        let right_barrier = barrier.clone();
        let right = thread::spawn(move || {
            right_barrier.wait();
            claim_next_mailbox_op(&right_path).unwrap()
        });

        let claimed = [left.join().unwrap(), right.join().unwrap()];
        let claimed_ids: Vec<_> = claimed
            .into_iter()
            .filter_map(|op| op.map(|op| op.id))
            .collect();
        assert_eq!(claimed_ids.len(), 2);
        assert!(claimed_ids.contains(&"first".to_string()));
        assert!(claimed_ids.contains(&"second".to_string()));

        let queued = sample_op("third", "charlie");
        append_op_to(&path, &queued).unwrap();
        let status = claim_next_mailbox_op(&path).unwrap().expect("claim exists");
        assert_eq!(status.status, "running");
        append_op_to(&path, &sample_op("fourth", "delta")).unwrap();
        assert!(finalize_mailbox_op(&path, &status.id, "done", Some(Utc::now())).unwrap());

        let ops = load_mailbox_from(&path).unwrap();
        assert!(ops.iter().any(|op| op.id == "first"));
        assert!(ops.iter().any(|op| op.id == "second"));
        assert!(ops.iter().any(|op| op.id == "third"));
        assert!(ops.iter().any(|op| op.id == "fourth"));
        assert_eq!(
            ops.iter().find(|op| op.id == status.id).unwrap().status,
            "done"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_components_are_rejected() {
        let dir = temp_dir();
        let real = dir.join("real");
        let link = dir.join("link");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let unsafe_path = link.join("mailbox.jsonl");
        assert!(ensure_no_symlink_components(&unsafe_path).is_err());
        assert!(ensure_no_symlink_components(&real.join("mailbox.jsonl")).is_ok());

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_are_set_on_dir_and_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let private_dir = dir.join("private");
        ensure_private_dir(&private_dir).unwrap();
        let mode = fs::metadata(&private_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let file = dir.join("secret.txt");
        atomic_write_string(&file, "secret").unwrap();
        let file_mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_rewrite_creates_complete_file() {
        let dir = temp_dir();
        let path = dir.join("mailbox.jsonl");
        let ops = vec![sample_op("x", "xray"), sample_op("y", "yankee")];

        write_mailbox_locked(&path, &ops).unwrap();
        let loaded = load_mailbox_from(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "x");
        assert_eq!(loaded[1].id, "y");

        let _ = fs::remove_dir_all(dir);
    }

    fn fake_opencode(log: &Path, exit_code: i32) -> PathBuf {
        let bin_dir = log.parent().unwrap().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join("opencode");
        let script_body = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> '{}'\nexit {}\n",
            log.display(),
            exit_code
        );
        fs::write(&script, script_body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        script
    }

    struct PathGuard(String);

    impl Drop for PathGuard {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }

    fn with_fake_opencode(log: &Path, exit_code: i32) -> PathGuard {
        let script = fake_opencode(log, exit_code);
        let old_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", script.parent().unwrap().display(), old_path);
        std::env::set_var("PATH", &new_path);
        PathGuard(old_path)
    }

    #[test]
    fn auth_provider_allowlist_rejects_unknown() {
        assert!(Cli::try_parse_from(["ufo", "auth", "login", "--provider", "openai"]).is_ok());
        assert!(Cli::try_parse_from(["ufo", "auth", "login", "--provider", "api"]).is_err());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn opencode_auth_login_uses_exact_argv_and_verifies_snapshot() {
        let _guard = env_guard();
        let dir = temp_dir();
        let log = dir.join("argv.log");
        let _path_guard = with_fake_opencode(&log, 0);
        let process = OpencodeAuthProcess {
            program: dir.join("bin").join("opencode"),
        };
        std::env::set_var(
            "OPENCODE_AUTH_CONTENT",
            r#"{
                "openai": {
                    "type": "oauth",
                    "refresh": "refresh-secret",
                    "access": "access-secret",
                    "expires": 9999999999,
                    "accountId": "acct-1",
                    "enterpriseUrl": "https://example.invalid"
                }
            }"#,
        );

        process.login(OAuthProviderId::OpenAI).await.unwrap();
        let snapshot = auth::load_auth().unwrap();
        let summary = verify_login_summary(&snapshot, OAuthProviderId::OpenAI).unwrap();
        let rendered = format_provider_summary(&summary);
        assert!(rendered.contains("openai"));
        assert!(rendered.contains("supported"));
        assert!(rendered.contains("valid"));
        assert!(!rendered.contains("refresh-secret"));
        assert!(!rendered.contains("access-secret"));

        let argv = fs::read_to_string(&log).unwrap();
        assert_eq!(argv.trim(), "auth login --pure --provider openai");
        std::env::remove_var("OPENCODE_AUTH_CONTENT");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn opencode_auth_logout_propagates_nonzero_exit() {
        let _guard = env_guard();
        let dir = temp_dir();
        let log = dir.join("argv.log");
        let _path_guard = with_fake_opencode(&log, 37);
        let process = OpencodeAuthProcess {
            program: dir.join("bin").join("opencode"),
        };

        let err = process
            .logout(OAuthProviderId::GithubCopilot)
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("37") || message.contains("exited"));

        let argv = fs::read_to_string(&log).unwrap();
        assert_eq!(argv.trim(), "auth logout --provider github-copilot");
    }
}
