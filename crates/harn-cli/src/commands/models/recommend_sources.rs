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
    match harn_vm::llm::provider_auth_status(provider).credential_status {
        harn_vm::llm::ProviderCredentialStatus::Ok => true,
        // `Deferred` says Harn does not resolve this provider's credential at
        // all — the platform SDK does, at call time, from an ambient source
        // such as GCP application-default credentials or an AWS instance role.
        // That is a statement about who resolves it, not evidence that one
        // exists, and `resolve_api_key` returns an empty string for it. This
        // command answers "what should someone who has configured nothing
        // start with", so it has to find the ambient source before saying yes.
        harn_vm::llm::ProviderCredentialStatus::Deferred => {
            platform_managed_credential_present(provider, &def)
        }
        _ => false,
    }
}

/// Positive, offline evidence that a platform-managed provider's ambient
/// credential exists.
///
/// The generic signal comes first: a provider's own declared `auth_env` names
/// the variables it authenticates from, and `platform_managed` short-circuits
/// to `Deferred` before those are ever probed, which is what let a provider
/// claim a credential it did not have. Anything beyond that has to be taught
/// per provider, and an unknown one fails closed — inheriting another
/// platform's discovery rules would reintroduce the same false positive.
///
/// Today `vertex` is the only provider that reaches here: `bedrock` is the
/// other `platform_managed` entry, but it declares no `auth_env` and is
/// rejected by the caller before this point.
fn platform_managed_credential_present(
    provider: &str,
    def: &harn_vm::llm_config::ProviderDef,
) -> bool {
    if declared_auth_env_set(def) {
        return true;
    }
    provider == "vertex" && google_adc_file_present()
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn declared_auth_env_set(def: &harn_vm::llm_config::ProviderDef) -> bool {
    match &def.auth_env {
        harn_vm::llm_config::AuthEnv::None => false,
        harn_vm::llm_config::AuthEnv::Single(name) => env_set(name),
        harn_vm::llm_config::AuthEnv::Multiple(names) => names.iter().any(|name| env_set(name)),
    }
}

/// The gcloud application-default credentials file, which is a file rather
/// than an environment variable and so is invisible to `declared_auth_env_set`.
/// The GCE metadata server is deliberately not probed: it is a network call on
/// a startup path, and its absence is not evidence either way.
fn google_adc_file_present() -> bool {
    let adc = std::env::var_os("CLOUDSDK_CONFIG")
        .map(PathBuf::from)
        .or_else(|| harn_vm::user_dirs::home_dir().map(|home| home.join(".config/gcloud")))
        .map(|dir| dir.join("application_default_credentials.json"));
    adc.is_some_and(|path| path.exists())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_guard::ScopedEnvVar;
    use crate::tests::common::harn_state_lock::lock_harn_state;

    /// Clear every source `vertex` can authenticate from, so the only thing
    /// left is its `platform_managed` status.
    fn without_any_vertex_credential(home: &Path) -> Vec<ScopedEnvVar> {
        vec![
            ScopedEnvVar::set("HOME", home.to_str().unwrap()),
            ScopedEnvVar::unset("CLOUDSDK_CONFIG"),
            ScopedEnvVar::unset("VERTEX_AI_ACCESS_TOKEN"),
            ScopedEnvVar::unset("GOOGLE_OAUTH_ACCESS_TOKEN"),
            ScopedEnvVar::unset("GOOGLE_APPLICATION_CREDENTIALS"),
        ]
    }

    /// `vertex` is `platform_managed`, so `provider_auth_status` answers
    /// `Deferred` regardless of what is configured — harn-vm asserts exactly
    /// that, alongside `resolve_api_key("vertex") == ""`. Counting `Deferred`
    /// by itself made every `has_provider_key = false` row in the
    /// recommendation table unreachable and handed a cloud model to someone
    /// with no account at all.
    #[test]
    fn deferred_alone_is_not_a_usable_cloud_credential() {
        let _lock = lock_harn_state();
        let tmp = tempfile::tempdir().unwrap();
        let _env = without_any_vertex_credential(tmp.path());

        assert!(!cloud_provider_key_available("vertex"));
    }

    #[test]
    fn a_declared_auth_env_var_is_positive_evidence() {
        let _lock = lock_harn_state();
        let tmp = tempfile::tempdir().unwrap();
        let _env = without_any_vertex_credential(tmp.path());
        let _token = ScopedEnvVar::set("VERTEX_AI_ACCESS_TOKEN", "placeholder");

        assert!(cloud_provider_key_available("vertex"));
    }

    #[test]
    fn the_gcloud_adc_file_is_positive_evidence() {
        let _lock = lock_harn_state();
        let tmp = tempfile::tempdir().unwrap();
        let _env = without_any_vertex_credential(tmp.path());

        let gcloud = tmp.path().join(".config/gcloud");
        std::fs::create_dir_all(&gcloud).unwrap();
        std::fs::write(gcloud.join("application_default_credentials.json"), "{}").unwrap();

        assert!(cloud_provider_key_available("vertex"));
    }
}
