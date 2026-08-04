//! ufo-cli-beads — pure Rust rover using beads (bd) as mailbox substrate.
//! Pilot commands intentionally use POSIX `sh`; the implementation stays Rust-only.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use ufo_auth as auth;
use uuid::Uuid;

const APP_DIR: &str = ".ufo";
const ROVERS_FILE: &str = "rovers.json";

#[derive(Parser, Debug)]
#[command(name = "ufo", about = "UFO rover (beads mailbox substrate)")]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Enroll {
        #[arg(long, default_value = "local-beads")]
        name: String,
        #[arg(long, default_value_t = 1)]
        units: u32,
        #[arg(long)]
        tags: Vec<String>,
    },
    Start {
        #[arg(long, default_value_t = 3)]
        poll_secs: u64,
        /// Working directory that already has `bd init`
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Create a beads task that the rover will pick up
    Push {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "echo 'hello from beads pilot'")]
        pilot_cmd: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    Mailbox {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Auth (cloned from OpenCode auth connection)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    List,
    Status,
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

#[derive(Debug, Deserialize)]
struct BdIssue {
    id: String,
    title: String,
    status: String,
    #[serde(default)]
    description: Option<String>,
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
                let value = i64::try_from(value)
                    .map_err(|_| E::custom("invalid unix timestamp"))?;
                Self::visit_i64(self, value)
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

fn atomic_write_string(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("missing parent directory")?;
    fs::create_dir_all(parent)?;
    set_private_mode(parent, true)?;

    let tmp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(OsStr::to_str).unwrap_or("ufo"),
        Uuid::new_v4()
    ));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
    }
    set_private_mode(&tmp_path, false)?;
    fs::rename(&tmp_path, path)?;
    let dir = OpenOptions::new().read(true).open(parent)?;
    dir.sync_all()?;
    set_private_mode(path, false)?;
    Ok(())
}

fn app_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home")?;
    let d = home.join(APP_DIR);
    ensure_no_symlink_components(&d)?;
    fs::create_dir_all(&d)?;
    set_private_mode(&d, true)?;
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

fn load_rovers() -> Result<Vec<RoverEntry>> {
    let p = app_dir()?.join(ROVERS_FILE);
    if !p.exists() {
        return Ok(vec![]);
    }
    Ok(serde_json::from_str(&fs::read_to_string(p)?)?)
}

fn save_rovers(r: &[RoverEntry]) -> Result<()> {
    atomic_write_string(
        &app_dir()?.join(ROVERS_FILE),
        &serde_json::to_string_pretty(r)?,
    )
}

async fn bd_output<I, S>(project: &Path, args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    ensure_no_symlink_components(project)?;
    let out = Command::new("bd")
        .args(args)
        .arg("--json")
        .current_dir(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("bd not found on PATH — install beads first")?;
    Ok(out)
}

async fn bd_json<I, S>(project: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = bd_output(project, args).await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("bd failed: {err}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn bd_run<I, S>(project: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = bd_output(project, args).await?;
    if !out.status.success() {
        bail!("bd failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

async fn close_issue(project: &Path, id: &str) -> Result<()> {
    bd_run(project, ["close", id]).await
}

async fn list_ready(project: &Path) -> Result<Vec<BdIssue>> {
    let raw = bd_json(project, ["ready", "--claim"]).await?;
    serde_json::from_str(&raw).context("parse bd ready JSON")
}

fn is_safe_issue_id(id: &str) -> bool {
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

async fn claim_and_run(project: &Path, issue: &BdIssue, work_root: &Path) -> Result<()> {
    if !is_safe_issue_id(&issue.id) {
        bail!("unsafe issue id: {}", issue.id);
    }

    let op_dir = work_root.join(&issue.id);
    ensure_no_symlink_components(&op_dir)?;
    fs::create_dir_all(&op_dir)?;
    set_private_mode(&op_dir, true)?;
    println!(
        "[ufo-beads] claimed {} — {} [{}]",
        issue.id, issue.title, issue.status
    );

    let pilot = issue
        .description
        .as_deref()
        .and_then(|d| {
            d.lines()
                .find(|l| l.starts_with("pilot:"))
                .map(|l| l.trim_start_matches("pilot:").trim().to_string())
        })
        .unwrap_or_else(|| format!("echo 'executing beads issue {}'", issue.id));

    let status = Command::new("sh")
        .arg("-c")
        .arg(&pilot)
        .current_dir(&op_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;

    match status {
        Ok(status) if status.success() => {
            close_issue(project, &issue.id).await?;
            println!("[ufo-beads] closed {}", issue.id);
            Ok(())
        }
        Ok(status) => {
            let primary =
                anyhow::anyhow!("pilot failed for {}: exit {:?}", issue.id, status.code());
            best_effort_failure_markers(project, issue, &primary).await;
            Err(primary)
        }
        Err(err) => {
            let primary =
                anyhow::Error::new(err).context(format!("pilot spawn failed for {}", issue.id));
            best_effort_failure_markers(project, issue, &primary).await;
            Err(primary)
        }
    }
}

async fn comment_issue(project: &Path, id: &str, text: &str) -> Result<()> {
    bd_run(project, ["comment", id, text]).await
}

async fn reopen_issue(project: &Path, id: &str) -> Result<()> {
    bd_run(project, ["reopen", id]).await
}

async fn unassign_issue(project: &Path, id: &str) -> Result<()> {
    bd_run(project, ["assign", id, ""]).await
}

async fn best_effort_failure_markers(project: &Path, issue: &BdIssue, primary: &anyhow::Error) {
    let note = format!("pilot failed for {}: {primary:#}", issue.id);
    if let Err(err) = comment_issue(project, &issue.id, &note).await {
        eprintln!("[ufo-beads] failure comment: {err:#}");
    }
    if let Err(err) = reopen_issue(project, &issue.id).await {
        eprintln!("[ufo-beads] failure reopen: {err:#}");
    }
    if let Err(err) = unassign_issue(project, &issue.id).await {
        eprintln!("[ufo-beads] failure unassign: {err:#}");
    }
}

async fn rover_loop(project: PathBuf, poll_secs: u64) -> Result<()> {
    let work_root = app_dir()?.join("work-beads");
    ensure_no_symlink_components(&work_root)?;
    fs::create_dir_all(&work_root)?;
    set_private_mode(&work_root, true)?;
    println!(
        "[ufo-beads] loop on project={:?} poll={}s",
        project, poll_secs
    );

    loop {
        match poll_once(&project, &work_root).await {
            Ok(true) => {}
            Ok(false) => sleep(Duration::from_secs(poll_secs)).await,
            Err(e) => {
                eprintln!("[ufo-beads] mailbox poll: {e:#}");
                sleep(Duration::from_secs(poll_secs)).await;
            }
        }
    }
}

async fn poll_once(project: &Path, work_root: &Path) -> Result<bool> {
    match list_ready(project).await {
        Ok(issues) => {
            if let Some(issue) = issues.into_iter().next() {
                claim_and_run(project, &issue, work_root).await?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        Err(e) => Err(e),
    }
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
            println!("[ufo-beads] enrolled {}", entry.id);
            rovers.push(entry);
            save_rovers(&rovers)?;
        }
        Commands::Start { poll_secs, project } => {
            if load_rovers()?.is_empty() {
                bail!("enroll first");
            }
            rover_loop(project, poll_secs).await?;
        }
        Commands::Push {
            title,
            pilot_cmd,
            project,
        } => {
            let desc = format!("pilot: {pilot_cmd}");
            let out = Command::new("bd")
                .arg("create")
                .arg(&title)
                .arg("-t")
                .arg("task")
                .arg("-p")
                .arg("2")
                .arg("--description")
                .arg(&desc)
                .arg("--json")
                .current_dir(&project)
                .output()
                .await
                .context("bd create")?;
            if !out.status.success() {
                bail!("bd create failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            println!(
                "[ufo-beads] pushed via beads:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        Commands::Mailbox { project } => {
            let raw = bd_json(&project, ["list", "--status=open"]).await?;
            println!("{raw}");
        }
        Commands::Auth { action } => match action {
            AuthAction::List => {
                let snapshot = auth::load_auth()?;
                let list = snapshot.store.summaries(snapshot.source.clone());
                if list.is_empty() {
                    println!(
                        "[ufo-beads] no providers (OpenCode auth: env, XDG_DATA_HOME/opencode/auth.json, then ~/.local/share/opencode/auth.json)"
                    );
                } else {
                    for item in list {
                        let oauth = item
                            .oauth
                            .as_ref()
                            .map(|oauth| {
                                format!(
                                    " expires={} account={} enterprise={}",
                                    oauth.expiry_state,
                                    oauth.account_id.as_deref().unwrap_or("-"),
                                    oauth.enterprise_url.as_deref().unwrap_or("-")
                                )
                            })
                            .unwrap_or_default();
                        println!(
                            "{}  ({} source={} policy={} metadata={}){}",
                            item.provider_id,
                            item.kind,
                            item.source,
                            item.policy,
                            if item.metadata_present {
                                "present"
                            } else {
                                "-"
                            },
                            oauth
                        );
                    }
                    if snapshot.malformed_entries > 0 {
                        println!(
                            "[ufo-beads] skipped malformed entries: {}",
                            snapshot.malformed_entries
                        );
                    }
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use tokio::sync::{Mutex, MutexGuard};

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    async fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ufo-cli-beads-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn issue_id_validation_rejects_paths() {
        assert!(is_safe_issue_id("abc-123"));
        assert!(!is_safe_issue_id("../abc"));
        assert!(!is_safe_issue_id("a/b"));
        assert!(!is_safe_issue_id(".."));
    }

    #[test]
    fn legacy_rfc3339_rover_timestamp_parses() {
        let rover: RoverEntry = serde_json::from_str(
            r#"{"id":"rover-1","name":"legacy","units":1,"tags":[],"enrolled_at":"2024-01-02T03:04:05Z"}"#,
        )
        .unwrap();
        assert_eq!(rover.enrolled_at.to_rfc3339(), "2024-01-02T03:04:05+00:00");
    }

    #[test]
    fn unix_timestamp_overflow_is_rejected() {
        let result: std::result::Result<RoverEntry, _> = serde_json::from_str(
            r#"{"id":"rover-1","name":"bad","units":1,"tags":[],"enrolled_at":18446744073709551615}"#,
        );
        assert!(result.is_err());
    }

    fn fake_bd_dir(log: &Path, ready_json: &str, ready_exit: i32) -> PathBuf {
        let bin_dir = log.parent().unwrap().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let bd = bin_dir.join("bd");
        let script = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  ready)\n    cat <<'JSON'\n{}\nJSON\n    exit {}\n    ;;\n  list)\n    printf 'LIST_CALLED\\n' >> '{}'\n    exit 0\n    ;;\n  close|comment|reopen|assign)\n    exit 0\n    ;;\n  *)\n    exit 1\n    ;;\nesac\n",
            log.display(),
            ready_json,
            ready_exit,
            log.display()
        );
        fs::write(&bd, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bd).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bd, perms).unwrap();
        }
        bd
    }

    struct PathGuard(String);

    impl Drop for PathGuard {
        fn drop(&mut self) {
            std::env::set_var("PATH", &self.0);
        }
    }

    fn with_fake_bd(log: &Path, ready_json: &str, ready_exit: i32) -> PathGuard {
        let bd = fake_bd_dir(log, ready_json, ready_exit);
        let old_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", bd.parent().unwrap().display(), old_path);
        std::env::set_var("PATH", &new_path);
        PathGuard(old_path)
    }

    #[tokio::test]
    async fn ready_claim_pilot_close_is_the_command_order() {
        let _guard = test_guard().await;
        let dir = temp_dir();
        let log = dir.join("trace.log");
        let pilot_log = log.display().to_string();
        let ready_json = format!(
            r#"[{{"id":"issue-1","title":"alpha","status":"open","description":"pilot: printf pilot >> {}"}}]"#,
            pilot_log
        );
        let _path_guard = with_fake_bd(&log, &ready_json, 0);

        let work_root = dir.join("work");
        fs::create_dir_all(&work_root).unwrap();
        let result = poll_once(&dir, &work_root).await.unwrap();
        assert!(result);

        let trace = fs::read_to_string(&log).unwrap();
        assert!(trace.contains("ready --claim --json"));
        assert!(trace.contains("pilot"));
        assert!(trace.contains("close issue-1 --json"));
        assert!(!trace.contains("update issue-1"));
    }

    #[tokio::test]
    async fn empty_queue_is_a_no_op() {
        let _guard = test_guard().await;
        let dir = temp_dir();
        let log = dir.join("trace.log");
        let _path_guard = with_fake_bd(&log, "[]", 0);
        let work_root = dir.join("work");
        fs::create_dir_all(&work_root).unwrap();

        let result = poll_once(&dir, &work_root).await.unwrap();
        assert!(!result);

        let trace = fs::read_to_string(&log).unwrap();
        assert!(trace.contains("ready --claim --json"));
        assert!(!trace.contains("pilot"));
        assert!(!trace.contains("close"));
        assert!(!trace.contains("LIST_CALLED"));
    }

    #[tokio::test]
    async fn ready_failure_never_lists_open() {
        let _guard = test_guard().await;
        let dir = temp_dir();
        let log = dir.join("trace.log");
        let _path_guard = with_fake_bd(&log, "[]", 2);
        let work_root = dir.join("work");
        fs::create_dir_all(&work_root).unwrap();

        let err = poll_once(&dir, &work_root).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("bd failed") || msg.contains("ready"));

        let trace = fs::read_to_string(&log).unwrap();
        assert!(trace.contains("ready --claim --json"));
        assert!(!trace.contains("list --status=open"));
    }

    #[tokio::test]
    async fn malformed_ready_json_is_reported() {
        let _guard = test_guard().await;
        let dir = temp_dir();
        let log = dir.join("trace.log");
        let _path_guard = with_fake_bd(&log, "not-json", 0);
        let work_root = dir.join("work");
        fs::create_dir_all(&work_root).unwrap();

        let err = poll_once(&dir, &work_root).await.unwrap_err();
        assert!(format!("{err:#}").contains("parse bd ready JSON"));
    }

    #[tokio::test]
    async fn pilot_failure_does_not_close() {
        let _guard = test_guard().await;
        let dir = temp_dir();
        let log = dir.join("trace.log");
        let ready_json = format!(
            r#"[{{"id":"issue-2","title":"beta","status":"open","description":"pilot: sh -c 'echo pilot >> {} ; exit 7'"}}]"#,
            log.display()
        );
        let _path_guard = with_fake_bd(&log, &ready_json, 0);
        let work_root = dir.join("work");
        fs::create_dir_all(&work_root).unwrap();

        let err = poll_once(&dir, &work_root).await.unwrap_err();

        let trace = fs::read_to_string(&log).unwrap();
        assert!(trace.contains("ready --claim --json"));
        assert!(trace.contains("pilot"));
        assert!(trace.contains("comment issue-2"));
        assert!(trace.contains("reopen issue-2"));
        assert!(trace.contains("assign issue-2"));
        assert!(!trace.contains("close issue-2"));
        let msg = format!("{err:#}");
        assert!(msg.contains("pilot") || msg.contains("exit"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_components_are_rejected() {
        let dir = temp_dir();
        let real = dir.join("real");
        let link = dir.join("link");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(ensure_no_symlink_components(&link.join("mailbox.jsonl")).is_err());
        assert!(ensure_no_symlink_components(&real.join("mailbox.jsonl")).is_ok());

        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_are_set_on_dir_and_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("ufo-cli-beads-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        set_private_mode(&dir, true).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let file = dir.join("secret.txt");
        atomic_write_string(&file, "secret").unwrap();
        let file_mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);

        let _ = fs::remove_dir_all(dir);
    }
}
