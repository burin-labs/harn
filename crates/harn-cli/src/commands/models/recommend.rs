//! `harn models recommend` — pick a starter model for the current
//! machine and credentials.
//!
//! ## .harn dispatch (W9 — see harn#2309)
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
//!
//! `HARN_CLI_IMPL=rust` keeps the legacy direct path for the
//! parity-snapshot harness (#2299) until the C1 ratchet (#2314) lands.

use std::collections::BTreeSet;
use std::io::Write as _;

use serde::{Deserialize, Serialize};

use crate::cli::ModelRecommendArgs;
use crate::commands::hardware::{
    bytes_to_gib_floor, bytes_to_gib_rounded, collect_hardware_snapshot, GpuKind, HardwareSnapshot,
};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const RECOMMENDATIONS_TOML: &str = include_str!("../../../data/model_recommendations.toml");
const CLOUD_DEFAULT_SENTINEL: &str = "$cloud_default";
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
/// race on the global env var. Same pattern as other partial-port
/// commands (see harn#2305 / #2306).
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
    fn from_available_bytes(bytes: Option<u64>) -> Self {
        let Some(bytes) = bytes else {
            return Self::Lt8;
        };
        match bytes_to_gib_floor(bytes) {
            0..=7 => Self::Lt8,
            8..=15 => Self::Between8And16,
            16..=31 => Self::Between16And32,
            _ => Self::Plus32,
        }
    }

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

impl RecommendationGpu {
    fn from_gpu(kind: GpuKind) -> Self {
        match kind {
            GpuKind::Mps => Self::Mps,
            GpuKind::Cuda => Self::Cuda,
            GpuKind::None => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => "no GPU acceleration",
            Self::Mps => "MPS available",
            Self::Cuda => "CUDA available",
        }
    }
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

#[derive(Debug, Clone, Serialize)]
struct ModelRecommendation {
    model_id: String,
    harn_selector: String,
    provider: String,
    rationale: String,
    ram_bucket: RamBucket,
    gpu: RecommendationGpu,
    has_provider_key: bool,
    hardware: HardwareSnapshot,
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
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_legacy(args);
        return;
    }
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

/// Legacy direct-render path. Kept verbatim for the parity-snapshot
/// harness (#2299) until C1 (#2314) deletes it.
fn run_legacy(args: &ModelRecommendArgs) {
    let snapshot = collect_hardware_snapshot();
    let cloud_model = detect_cloud_model();
    let has_provider_key = cloud_model.is_some();
    let recommendation = recommend_model(snapshot, has_provider_key, cloud_model)
        .unwrap_or_else(|error| crate::command_error(&error));

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&recommendation).unwrap_or_else(|error| {
                crate::command_error(&format!(
                    "failed to serialize model recommendation: {error}"
                ))
            })
        );
    } else {
        println!("{}", recommendation.model_id);
        println!("{}", recommendation.rationale);
    }
}

fn recommend_model(
    hardware: HardwareSnapshot,
    has_provider_key: bool,
    cloud_model: Option<CloudModel>,
) -> Result<ModelRecommendation, String> {
    let table = load_recommendation_table()?;
    validate_recommendation_table(&table)?;

    let ram_bucket = RamBucket::from_available_bytes(hardware.ram.available_bytes);
    let gpu = RecommendationGpu::from_gpu(hardware.gpu.kind);
    let rule = table
        .recommendations
        .iter()
        .find(|rule| {
            rule.ram_bucket == ram_bucket
                && rule.gpu == gpu
                && rule.has_provider_key == has_provider_key
        })
        .ok_or_else(|| {
            format!(
                "no model recommendation for ram_bucket={} gpu={:?} has_provider_key={has_provider_key}",
                ram_bucket.as_str(),
                gpu
            )
        })?;

    let (provider, model_id) = if rule.model_id == CLOUD_DEFAULT_SENTINEL {
        let cloud = cloud_model.ok_or_else(|| {
            "recommendation table requested a cloud default without cloud credentials".to_string()
        })?;
        let provider = cloud.provider;
        (provider.clone(), format!("{}/{}", provider, cloud.model_id))
    } else {
        (rule.provider.clone(), rule.model_id.clone())
    };
    let harn_selector = harn_selector_for(&provider, &model_id);
    let rationale = rationale_for(&hardware, gpu, has_provider_key, &model_id);

    Ok(ModelRecommendation {
        model_id,
        harn_selector,
        provider,
        rationale,
        ram_bucket,
        gpu,
        has_provider_key,
        hardware,
    })
}

fn load_recommendation_table() -> Result<RecommendationTable, String> {
    toml::from_str(RECOMMENDATIONS_TOML)
        .map_err(|error| format!("failed to parse model_recommendations.toml: {error}"))
}

fn validate_recommendation_table(table: &RecommendationTable) -> Result<(), String> {
    let mut seen = BTreeSet::new();
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

fn harn_selector_for(provider: &str, model_id: &str) -> String {
    if provider == "ollama" {
        return model_id
            .strip_prefix("ollama/")
            .map(|model| format!("ollama:{model}"))
            .unwrap_or_else(|| model_id.to_string());
    }
    model_id
        .strip_prefix(&format!("{provider}/"))
        .unwrap_or(model_id)
        .to_string()
}

fn rationale_for(
    hardware: &HardwareSnapshot,
    gpu: RecommendationGpu,
    has_provider_key: bool,
    model_id: &str,
) -> String {
    let ram = match hardware.ram.available_bytes {
        Some(bytes) => format!("{} GB free", bytes_to_gib_rounded(bytes)),
        None => "unknown free RAM".to_string(),
    };
    let creds = if has_provider_key {
        "cloud creds available"
    } else {
        "no cloud creds"
    };
    format!("{ram}, {}, {creds} -> {model_id}", gpu.label())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        harn_selector_for, load_recommendation_table, recommend_model,
        validate_recommendation_table, CloudModel, GPU_KEYS, RAM_BUCKETS,
    };
    use crate::commands::hardware::{
        DiskSnapshot, GpuKind, GpuSnapshot, HardwareSnapshot, RamSnapshot,
    };

    const GIB: u64 = 1024 * 1024 * 1024;

    fn snapshot(available_gib: u64, gpu: GpuKind) -> HardwareSnapshot {
        HardwareSnapshot {
            ram: RamSnapshot {
                total_bytes: Some(16 * GIB),
                available_bytes: Some(available_gib * GIB),
            },
            gpu: GpuSnapshot { kind: gpu },
            disk: DiskSnapshot {
                path: PathBuf::from("/workspace"),
                free_bytes: Some(128 * GIB),
            },
        }
    }

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
    fn no_cloud_creds_recommends_ollama_by_ram_and_acceleration() {
        let recommendation =
            recommend_model(snapshot(8, GpuKind::Mps), false, None).expect("recommendation");
        assert_eq!(recommendation.model_id, "ollama/qwen2.5:7b-instruct");
        assert_eq!(recommendation.harn_selector, "ollama:qwen2.5:7b-instruct");
        assert_eq!(
            recommendation.rationale,
            "8 GB free, MPS available, no cloud creds -> ollama/qwen2.5:7b-instruct"
        );
    }

    #[test]
    fn high_memory_mps_recommends_current_local_coding_alias() {
        let recommendation =
            recommend_model(snapshot(48, GpuKind::Mps), false, None).expect("recommendation");
        assert_eq!(recommendation.model_id, "qwen3.6-coding");
        assert_eq!(recommendation.harn_selector, "qwen3.6-coding");
        assert_eq!(recommendation.provider, "ollama");
    }

    #[test]
    fn cloud_creds_resolve_to_best_available_cloud_default() {
        let recommendation = recommend_model(
            snapshot(32, GpuKind::Cuda),
            true,
            Some(CloudModel {
                provider: "openai".to_string(),
                model_id: "gpt-4o-mini".to_string(),
            }),
        )
        .expect("recommendation");
        assert_eq!(recommendation.model_id, "openai/gpt-4o-mini");
        assert_eq!(recommendation.harn_selector, "gpt-4o-mini");
        assert_eq!(recommendation.provider, "openai");
        assert!(recommendation
            .rationale
            .contains("CUDA available, cloud creds available"));
    }

    #[test]
    fn harn_selector_normalizes_provider_display_prefixes() {
        assert_eq!(
            harn_selector_for("ollama", "ollama/qwen2.5:7b-instruct"),
            "ollama:qwen2.5:7b-instruct"
        );
        assert_eq!(
            harn_selector_for("openai", "openai/gpt-4o-mini"),
            "gpt-4o-mini"
        );
    }
}
