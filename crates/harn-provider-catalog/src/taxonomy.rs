//! Catalog-owned model family, lineage, and tier facts.

use crate::config::ProvidersConfig;
use harn_glob::match_name as glob_match;

pub fn model_tier_with_config(config: &ProvidersConfig, model_id: &str) -> String {
    if let Some(tier) = config
        .models
        .get(model_id)
        .and_then(|model| model.tier.as_deref())
    {
        let trimmed = tier.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    for rule in &config.tier_rules {
        if rule.exact.as_deref() == Some(model_id)
            || rule
                .pattern
                .as_deref()
                .is_some_and(|pattern| glob_match(pattern, model_id))
            || rule
                .contains
                .as_deref()
                .is_some_and(|part| model_id.contains(part))
        {
            return rule.tier.clone();
        }
    }
    config.tier_defaults.default.clone()
}

pub fn model_family_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.family.as_deref()))
        .unwrap_or_else(|| derive_model_family(provider, model_id))
}

pub fn model_lineage_with_config(
    config: &ProvidersConfig,
    provider: &str,
    model_id: &str,
) -> String {
    config
        .models
        .get(model_id)
        .and_then(|model| normalized_catalog_token(model.lineage.as_deref()))
        .unwrap_or_else(|| derive_model_lineage(provider, model_id))
}

pub fn normalized_catalog_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase().replace('_', "-"))
}

pub fn derive_model_family(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["claude", "anthropic.claude"]) {
        return "anthropic-claude".to_string();
    }
    if contains_any(&id, &["gemini", "google/gemini"]) {
        return "google-gemini".to_string();
    }
    if contains_any(&id, &["deepseek"]) {
        return "deepseek".to_string();
    }
    if contains_any(&id, &["qwen"]) {
        return "qwen".to_string();
    }
    if contains_any(&id, &["kimi", "moonshot"]) {
        return "kimi".to_string();
    }
    if contains_any(&id, &["glm", "z-ai/glm", "zhipu"]) {
        return "glm".to_string();
    }
    if contains_any(&id, &["mistral", "mixtral", "devstral"]) {
        return "mistral".to_string();
    }
    if contains_any(&id, &["minimax"]) {
        return "minimax".to_string();
    }
    if contains_any(&id, &["llama"]) {
        return "llama".to_string();
    }
    if contains_any(&id, &["gemma"]) {
        return "gemma".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-oss", "openai/gpt", "gpt-"]) {
        return "openai-gpt".to_string();
    }
    match provider {
        "anthropic" | "bedrock" | "vertex-anthropic" => "anthropic-claude".to_string(),
        "openai" | "azure" | "azure_openai" => "openai-gpt".to_string(),
        "gemini" | "vertex" | "google" => "google-gemini".to_string(),
        "deepseek" => "deepseek".to_string(),
        "zai" => "glm".to_string(),
        "minimax" => "minimax".to_string(),
        other if !other.is_empty() => normalize_identifier_token(other),
        _ => "unknown".to_string(),
    }
}

pub fn derive_model_lineage(provider: &str, model_id: &str) -> String {
    let id = model_id.to_ascii_lowercase();
    if contains_any(&id, &["haiku"]) {
        return "claude-haiku".to_string();
    }
    if contains_any(&id, &["opus-4-7", "opus-4-8", "opus-mythos"]) {
        return "claude-opus-adaptive".to_string();
    }
    if contains_any(&id, &["claude"]) {
        return "claude-sonnet-opus".to_string();
    }
    if contains_any(&id, &["gpt-5"]) {
        return "openai-gpt5".to_string();
    }
    if is_openai_reasoning_model(&id) {
        return "openai-reasoning".to_string();
    }
    if contains_any(&id, &["gpt-", "gpt_"]) {
        return "openai-legacy".to_string();
    }
    if contains_any(&id, &["gemini"]) {
        if contains_any(&id, &["flash"]) {
            return "gemini-flash".to_string();
        }
        return "gemini-pro".to_string();
    }
    if contains_any(&id, &["qwen3", "qwen/qwen3"]) {
        return "qwen3".to_string();
    }
    if contains_any(&id, &["gemma4", "gemma-4"]) {
        return "gemma4".to_string();
    }
    derive_model_family(provider, model_id)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn is_openai_reasoning_model(id: &str) -> bool {
    ["o1", "o3", "o4"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
        || contains_any(
            id,
            &[
                "/o1", "/o3", "/o4", ":o1", ":o3", ":o4", ".o1", ".o3", ".o4",
            ],
        )
}

fn normalize_identifier_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
