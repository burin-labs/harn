//! `harn models recommend` — pick a starter model for the current
//! machine and credentials.
//!
//! ## Harn renderer
//!
//! The rule-lookup + rendering pipeline lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/recommend.harn`. Hardware
//! probing (`sysctl` / `/proc/meminfo` / `nvidia-smi` /
//! `MetalPerformanceShaders`), provider credential detection
//! (`llm_config::provider_key_available`), and parsing
//! `data/model_recommendations.toml` all stay in Rust — none of those
//! capabilities are exposed to script-land today, and the sandbox
//! would block the subprocess calls. The Rust shim collects everything
//! into a single JSON payload and hands it across; the script picks
//! the matching rule and formats the output.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use serde::{Deserialize, Serialize};

use crate::cli::ModelRecommendArgs;
use crate::commands::hardware::{collect_hardware_snapshot, HardwareSnapshot};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const RECOMMENDATIONS_TOML: &str = include_str!("../../../data/model_recommendations.toml");
const RAM_BUCKETS: [RamBucket; 4] = [
    RamBucket::Lt8,
    RamBucket::Between8And16,
    RamBucket::Between16And32,
    RamBucket::Plus32,
];
const GPU_KEYS: [RecommendationGpu; 3] = [
    RecommendationGpu::None,
    RecommendationGpu::Mps,
    RecommendationGpu::Cuda,
];

/// Env var carrying the full recommend payload (hardware snapshot,
/// has-provider-key flag, cloud-model resolution, parsed recommendation
/// table) handed to the embedded `cli/models/recommend` script.
const RECOMMEND_PAYLOAD_ENV: &str = "HARN_MODELS_RECOMMEND_PAYLOAD_JSON";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env var.
static DISPATCH_RECOMMEND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RamBucket {
    Lt8,
    #[serde(rename = "8_16")]
    Between8And16,
    #[serde(rename = "16_32")]
    Between16And32,
    #[serde(rename = "32_plus")]
    Plus32,
}

impl RamBucket {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lt8 => "lt8",
            Self::Between8And16 => "8_16",
            Self::Between16And32 => "16_32",
            Self::Plus32 => "32_plus",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecommendationGpu {
    None,
    Mps,
    Cuda,
}

#[derive(Debug, Deserialize)]
struct RecommendationTable {
    recommendations: Vec<RecommendationRule>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RecommendationRule {
    ram_bucket: RamBucket,
    gpu: RecommendationGpu,
    has_provider_key: bool,
    provider: String,
    model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CloudModel {
    provider: String,
    model_id: String,
}

/// JSON payload handed to the embedded `cli/models/recommend` script.
/// The script picks the matching rule and renders — see the script's
/// docstring for the input contract.
#[derive(Debug, Serialize)]
struct RecommendDispatchPayload<'a> {
    hardware: &'a HardwareSnapshot,
    has_provider_key: bool,
    cloud_model: Option<&'a CloudModel>,
    recommendations: &'a [RecommendationRule],
}

pub(crate) async fn run(args: &ModelRecommendArgs) {
    let exit_code = run_dispatch(args).await;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_dispatch(args: &ModelRecommendArgs) -> i32 {
    let snapshot = collect_hardware_snapshot();
    let cloud_model = detect_cloud_model();
    let has_provider_key = cloud_model.is_some();
    let table = match load_recommendation_table() {
        Ok(table) => table,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if let Err(error) = validate_recommendation_table(&table) {
        eprintln!("error: {error}");
        return 1;
    }

    let payload = RecommendDispatchPayload {
        hardware: &snapshot,
        has_provider_key,
        cloud_model: cloud_model.as_ref(),
        recommendations: &table.recommendations,
    };
    let payload_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise recommend payload: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_RECOMMEND_LOCK.lock().await;
    let _payload_guard = ScopedEnvVar::set(RECOMMEND_PAYLOAD_ENV, &payload_json);
    let outcome = dispatch::run_embedded_script("models/recommend", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

fn load_recommendation_table() -> Result<RecommendationTable, String> {
    toml::from_str(RECOMMENDATIONS_TOML)
        .map_err(|error| format!("failed to parse model_recommendations.toml: {error}"))
}

fn validate_recommendation_table(table: &RecommendationTable) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let aliases = harn_vm::llm_config::alias_entries()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for rule in &table.recommendations {
        let key = (rule.ram_bucket, rule.gpu, rule.has_provider_key);
        if !seen.insert(key) {
            return Err(format!(
                "duplicate model recommendation for ram_bucket={} gpu={:?} has_provider_key={}",
                rule.ram_bucket.as_str(),
                rule.gpu,
                rule.has_provider_key
            ));
        }
        validate_recommendation_model(rule, &aliases)?;
    }
    let expected_count = RAM_BUCKETS.len() * GPU_KEYS.len() * 2;
    if seen.len() != expected_count {
        return Err(format!(
            "model recommendation table covers {} tuples; expected {expected_count}",
            seen.len()
        ));
    }
    Ok(())
}

fn validate_recommendation_model(
    rule: &RecommendationRule,
    aliases: &BTreeMap<String, harn_vm::llm_config::AliasDef>,
) -> Result<(), String> {
    if rule.provider == "cloud" {
        if rule.model_id == "$cloud_default" {
            return Ok(());
        }
        return Err(format!(
            "cloud model recommendation for ram_bucket={} gpu={:?} must use $cloud_default, got {}",
            rule.ram_bucket.as_str(),
            rule.gpu,
            rule.model_id
        ));
    }
    if rule.model_id == "$cloud_default" {
        return Err(format!(
            "non-cloud model recommendation for ram_bucket={} gpu={:?} uses $cloud_default",
            rule.ram_bucket.as_str(),
            rule.gpu
        ));
    }

    let resolved_model_id;
    let (model_id, provider, alias_label) = if let Some(alias) = aliases.get(&rule.model_id) {
        (
            alias.id.as_str(),
            alias.provider.as_str(),
            Some(rule.model_id.as_str()),
        )
    } else {
        resolved_model_id = harn_vm::llm_config::normalize_model_id(&rule.model_id);
        (resolved_model_id.as_str(), rule.provider.as_str(), None)
    };
    if provider != rule.provider {
        return Err(format!(
            "model recommendation for ram_bucket={} gpu={:?} says provider={} but {} routes to provider={provider}",
            rule.ram_bucket.as_str(),
            rule.gpu,
            rule.provider,
            alias_label.unwrap_or(&rule.model_id)
        ));
    }
    let Some(model) = harn_vm::llm_config::model_catalog_entry(model_id) else {
        return Err(format!(
            "model recommendation for ram_bucket={} gpu={:?} references unknown model_id={} (resolved id={model_id})",
            rule.ram_bucket.as_str(),
            rule.gpu,
            rule.model_id
        ));
    };
    if model.provider != provider {
        return Err(format!(
            "model recommendation for ram_bucket={} gpu={:?} resolves {} to provider={} but row provider={provider}",
            rule.ram_bucket.as_str(),
            rule.gpu,
            rule.model_id,
            model.provider
        ));
    }
    Ok(())
}

fn detect_cloud_model() -> Option<CloudModel> {
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
    harn_vm::llm_config::provider_key_available(provider)
}

#[cfg(test)]
mod tests {
    use super::{
        load_recommendation_table, validate_recommendation_table, RamBucket, RecommendationGpu,
        RecommendationRule, RecommendationTable, GPU_KEYS, RAM_BUCKETS,
    };

    #[test]
    fn recommendation_table_has_unique_tuple_keys() {
        let table = load_recommendation_table().expect("table parses");
        validate_recommendation_table(&table).expect("table is unique");
        assert_eq!(
            table.recommendations.len(),
            RAM_BUCKETS.len() * GPU_KEYS.len() * 2
        );
    }

    #[test]
    fn recommendation_table_rejects_unknown_local_model_ids() {
        let table = RecommendationTable {
            recommendations: vec![RecommendationRule {
                ram_bucket: RamBucket::Lt8,
                gpu: RecommendationGpu::None,
                has_provider_key: false,
                provider: "ollama".to_string(),
                model_id: "ollama/qwen2.5:3b-instruct".to_string(),
            }],
        };
        let error = validate_recommendation_table(&table).expect_err("dead model should fail");
        assert!(error.contains("unknown model_id=ollama/qwen2.5:3b-instruct"));
    }
}
