use std::collections::{BTreeMap, BTreeSet};

use crate::value::VmValue;

use super::*;

const GITHUB_CREDENTIAL_ENV_KEYS: &[&str] =
    &["GITHUB_PERSONAL_ACCESS_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];

#[derive(Debug, Clone)]
pub(super) struct McpPresetCandidate {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) missing_credentials: Vec<String>,
}

impl McpPresetCandidate {
    pub(super) fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), VmValue::string(self.id));
        out.insert("status".to_string(), VmValue::string(self.status));
        out.insert(
            "missing_credentials".to_string(),
            string_list_value(self.missing_credentials),
        );
        VmValue::dict(out)
    }
}

pub(super) fn mcp_preset_candidate(id: &str, credentials: &BTreeSet<String>) -> McpPresetCandidate {
    let missing_credentials = missing_preset_credentials(id, credentials);
    McpPresetCandidate {
        id: id.to_string(),
        status: if missing_credentials.is_empty() {
            "ready".to_string()
        } else {
            "needs_credentials".to_string()
        },
        missing_credentials,
    }
}

fn missing_preset_credentials(id: &str, credentials: &BTreeSet<String>) -> Vec<String> {
    let Some(preset) = crate::mcp_presets::preset(id) else {
        return Vec::new();
    };
    let mut missing = Vec::new();
    for placeholder in &preset.placeholders {
        if !placeholder.required {
            continue;
        }
        if placeholder.target != crate::mcp_presets::PlaceholderTarget::Env {
            missing.push(placeholder.key.clone());
            continue;
        }
        let alias = credential_alias(&placeholder.key);
        if !credentials.contains(&alias) {
            missing.push(placeholder.key.clone());
        }
    }
    missing
}

pub(super) fn parse_credentials(value: &VmValue) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match value {
        VmValue::List(items) => {
            for item in items.iter() {
                let credential = credential_alias(&item.display());
                if !credential.is_empty() {
                    out.insert(credential);
                }
            }
        }
        VmValue::Dict(dict) => {
            for (key, value) in dict.iter() {
                if matches!(value, VmValue::Bool(false) | VmValue::Nil) {
                    continue;
                }
                let credential = credential_alias(key);
                if !credential.is_empty() {
                    out.insert(credential);
                }
            }
        }
        _ => {}
    }
    out
}

pub(super) fn env_credentials() -> BTreeSet<String> {
    GITHUB_CREDENTIAL_ENV_KEYS
        .iter()
        .filter(|key| {
            std::env::var(key)
                .ok()
                .is_some_and(|value| !value.is_empty())
        })
        .map(|key| credential_alias(key))
        .collect()
}

fn credential_alias(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return String::new();
    }
    if normalized.contains("github") || normalized == "gh_token" || normalized == "gh-token" {
        return "github".to_string();
    }
    normalized.replace('-', "_")
}
