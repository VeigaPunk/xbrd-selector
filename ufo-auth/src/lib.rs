use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;

pub const OAUTH_PROVIDER_ALLOWLIST: [&str; 2] = ["openai", "github-copilot"];

pub fn oauth_provider_allowed(provider_id: &str) -> bool {
    OAUTH_PROVIDER_ALLOWLIST.contains(&provider_id)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthStore {
    providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthSnapshot {
    pub source: AuthSource,
    pub resolved_path: Option<PathBuf>,
    pub malformed_entries: usize,
    pub store: AuthStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthSource {
    Env,
    File,
    Missing,
}

impl fmt::Display for AuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Missing => "missing",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSummary {
    pub provider_id: String,
    pub kind: ProviderKind,
    pub source: AuthSource,
    pub policy: ProviderPolicy,
    pub oauth: Option<OauthSummary>,
    pub metadata_present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OauthSummary {
    pub expiry_state: ExpiryState,
    pub account_id: Option<String>,
    pub enterprise_url: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OauthAccessTokenRequest {
    provider_id: String,
    access_token: SecretString,
    expires_at: u64,
}

impl OauthAccessTokenRequest {
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at < current_unix_secs()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Oauth,
    Api,
    Wellknown,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Oauth => "oauth",
            Self::Api => "api",
            Self::Wellknown => "wellknown",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderPolicy {
    Usable,
    UnsupportedCredential,
    LocalModelConfig,
}

impl fmt::Display for ProviderPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Usable => "supported",
            Self::UnsupportedCredential => "ignored",
            Self::LocalModelConfig => "local-model",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpiryState {
    Valid,
    Expired,
}

impl fmt::Display for ExpiryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Valid => "valid",
            Self::Expired => "expired",
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ProviderEntry {
    Oauth(OauthAuth),
    Api(ApiAuth),
    Wellknown(WellknownAuth),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct OauthAuth {
    refresh: SecretString,
    access: SecretString,
    expires: u64,
    #[serde(rename = "accountId", default)]
    account_id: Option<String>,
    #[serde(rename = "enterpriseUrl", default)]
    enterprise_url: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiAuth {
    key: SecretString,
    #[serde(default)]
    metadata: Option<Value>,
}

impl fmt::Debug for ApiAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api")
            .field("key", &self.key)
            .field("metadata_present", &self.metadata.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct WellknownAuth {
    key: SecretString,
    token: SecretString,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum RawProviderEntry {
    Oauth(OauthAuth),
    Api(ApiAuth),
    Wellknown(WellknownAuth),
}

impl AuthStore {
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn summaries(&self, source: AuthSource) -> Vec<ProviderSummary> {
        self.providers
            .iter()
            .map(|(provider_id, entry)| entry.summary(provider_id, source.clone()))
            .collect()
    }

    pub fn usable_count(&self) -> usize {
        self.providers
            .iter()
            .filter(|(provider_id, entry)| {
                matches!(entry.policy(provider_id), ProviderPolicy::Usable)
            })
            .count()
    }

    pub fn oauth_provider_ids(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter_map(|(provider_id, entry)| {
                if entry.is_oauth() {
                    Some(provider_id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn oauth_summary(&self, provider_id: &str, source: AuthSource) -> Option<ProviderSummary> {
        self.providers.get(provider_id).and_then(|entry| {
            if entry.is_oauth() {
                Some(entry.summary(provider_id, source))
            } else {
                None
            }
        })
    }

    pub fn oauth_access_token_request(&self, provider_id: &str) -> Option<OauthAccessTokenRequest> {
        if !oauth_provider_allowed(provider_id) {
            return None;
        }
        match self.providers.get(provider_id) {
            Some(ProviderEntry::Oauth(auth)) if auth.expires >= current_unix_secs() => {
                Some(OauthAccessTokenRequest {
                    provider_id: provider_id.to_string(),
                    access_token: auth.access.clone(),
                    expires_at: auth.expires,
                })
            }
            _ => None,
        }
    }
}

impl ProviderEntry {
    fn is_oauth(&self) -> bool {
        matches!(self, Self::Oauth(_))
    }

    fn summary(&self, provider_id: &str, source: AuthSource) -> ProviderSummary {
        let provider_id = sanitize_terminal(provider_id);
        let policy = self.policy(&provider_id);
        match self {
            Self::Oauth(auth) => ProviderSummary {
                provider_id,
                kind: ProviderKind::Oauth,
                source,
                policy,
                oauth: Some(OauthSummary {
                    expiry_state: if auth.expires >= current_unix_secs() {
                        ExpiryState::Valid
                    } else {
                        ExpiryState::Expired
                    },
                    account_id: auth.account_id.clone(),
                    enterprise_url: auth.enterprise_url.clone(),
                }),
                metadata_present: false,
            },
            Self::Api(auth) => ProviderSummary {
                provider_id,
                kind: ProviderKind::Api,
                source,
                policy,
                oauth: None,
                metadata_present: auth.metadata.is_some(),
            },
            Self::Wellknown(_) => ProviderSummary {
                provider_id,
                kind: ProviderKind::Wellknown,
                source,
                policy,
                oauth: None,
                metadata_present: false,
            },
        }
    }

    fn policy(&self, provider_id: &str) -> ProviderPolicy {
        match self {
            Self::Oauth(_) if oauth_provider_allowed(provider_id) => ProviderPolicy::Usable,
            Self::Oauth(_) | Self::Api(_) | Self::Wellknown(_) => {
                ProviderPolicy::UnsupportedCredential
            }
        }
    }
}

impl fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oauth(auth) => f
                .debug_struct("Oauth")
                .field("refresh", &auth.refresh)
                .field("access", &auth.access)
                .field("expires", &auth.expires)
                .field("account_id", &auth.account_id)
                .field("enterprise_url", &auth.enterprise_url)
                .finish(),
            Self::Api(auth) => f
                .debug_struct("Api")
                .field("key", &auth.key)
                .field("metadata_present", &auth.metadata.is_some())
                .finish(),
            Self::Wellknown(auth) => f
                .debug_struct("Wellknown")
                .field("key", &auth.key)
                .field("token", &auth.token)
                .finish(),
        }
    }
}

pub fn load_auth() -> Result<AuthSnapshot> {
    if let Some(content) = env::var_os("OPENCODE_AUTH_CONTENT") {
        let content = content.to_string_lossy();
        if !content.trim().is_empty() {
            if let Ok(snapshot) = parse_auth_content(&content, AuthSource::Env, None) {
                return Ok(snapshot);
            }
        }
    }

    for path in auth_file_candidates() {
        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("read OpenCode auth file: {}", path.display()))?;
            let snapshot = parse_auth_content(&content, AuthSource::File, Some(path.clone()))?;
            return Ok(snapshot);
        }
    }

    Ok(AuthSnapshot {
        source: AuthSource::Missing,
        resolved_path: None,
        malformed_entries: 0,
        store: AuthStore::default(),
    })
}

pub fn parse_auth_content(
    content: &str,
    source: AuthSource,
    resolved_path: Option<PathBuf>,
) -> Result<AuthSnapshot> {
    let value: Value = serde_json::from_str(content).context("parse OpenCode auth JSON")?;
    let (providers, malformed_entries) = parse_provider_map(value)?;
    let store = AuthStore { providers };
    Ok(AuthSnapshot {
        source,
        resolved_path,
        malformed_entries,
        store,
    })
}

fn parse_provider_map(value: Value) -> Result<(BTreeMap<String, ProviderEntry>, usize)> {
    let Some(object) = value.as_object() else {
        bail!("OpenCode auth JSON must be an object");
    };

    let entries = if let Some(wrapper) = object.get("providers") {
        let Some(wrapper_object) = wrapper.as_object() else {
            bail!("legacy UFO wrapper `providers` must be an object");
        };
        // `providers` is the legacy wrapper; ignore sibling metadata fields
        // rather than treating them as malformed provider entries.
        wrapper_object.clone()
    } else {
        object.clone()
    };

    let mut providers = BTreeMap::new();
    let mut malformed_entries = 0;
    for (provider_id, entry_value) in entries {
        match parse_provider_entry(entry_value) {
            Ok(entry) => {
                providers.insert(provider_id, entry);
            }
            Err(_) => {
                malformed_entries += 1;
            }
        }
    }
    Ok((providers, malformed_entries))
}

pub fn sanitize_terminal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => {
                if matches!(chars.peek(), Some('[')) {
                    let _ = chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() => {}
            other => out.push(other),
        }
    }
    out
}

pub fn auth_file_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        out.push(PathBuf::from(xdg).join("opencode/auth.json"));
    } else if let Some(home) = dirs::home_dir() {
        out.push(home.join(".local/share/opencode/auth.json"));
    }
    if let Some(home) = dirs::home_dir() {
        let legacy = home.join(".local/share/opencode/auth.json");
        if out.first().map(|path| path != &legacy).unwrap_or(true) {
            out.push(legacy);
        }
    }
    out
}

fn parse_provider_entry(value: Value) -> Result<ProviderEntry> {
    let value = if let Value::Object(mut object) = value {
        if object.get("type").is_none() {
            if let Some(kind) = object.remove("kind") {
                object.insert("type".to_string(), kind);
            }
        }
        Value::Object(object)
    } else {
        value
    };
    let raw: RawProviderEntry = serde_json::from_value(value).context("parse auth entry")?;
    Ok(match raw {
        RawProviderEntry::Oauth(auth) => ProviderEntry::Oauth(auth),
        RawProviderEntry::Api(auth) => ProviderEntry::Api(auth),
        RawProviderEntry::Wellknown(auth) => ProviderEntry::Wellknown(auth),
    })
}

fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ufo-auth-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    #[test]
    fn parses_exact_schema() {
        let _guard = guard();
        let snapshot = parse_auth_content(
            r#"{
                "openai": {
                  "type": "oauth",
                  "refresh": "r",
                  "access": "a",
                  "expires": 9999999999,
                  "accountId": "acct",
                  "enterpriseUrl": "https://example.invalid"
                },
                "provider-api": {
                  "type": "api",
                  "key": "k",
                  "metadata": {"label": "demo"}
                },
                "provider-wellknown": {
                  "kind": "wellknown",
                  "key": "k2",
                  "token": "t"
                }
            }"#,
            AuthSource::File,
            Some(PathBuf::from("/tmp/auth.json")),
        )
        .unwrap();

        assert_eq!(snapshot.store.len(), 3);
        let items = snapshot.store.summaries(snapshot.source.clone());
        let oauth = items
            .iter()
            .find(|item| item.kind == ProviderKind::Oauth)
            .unwrap();
        assert_eq!(oauth.policy, ProviderPolicy::Usable);
        assert_eq!(
            oauth.oauth.as_ref().unwrap().account_id.as_deref(),
            Some("acct")
        );
        let api = items
            .iter()
            .find(|item| item.kind == ProviderKind::Api)
            .unwrap();
        assert_eq!(api.policy, ProviderPolicy::UnsupportedCredential);
        assert!(api.metadata_present);
        let wellknown = items
            .iter()
            .find(|item| item.kind == ProviderKind::Wellknown)
            .unwrap();
        assert_eq!(wellknown.policy, ProviderPolicy::UnsupportedCredential);
    }

    #[test]
    fn wrapper_ignores_sibling_metadata() {
        let snapshot = parse_auth_content(
            r#"{
                "providers": {
                  "openai": {"type":"api","key":"secret"}
                },
                "version": 1
            }"#,
            AuthSource::Env,
            None,
        )
        .unwrap();
        assert_eq!(snapshot.store.len(), 1);
        assert_eq!(snapshot.malformed_entries, 0);
    }

    #[test]
    fn oauth_access_token_request() {
        let _guard = guard();
        let snapshot = parse_auth_content(
            r#"{
                "openai": {"type":"oauth","refresh":"r","access":"a","expires":9999999999},
                "github-copilot": {"type":"oauth","refresh":"r2","access":"b","expires":1},
                "custom": {"type":"oauth","refresh":"r3","access":"c","expires":9999999999}
            }"#,
            AuthSource::File,
            None,
        )
        .unwrap();

        let request = snapshot.store.oauth_access_token_request("openai").unwrap();
        assert_eq!(request.provider_id(), "openai");
        assert_eq!(request.expires_at(), 9_999_999_999);
        assert!(!request.is_expired());

        assert!(snapshot
            .store
            .oauth_access_token_request("github-copilot")
            .is_none());
        assert!(snapshot
            .store
            .oauth_access_token_request("custom")
            .is_none());
        assert!(snapshot.store.oauth_access_token_request("api").is_none());
    }

    #[test]
    fn xdg_and_env_precedence() {
        let _guard = guard();
        let home = temp_dir("home");
        let xdg = temp_dir("xdg");
        let xdg_auth = xdg.join("opencode/auth.json");
        let legacy_auth = home.join(".local/share/opencode/auth.json");
        fs::create_dir_all(xdg_auth.parent().unwrap()).unwrap();
        fs::create_dir_all(legacy_auth.parent().unwrap()).unwrap();
        fs::write(
            &xdg_auth,
            r#"{"from-xdg":{"type":"oauth","refresh":"r","access":"a","expires":9999999999}}"#,
        )
        .unwrap();
        fs::write(
            &legacy_auth,
            r#"{"from-legacy":{"type":"oauth","refresh":"r","access":"a","expires":9999999999}}"#,
        )
        .unwrap();

        set_env("HOME", Some(home.to_str().unwrap()));
        set_env("XDG_DATA_HOME", Some(xdg.to_str().unwrap()));
        set_env(
            "OPENCODE_AUTH_CONTENT",
            Some(
                r#"{"from-env":{"type":"oauth","refresh":"r","access":"a","expires":9999999999}}"#,
            ),
        );

        let snapshot = load_auth().unwrap();
        assert_eq!(snapshot.source, AuthSource::Env);
        assert_eq!(snapshot.store.len(), 1);
        assert_eq!(
            snapshot.store.summaries(snapshot.source.clone())[0].provider_id,
            "from-env"
        );

        set_env("OPENCODE_AUTH_CONTENT", None);
        let snapshot = load_auth().unwrap();
        assert_eq!(snapshot.source, AuthSource::File);
        assert_eq!(
            snapshot.store.summaries(snapshot.source.clone())[0].provider_id,
            "from-xdg"
        );

        set_env("XDG_DATA_HOME", None);
        let snapshot = load_auth().unwrap();
        assert_eq!(
            snapshot.store.summaries(snapshot.source.clone())[0].provider_id,
            "from-legacy"
        );

        set_env("HOME", None);
    }

    #[test]
    fn malformed_env_falls_back_to_file() {
        let _guard = guard();
        let home = temp_dir("fallback-home");
        let auth = home.join(".local/share/opencode/auth.json");
        fs::create_dir_all(auth.parent().unwrap()).unwrap();
        fs::write(
            &auth,
            r#"{"from-file":{"type":"oauth","refresh":"r","access":"a","expires":9999999999}}"#,
        )
        .unwrap();

        set_env("HOME", Some(home.to_str().unwrap()));
        set_env("XDG_DATA_HOME", None);
        set_env("OPENCODE_AUTH_CONTENT", Some("{"));

        let snapshot = load_auth().unwrap();
        assert_eq!(snapshot.source, AuthSource::File);
        assert_eq!(
            snapshot.store.summaries(snapshot.source.clone())[0].provider_id,
            "from-file"
        );

        set_env("OPENCODE_AUTH_CONTENT", None);
        set_env("HOME", None);
    }

    #[test]
    fn malformed_entries_are_isolated() {
        let _guard = guard();
        let snapshot = parse_auth_content(
            r#"{
                "good": {"type":"oauth","refresh":"r","access":"a","expires":9999999999},
                "bad": {"type":"oauth","refresh":"r","expires":9999999999}
            }"#,
            AuthSource::File,
            None,
        )
        .unwrap();

        assert_eq!(snapshot.store.len(), 1);
        assert_eq!(snapshot.malformed_entries, 1);
    }

    #[test]
    fn terminal_control_sanitization() {
        let _guard = guard();
        let snapshot = parse_auth_content(
            r#"{"\u001b[31mred\u001b[0m":{"type":"oauth","refresh":"r","access":"a","expires":9999999999}}"#,
            AuthSource::File,
            None,
        )
        .unwrap();

        let summary = &snapshot.store.summaries(snapshot.source.clone())[0];
        assert!(!summary.provider_id.contains('\u{1b}'));
        assert!(!summary.provider_id.contains("[31m"));
    }

    #[test]
    fn debug_redacts_secrets() {
        let _guard = guard();
        let snapshot = parse_auth_content(
            r#"{
                "secret": {"type":"wellknown","key":"top-key","token":"top-token"}
            }"#,
            AuthSource::File,
            None,
        )
        .unwrap();

        let debug = format!("{:?}", snapshot.store);
        assert!(!debug.contains("top-key"));
        assert!(!debug.contains("top-token"));
        assert!(debug.contains("[redacted]"));
    }
}
