//! Data-driven local runtime risk profiles.
//!
//! This layer keeps local model selection explicit: the model family, runtime,
//! known risks, required probes, and workarounds are all table data that CLI
//! lifecycle commands can explain and enforce.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::tool_conformance::{report_satisfies_required_probe, ToolConformanceReport};
use crate::llm_config::{self, LocalMemoryDef, ModelDef};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProfileStatus {
    Preferred,
    Experimental,
    VisionOnlyExperimental,
    Quarantined,
    Unknown,
}

impl RuntimeProfileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preferred => "preferred",
            Self::Experimental => "experimental",
            Self::VisionOnlyExperimental => "vision_only_experimental",
            Self::Quarantined => "quarantined",
            Self::Unknown => "unknown",
        }
    }

    pub fn requires_probe_gate(&self) -> bool {
        !matches!(self, Self::Preferred | Self::Unknown)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProfile {
    pub status: RuntimeProfileStatus,
    pub requires: Vec<String>,
    pub recommended_num_ctx: Option<u64>,
    pub known_risks: Vec<String>,
    pub workarounds: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRuntimeProfileReport {
    pub alias: Option<String>,
    pub model_id: String,
    pub provider: String,
    pub model_family: String,
    pub selected_runtime: String,
    pub selected_status: RuntimeProfileStatus,
    pub requires_probe_gate: bool,
    pub selected: RuntimeProfile,
    pub runtime_profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RuntimeProfileHost {
    pub system_available_gib: Option<f64>,
    pub accelerator_free_gib: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProfileGate {
    pub allowed: bool,
    pub forced: bool,
    pub selected_status: RuntimeProfileStatus,
    pub missing_required_probes: Vec<String>,
    pub passed_probes: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeProbeEvidence {
    passed: BTreeSet<String>,
    tool_reports: Vec<ToolConformanceReport>,
}

impl RuntimeProbeEvidence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_passed(&mut self, probe: impl Into<String>) {
        let probe = probe.into();
        if !probe.trim().is_empty() {
            self.passed.insert(probe);
        }
    }

    pub fn add_tool_report(&mut self, report: ToolConformanceReport) {
        if report_satisfies_required_probe(&report, "tool_probe") {
            self.passed.insert("tool_probe".to_string());
            self.passed.insert("tool_call_probe".to_string());
        }
        if report_satisfies_required_probe(&report, "native_tool_probe") {
            self.passed.insert("native_tool_probe".to_string());
        }
        if report_satisfies_required_probe(&report, "streaming_tool_probe") {
            self.passed.insert("streaming_tool_probe".to_string());
        }
        self.tool_reports.push(report);
    }

    pub fn passed(&self) -> Vec<String> {
        self.passed.iter().cloned().collect()
    }

    fn satisfies(&self, requirement: &str) -> bool {
        self.passed.contains(requirement)
            || self
                .tool_reports
                .iter()
                .any(|report| report_satisfies_required_probe(report, requirement))
    }
}

pub fn local_runtime_profile_report(
    selector: &str,
    provider_override: Option<&str>,
) -> LocalRuntimeProfileReport {
    let resolved = llm_config::resolve_model_info(selector);
    let provider = provider_override
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolved.provider.clone());
    local_runtime_profile_report_for(resolved.alias.as_deref(), &resolved.id, &provider)
}

pub fn local_runtime_profile_report_for(
    alias: Option<&str>,
    model_id: &str,
    provider: &str,
) -> LocalRuntimeProfileReport {
    local_runtime_profile_report_for_host(alias, model_id, provider, None)
}

pub fn local_runtime_profile_report_for_host(
    alias: Option<&str>,
    model_id: &str,
    provider: &str,
    host: Option<RuntimeProfileHost>,
) -> LocalRuntimeProfileReport {
    let family = model_family(alias, model_id);
    let catalog_model = llm_config::model_catalog_entry(model_id);
    let runtime_profiles = profiles_for_family(family)
        .into_iter()
        .map(|(runtime, profile)| {
            let adjusted = adjust_profile_for_host(
                family,
                &runtime,
                catalog_model.as_ref(),
                profile,
                host.as_ref(),
            );
            (runtime, adjusted)
        })
        .collect::<BTreeMap<_, _>>();
    let selected = runtime_profiles
        .get(provider)
        .cloned()
        .unwrap_or_else(|| generic_profile(provider));
    LocalRuntimeProfileReport {
        alias: alias.map(str::to_string),
        model_id: model_id.to_string(),
        provider: provider.to_string(),
        model_family: family.to_string(),
        selected_runtime: provider.to_string(),
        selected_status: selected.status.clone(),
        requires_probe_gate: selected.status.requires_probe_gate(),
        selected,
        runtime_profiles,
    }
}

pub fn evaluate_runtime_profile_gate(
    report: &LocalRuntimeProfileReport,
    evidence: &RuntimeProbeEvidence,
    force: bool,
) -> RuntimeProfileGate {
    let missing: Vec<String> = if report.selected_status.requires_probe_gate() {
        report
            .selected
            .requires
            .iter()
            .filter(|requirement| !evidence.satisfies(requirement))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let allowed = force || missing.is_empty();
    let message = if force {
        format!(
            "{} via {} is {} but allowed by --force",
            report.model_id,
            report.provider,
            report.selected_status.as_str()
        )
    } else if allowed {
        format!(
            "{} via {} is {}",
            report.model_id,
            report.provider,
            report.selected_status.as_str()
        )
    } else {
        format!(
            "{} via {} is {}; required probes missing: {}",
            report.model_id,
            report.provider,
            report.selected_status.as_str(),
            missing.join(", ")
        )
    };
    RuntimeProfileGate {
        allowed,
        forced: force,
        selected_status: report.selected_status.clone(),
        missing_required_probes: missing,
        passed_probes: evidence.passed(),
        message,
    }
}

fn model_family<'a>(alias: Option<&'a str>, model_id: &'a str) -> &'static str {
    let haystack = format!(
        "{} {}",
        alias.unwrap_or_default().to_ascii_lowercase(),
        model_id.to_ascii_lowercase()
    );
    if haystack.contains("qwen3.6") || haystack.contains("qwen36") {
        "qwen3.6-a3b-hybrid"
    } else if haystack.contains("gemma4") || haystack.contains("gemma-4") {
        "gemma4-hybrid-moe"
    } else {
        "generic-local"
    }
}

fn profiles_for_family(family: &str) -> BTreeMap<String, RuntimeProfile> {
    match family {
        "qwen3.6-a3b-hybrid" => BTreeMap::from([
            (
                "ollama".to_string(),
                profile(
                    RuntimeProfileStatus::Preferred,
                    &["tool_probe", "effective_context_probe"],
                    Some(32_768),
                    &[],
                    &[
                        "Use the text tool wire format unless a fresh native probe passes.",
                        "Keep an explicit num_ctx so the resident runner matches eval settings.",
                    ],
                    &["Best cheap local default on the 2026-05-13 Burin eval pass."],
                ),
            ),
            (
                "llamacpp".to_string(),
                profile(
                    RuntimeProfileStatus::Experimental,
                    &["tool_probe"],
                    Some(65_536),
                    &["inflated_input_token_accounting_on_repeated_turns"],
                    &[
                        "Run a tool probe before write-heavy evals.",
                        "Record llama.cpp build, ctx, cache type, and prefix-cache telemetry in eval receipts.",
                    ],
                    &[
                        "Current llama.cpp builds reuse two-turn Qwen3.6 hybrid-cache prefixes; keep token accounting visible in receipts.",
                    ],
                ),
            ),
            (
                "mlx".to_string(),
                profile(
                    RuntimeProfileStatus::VisionOnlyExperimental,
                    &[
                        "served_model_identity_probe",
                        "persistent_readiness_probe",
                        "tool_probe",
                    ],
                    None,
                    &[
                        "stale_or_default_v1_models_identity",
                        "hybrid_prefix_cache_reuse_gap",
                    ],
                    &[
                        "Probe /v1/models twice and send one minimal chat request before selection.",
                        "Record server flags for APC, context length, batching, and thinking mode.",
                    ],
                    &["Use only when MLX-specific throughput or vision support is needed."],
                ),
            ),
        ]),
        "gemma4-hybrid-moe" => BTreeMap::from([
            (
                "ollama".to_string(),
                profile(
                    RuntimeProfileStatus::Quarantined,
                    &["tool_probe"],
                    Some(32_768),
                    &[
                        "raw_tool_tag_no_structured_calls",
                        "completion_prose_without_executable_tool_calls",
                    ],
                    &[
                        "Allow only after the one-tool probe returns native or parseable text calls.",
                        "Use text mode and corrective retry for write-required turns.",
                    ],
                    &[
                        "Gemma4 through Ollama has produced raw <tool_call> blocks and final prose in local evals.",
                    ],
                ),
            ),
            (
                "llamacpp".to_string(),
                profile(
                    RuntimeProfileStatus::Experimental,
                    &["tool_probe", "two_turn_cache_probe"],
                    Some(32_768),
                    &[
                        "full_prompt_reprocess_on_hybrid_cache",
                        "parser_template_drift",
                    ],
                    &[
                        "Confirm the served template emits parseable calls before any write eval.",
                        "Treat final prose as insufficient when artifacts are unchanged.",
                    ],
                    &["Prefer as an eval candidate, not a default editing runtime."],
                ),
            ),
            (
                "mlx".to_string(),
                profile(
                    RuntimeProfileStatus::Experimental,
                    &[
                        "served_model_identity_probe",
                        "persistent_readiness_probe",
                        "tool_probe",
                    ],
                    None,
                    &[
                        "raw_gemma_tool_markers_in_content",
                        "hybrid_prefix_cache_reuse_gap",
                    ],
                    &[
                        "Keep raw marker parser fixtures enabled in the Harn text parser.",
                        "Verify OpenAI-compatible tool_calls is non-empty before native mode.",
                    ],
                    &["Use explicit server flags instead of opaque defaults."],
                ),
            ),
            (
                "local".to_string(),
                profile(
                    RuntimeProfileStatus::Experimental,
                    &["tool_probe"],
                    Some(32_768),
                    &["provider_specific_parser_required"],
                    &["Prefer text mode until native parser support is proven."],
                    &["Generic local Gemma endpoints vary by serving stack."],
                ),
            ),
        ]),
        _ => BTreeMap::new(),
    }
}

fn adjust_profile_for_host(
    family: &str,
    runtime: &str,
    model: Option<&ModelDef>,
    mut profile: RuntimeProfile,
    host: Option<&RuntimeProfileHost>,
) -> RuntimeProfile {
    if family == "qwen3.6-a3b-hybrid" && runtime == "llamacpp" {
        if let (Some(model), Some(host)) = (model, host) {
            if let Some(ctx) = recommended_context_from_local_memory(model, host) {
                profile.recommended_num_ctx = Some(ctx);
            }
        }
    }
    profile
}

fn recommended_context_from_local_memory(
    model: &ModelDef,
    host: &RuntimeProfileHost,
) -> Option<u64> {
    let memory = model.local_memory.as_ref()?;
    let available_gib = host
        .accelerator_free_gib
        .or(host.system_available_gib)
        .filter(|available| *available > 0.0)?;
    let base = memory.base_resident_gib?;
    let kv_per_1k = scaled_kv_cache_gib_per_1k(memory)?;
    let safety = memory.safety_margin_gib.unwrap_or(4.0);
    let usable_for_kv = available_gib - base - safety;
    if usable_for_kv <= 0.0 {
        return Some(8_192);
    }

    let by_memory = ((usable_for_kv / kv_per_1k) * 1_000.0).floor() as u64;
    let ceiling = memory
        .max_recommended_context
        .or(model.runtime_context_window)
        .unwrap_or(model.context_window)
        .min(model.context_window);
    let floor = 65_536_u64.min(ceiling).min(model.context_window);
    Some(round_context_down(by_memory.min(ceiling).max(floor)))
}

fn scaled_kv_cache_gib_per_1k(memory: &LocalMemoryDef) -> Option<f64> {
    let base = memory.kv_cache_gib_per_1k_ctx?;
    let multiplier = memory
        .default_cache_type
        .as_ref()
        .and_then(|cache_type| memory.cache_type_multipliers.get(cache_type))
        .copied()
        .unwrap_or(1.0);
    let scaled = base * multiplier;
    (scaled > 0.0).then_some(scaled)
}

fn round_context_down(ctx: u64) -> u64 {
    const STEP: u64 = 8_192;
    (ctx / STEP).max(1) * STEP
}

fn generic_profile(provider: &str) -> RuntimeProfile {
    RuntimeProfile {
        status: RuntimeProfileStatus::Unknown,
        requires: vec!["readiness_probe".to_string()],
        recommended_num_ctx: None,
        known_risks: Vec::new(),
        workarounds: Vec::new(),
        notes: vec![format!(
            "No dedicated local runtime profile for provider `{provider}` and this model family."
        )],
    }
}

fn profile(
    status: RuntimeProfileStatus,
    requires: &[&str],
    recommended_num_ctx: Option<u64>,
    known_risks: &[&str],
    workarounds: &[&str],
    notes: &[&str],
) -> RuntimeProfile {
    RuntimeProfile {
        status,
        requires: requires.iter().map(|value| (*value).to_string()).collect(),
        recommended_num_ctx,
        known_risks: known_risks
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        workarounds: workarounds
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        notes: notes.iter().map(|value| (*value).to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tool_conformance::{classify_tool_conformance_fixture, ToolProbeMode};

    #[test]
    fn qwen_ollama_profile_is_preferred_and_llamacpp_is_experimental() {
        let ollama = local_runtime_profile_report("local-qwen3.6", Some("ollama"));
        assert_eq!(ollama.model_family, "qwen3.6-a3b-hybrid");
        assert_eq!(ollama.selected_status, RuntimeProfileStatus::Preferred);

        let llamacpp = local_runtime_profile_report("local-qwen3.6", Some("llamacpp"));
        assert_eq!(llamacpp.selected_status, RuntimeProfileStatus::Experimental);
        assert_eq!(llamacpp.selected.requires, vec!["tool_probe".to_string()]);
        assert!(!llamacpp
            .selected
            .known_risks
            .contains(&"full_prompt_reprocess_on_hybrid_cache".to_string()));
    }

    #[test]
    fn qwen_llamacpp_profile_raises_context_when_accelerator_memory_fits() {
        let report = local_runtime_profile_report_for_host(
            Some("local-qwen3.6"),
            "qwen3.6-35b-a3b-ud-q4-k-xl",
            "llamacpp",
            Some(RuntimeProfileHost {
                system_available_gib: None,
                accelerator_free_gib: Some(32.0),
            }),
        );
        assert_eq!(report.selected.recommended_num_ctx, Some(262_144));
    }

    #[test]
    fn qwen_llamacpp_profile_keeps_conservative_context_when_memory_is_tight() {
        let report = local_runtime_profile_report_for_host(
            Some("local-qwen3.6"),
            "qwen3.6-35b-a3b-ud-q4-k-xl",
            "llamacpp",
            Some(RuntimeProfileHost {
                system_available_gib: None,
                accelerator_free_gib: Some(24.0),
            }),
        );
        assert_eq!(report.selected.recommended_num_ctx, Some(73_728));
    }

    #[test]
    fn gemma4_ollama_profile_is_quarantined_until_tool_probe_passes() {
        let report = local_runtime_profile_report("ollama-gemma4", None);
        assert_eq!(report.selected_status, RuntimeProfileStatus::Quarantined);
        let gate = evaluate_runtime_profile_gate(&report, &RuntimeProbeEvidence::new(), false);
        assert!(!gate.allowed);
        assert_eq!(gate.missing_required_probes, vec!["tool_probe".to_string()]);

        let mut evidence = RuntimeProbeEvidence::new();
        evidence.add_tool_report(classify_tool_conformance_fixture(
            "ollama",
            "gemma4:26b",
            ToolProbeMode::NonStreaming,
            "harn_tool_probe_marker",
            r#"{"content":"echo_marker({ value: \"harn_tool_probe_marker\" })"}"#,
        ));
        let gate = evaluate_runtime_profile_gate(&report, &evidence, false);
        assert!(gate.allowed, "{gate:?}");
    }

    #[test]
    fn force_allows_risky_profile_with_receipt() {
        let report = local_runtime_profile_report("local-qwen3.6", None);
        assert_eq!(report.selected_status, RuntimeProfileStatus::Experimental);
        let gate = evaluate_runtime_profile_gate(&report, &RuntimeProbeEvidence::new(), true);
        assert!(gate.allowed);
        assert!(gate.forced);
    }
}
