use anyhow::{bail, Context, Result};
use reqwest::Url;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use crate::sanitize_terminal;

use crate::{validate_local_model_endpoint, validate_model_id, validate_provider_id};

const BUILTIN_LOCAL_PROVIDER_TEMPLATES: [(&str, &str); 3] = [
    ("ollama", "http://127.0.0.1:11434/v1"),
    ("lm-studio", "http://127.0.0.1:1234/v1"),
    ("llama.cpp", "http://127.0.0.1:8080/v1"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalModelSource {
    Env,
    File,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModelCatalog {
    pub source: LocalModelSource,
    pub resolved_path: Option<PathBuf>,
    pub malformed_entries: usize,
    providers: BTreeMap<String, LocalModelProvider>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModelProvider {
    provider_id: String,
    endpoint: Url,
    models: Vec<String>,
    origin: LocalModelOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalModelOrigin {
    Config,
    Builtin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModelEntry {
    pub provider_id: String,
    pub model_id: String,
    pub endpoint: String,
    pub origin: &'static str,
}

impl LocalModelCatalog {
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            source: LocalModelSource::Missing,
            resolved_path: None,
            malformed_entries: 0,
            providers: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn entries(&self) -> Vec<LocalModelEntry> {
        self.providers
            .values()
            .flat_map(|provider| provider.entries())
            .collect()
    }

    pub fn resolve_selection(&self, provider_spec: &str) -> Result<(Url, String)> {
        let (provider_id, model_id) = parse_provider_model_spec(provider_spec)?;
        if let Some(provider) = self.providers.get(provider_id) {
            if !provider.models.iter().any(|item| item == model_id) {
                bail!("unknown local model for provider")
            }
            return Ok((provider.endpoint.clone(), model_id.to_string()));
        }
        builtin_template(provider_id, model_id).context("unknown local model provider")
    }
}

impl LocalModelProvider {
    fn entries(&self) -> Vec<LocalModelEntry> {
        let origin = match self.origin {
            LocalModelOrigin::Config => "config",
            LocalModelOrigin::Builtin => "builtin",
        };
        self.models
            .iter()
            .map(|model_id| LocalModelEntry {
                provider_id: sanitize_terminal(&self.provider_id),
                model_id: sanitize_terminal(model_id),
                endpoint: sanitize_terminal(self.endpoint.as_str()),
                origin,
            })
            .collect()
    }
}

fn builtin_provider(provider_id: &str) -> Option<LocalModelProvider> {
    BUILTIN_LOCAL_PROVIDER_TEMPLATES
        .iter()
        .find(|(name, _)| *name == provider_id)
        .and_then(|(name, endpoint)| {
            validate_provider_id(name).ok()?;
            let endpoint = validate_local_model_endpoint(endpoint).ok()?;
            Some(LocalModelProvider {
                provider_id: (*name).to_string(),
                endpoint,
                models: vec![],
                origin: LocalModelOrigin::Builtin,
            })
        })
}

fn builtin_template_provider(provider_id: &str, model_id: &str) -> Option<LocalModelProvider> {
    let mut provider = builtin_provider(provider_id)?;
    validate_model_id(model_id).ok()?;
    provider.models.push(model_id.to_string());
    Some(provider)
}

pub fn load_local_model_catalog() -> Result<LocalModelCatalog> {
    let mut last_error: Option<anyhow::Error> = None;
    if let Some(content) = env::var_os("OPENCODE_CONFIG_CONTENT") {
        let content = content.to_string_lossy();
        if !content.trim().is_empty() {
            match parse_local_model_catalog(&content, LocalModelSource::Env, None) {
                Ok(catalog) => return Ok(catalog),
                Err(err) => last_error = Some(err),
            }
        }
    }

    for path in config_file_candidates() {
        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("read OpenCode config file: {}", path.display()))?;
            match parse_local_model_catalog(&content, LocalModelSource::File, Some(path.clone())) {
                Ok(catalog) => return Ok(catalog),
                Err(err) => last_error = Some(err),
            }
        }
    }

    if let Some(err) = last_error {
        return Err(err.context("load OpenCode local model config"));
    }

    Ok(LocalModelCatalog {
        source: LocalModelSource::Missing,
        resolved_path: None,
        malformed_entries: 0,
        providers: BTreeMap::new(),
    })
}

pub fn parse_local_model_catalog(
    content: &str,
    source: LocalModelSource,
    resolved_path: Option<PathBuf>,
) -> Result<LocalModelCatalog> {
    let value: Value =
        json5::from_str(content).map_err(|_| anyhow::anyhow!("parse OpenCode config JSONC"))?;
    let (providers, malformed_entries) = parse_provider_entries(value)?;
    Ok(LocalModelCatalog {
        source,
        resolved_path,
        malformed_entries,
        providers,
    })
}

fn parse_provider_entries(value: Value) -> Result<(BTreeMap<String, LocalModelProvider>, usize)> {
    let Some(object) = value.as_object() else {
        bail!("OpenCode config must be a JSON object");
    };

    let entries = object
        .get("providers")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_else(|| object.clone());

    let mut providers = BTreeMap::new();
    let mut malformed_entries = 0usize;

    for (provider_id, value) in entries {
        match parse_provider_entry(&provider_id, value) {
            Ok(provider) => {
                providers.insert(provider_id, provider);
            }
            Err(_) => malformed_entries += 1,
        }
    }

    Ok((providers, malformed_entries))
}

fn parse_provider_entry(provider_id: &str, value: Value) -> Result<LocalModelProvider> {
    let provider_id = validate_provider_id(provider_id)?.to_string();
    let Some(object) = value.as_object() else {
        bail!("invalid local provider entry")
    };
    let Some(options) = object.get("options").and_then(|value| value.as_object()) else {
        bail!("missing local provider options")
    };
    let Some(base_url) = options.get("baseURL").and_then(|value| value.as_str()) else {
        bail!("missing local provider baseURL")
    };
    let endpoint = validate_local_model_endpoint(base_url)?;
    let Some(models) = object.get("models").and_then(|value| value.as_array()) else {
        bail!("missing local provider models")
    };

    let mut parsed_models = Vec::new();
    for model in models {
        let Some(model) = model.as_str() else {
            bail!("invalid local model id")
        };
        parsed_models.push(validate_model_id(model)?.to_string());
    }
    if parsed_models.is_empty() {
        bail!("empty local provider models")
    }

    Ok(LocalModelProvider {
        provider_id,
        endpoint,
        models: parsed_models,
        origin: LocalModelOrigin::Config,
    })
}

fn config_file_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut base_dirs = Vec::new();
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        base_dirs.push(PathBuf::from(xdg));
    }
    if let Some(home) = dirs::home_dir() {
        base_dirs.push(home.join(".config"));
    }
    for base in base_dirs {
        out.push(base.join("opencode/opencode.jsonc"));
        out.push(base.join("opencode/opencode.json"));
    }
    out
}

fn parse_provider_model_spec(spec: &str) -> Result<(&str, &str)> {
    let Some((provider_id, model_id)) = spec.split_once('/') else {
        bail!("provider selection must be provider/model")
    };
    let provider_id = validate_provider_id(provider_id)?;
    let model_id = validate_model_id(model_id)?;
    Ok((provider_id, model_id))
}

pub fn builtin_template(provider_id: &str, model_id: &str) -> Option<(Url, String)> {
    let provider = builtin_template_provider(provider_id, model_id)?;
    Some((provider.endpoint, model_id.to_string()))
}

pub fn format_local_model_entry(entry: &LocalModelEntry) -> String {
    format!(
        "{}  model={} endpoint={} source={}",
        sanitize_terminal(&entry.provider_id),
        sanitize_terminal(&entry.model_id),
        sanitize_terminal(&entry.endpoint),
        entry.origin
    )
}
