//! Suggested cloud model catalog (policy lens): OpenAI ChatGPT + xAI Grok only.
//! Local loopback models stay in `opencode_local_models` and are never merged here.
//!
//! Catalog is fixture-driven for offline tests and TUI display. Live OpenCode
//! discovery may intersect later; remote cloud `model prompt` is out of scope.

use crate::sanitize_terminal;

/// Cloud tier provider ids that appear in the curated catalog.
pub const CLOUD_PROVIDER_OPENAI: &str = "openai";
pub const CLOUD_PROVIDER_XAI: &str = "xai";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudModelEntry {
    pub provider_id: String,
    pub model_id: String,
    pub origin: &'static str,
}

/// Curated operator defaults — not the live provider inventory.
pub const CLOUD_MODEL_CATALOG: &[(&str, &str)] = &[
    // OpenAI / ChatGPT
    (CLOUD_PROVIDER_OPENAI, "gpt-5.4"),
    (CLOUD_PROVIDER_OPENAI, "gpt-5.4-mini"),
    (CLOUD_PROVIDER_OPENAI, "gpt-5.5"),
    (CLOUD_PROVIDER_OPENAI, "o4-mini"),
    // xAI / Grok
    (CLOUD_PROVIDER_XAI, "grok-4.5"),
    (CLOUD_PROVIDER_XAI, "grok-4.3"),
    (CLOUD_PROVIDER_XAI, "grok-4"),
    (CLOUD_PROVIDER_XAI, "grok-3"),
    (CLOUD_PROVIDER_XAI, "grok-3-mini"),
];

/// Model id prefixes that must never appear on the cloud catalog surface.
const DENIED_MODEL_PREFIXES: &[&str] = &["claude", "sonnet", "opus", "haiku", "anthropic"];

pub fn cloud_model_denied(model_id: &str) -> bool {
    let lower = model_id.to_ascii_lowercase();
    DENIED_MODEL_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix) || lower.contains(prefix))
}

pub fn cloud_model_entries() -> Vec<CloudModelEntry> {
    CLOUD_MODEL_CATALOG
        .iter()
        .filter(|(_, model_id)| !cloud_model_denied(model_id))
        .map(|(provider_id, model_id)| CloudModelEntry {
            provider_id: sanitize_terminal(provider_id),
            model_id: sanitize_terminal(model_id),
            origin: "catalog",
        })
        .collect()
}

pub fn format_cloud_model_entry(entry: &CloudModelEntry) -> String {
    format!(
        "tier=cloud  provider={}  model={}  source={}",
        sanitize_terminal(&entry.provider_id),
        sanitize_terminal(&entry.model_id),
        entry.origin
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_openai_and_xai_only() {
        let entries = cloud_model_entries();
        assert!(!entries.is_empty());
        for entry in &entries {
            assert!(
                entry.provider_id == CLOUD_PROVIDER_OPENAI
                    || entry.provider_id == CLOUD_PROVIDER_XAI,
                "unexpected provider {}",
                entry.provider_id
            );
            assert!(!cloud_model_denied(&entry.model_id));
            assert!(CLOUD_MODEL_CATALOG
                .iter()
                .any(|(p, m)| *p == entry.provider_id && *m == entry.model_id));
        }
    }

    #[test]
    fn denies_claude_family_ids() {
        assert!(cloud_model_denied("claude-3-5-sonnet"));
        assert!(cloud_model_denied("sonnet"));
        assert!(cloud_model_denied("opus-4"));
        assert!(!cloud_model_denied("grok-4.5"));
        assert!(!cloud_model_denied("gpt-5.4"));
    }

    #[test]
    fn format_has_no_secrets() {
        let entry = CloudModelEntry {
            provider_id: "openai".into(),
            model_id: "gpt-5.4".into(),
            origin: "catalog",
        };
        let line = format_cloud_model_entry(&entry);
        assert!(line.contains("tier=cloud"));
        assert!(line.contains("openai"));
        assert!(!line.contains("sk-"));
    }
}
