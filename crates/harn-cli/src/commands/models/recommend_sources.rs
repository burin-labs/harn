use std::path::{Path, PathBuf};

use crate::commands::hardware::{GpuKind, HardwareSnapshot};

use super::recommend::{
    ram_bucket_from_available_bytes, recommendation_gpu_from_kind, CloudModel, LocalModel,
    RecommendationTable,
};

pub(super) fn detect_cloud_model() -> Option<CloudModel> {
    for provider in cloud_provider_candidates() {
        if cloud_provider_key_available(&provider) {
            let model_id = cloud_model_for_provider(&provider);
            return Some(CloudModel { provider, model_id });
        }
    }
    None
}

fn cloud_provider_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    push_unique(&mut candidates, harn_vm::llm_config::default_provider());
    for provider in [
        "anthropic",
        "openai",
        "openrouter",
        "gemini",
        "together",
        "groq",
        "cerebras",
        "deepseek",
        "fireworks",
        "dashscope",
        "huggingface",
        "azure_openai",
    ] {
        push_unique(&mut candidates, provider.to_string());
    }
    let mut provider_names = harn_vm::llm_config::provider_names();
    provider_names.sort();
    for provider in provider_names {
        push_unique(&mut candidates, provider);
    }
    candidates
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn cloud_model_for_provider(provider: &str) -> String {
    harn_vm::llm::selected_model_for_provider(provider)
        .or_else(|| harn_vm::llm_config::qc_default_model(provider))
        .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider(provider))
}

fn cloud_provider_key_available(provider: &str) -> bool {
    let Some(def) = harn_vm::llm_config::provider_config(provider) else {
        return false;
    };
    if def.auth_style == "none" || matches!(def.auth_env, harn_vm::llm_config::AuthEnv::None) {
        return false;
    }
    matches!(
        harn_vm::llm::provider_auth_status(provider).credential_status,
        harn_vm::llm::ProviderCredentialStatus::Ok
            | harn_vm::llm::ProviderCredentialStatus::Deferred
    )
}

pub(super) fn detect_local_model(
    snapshot: &HardwareSnapshot,
    table: &RecommendationTable,
) -> Option<LocalModel> {
    if matches!(snapshot.gpu.kind, GpuKind::None) {
        return None;
    }
    let ram_bucket = ram_bucket_from_available_bytes(snapshot.ram.available_bytes);
    let gpu = recommendation_gpu_from_kind(snapshot.gpu.kind);
    let rule = table
        .recommendations
        .iter()
        .find(|rule| rule.ram_bucket == ram_bucket && rule.gpu == gpu && !rule.has_provider_key)?;
    if rule.provider == "cloud" {
        return None;
    }
    Some(LocalModel {
        provider: rule.provider.clone(),
        model_id: rule.model_id.clone(),
        harn_selector: harn_selector_for(&rule.provider, &rule.model_id),
        cached: local_model_cache_ready(&rule.provider, &rule.model_id),
    })
}

fn harn_selector_for(provider: &str, model_id: &str) -> String {
    if provider == "ollama" {
        if let Some(model) = model_id.strip_prefix("ollama/") {
            return format!("ollama:{model}");
        }
    } else if let Some(model) = model_id.strip_prefix(&format!("{provider}/")) {
        return model.to_string();
    }
    model_id.to_string()
}

fn local_model_cache_ready(provider: &str, model_id: &str) -> bool {
    provider == "llamacpp"
        && model_id.contains("qwen3.6")
        && hf_cache_contains_repo("unsloth/Qwen3.6-35B-A3B-GGUF")
}

fn hf_cache_contains_repo(repo_id: &str) -> bool {
    hf_cache_roots()
        .into_iter()
        .any(|root| hf_cache_repo_dir(&root, repo_id).is_dir())
}

fn hf_cache_roots() -> Vec<PathBuf> {
    if let Ok(cache) = std::env::var("HUGGINGFACE_HUB_CACHE") {
        return vec![PathBuf::from(cache)];
    }
    if let Ok(cache) = std::env::var("HF_HUB_CACHE") {
        return vec![PathBuf::from(cache)];
    }
    if let Ok(home) = std::env::var("HF_HOME") {
        return vec![PathBuf::from(home).join("hub")];
    }
    harn_vm::user_dirs::home_dir()
        .map(|home| home.join(".cache").join("huggingface").join("hub"))
        .into_iter()
        .collect()
}

#[cfg(test)]
pub(super) fn hf_cache_repo_dir(root: &Path, repo_id: &str) -> PathBuf {
    hf_cache_repo_dir_impl(root, repo_id)
}

#[cfg(not(test))]
fn hf_cache_repo_dir(root: &Path, repo_id: &str) -> PathBuf {
    hf_cache_repo_dir_impl(root, repo_id)
}

fn hf_cache_repo_dir_impl(root: &Path, repo_id: &str) -> PathBuf {
    let cache_name = format!("models--{}", repo_id.replace('/', "--"));
    root.join(cache_name)
}
