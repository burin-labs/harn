use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use super::{
    canonicalize_run, run_replay_oracle_trace, ReplayAllowlistRule, ReplayDivergence,
    ReplayExpectation, ReplayOracleError, ReplayOracleReport, ReplayOracleTrace, ReplayTraceRun,
    ReplayTraceRunCounts, REPLAY_TRACE_SCHEMA_VERSION,
};

pub const REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION: &str = "harn.replay_benchmark.report.v1";
pub const REPLAY_BENCHMARK_CLOUD_INGEST_KIND: &str = "harn_cloud.replay_determinism.leaderboard.v1";
pub const OPENCODE_JSONL_ADAPTER_ID: &str = "opencode-jsonl";
pub const OPENCODE_JSONL_ADAPTER_SCHEMA_VERSION: &str =
    "harn.replay_benchmark.adapter.opencode_jsonl.v1";

const REPLAY_TRACE_SECTIONS: [&str; 11] = [
    "event_log_entries",
    "trigger_firings",
    "llm_interactions",
    "protocol_interactions",
    "approval_interactions",
    "effect_receipts",
    "persona_runtime_states",
    "agent_transcript_deltas",
    "final_artifacts",
    "policy_decisions",
    // CH-07 (#1878): channel emit/match audit receipts.
    "channel_receipts",
];

const TOOL_DRIFT_SECTIONS: [&str; 3] = [
    "llm_interactions",
    "protocol_interactions",
    "effect_receipts",
];

const PERMISSION_SECTIONS: [&str; 2] = ["approval_interactions", "policy_decisions"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayBenchmarkReport {
    pub schema_version: String,
    pub cloud_ingest: ReplayBenchmarkCloudIngest,
    pub suite: ReplayBenchmarkSuiteIdentity,
    pub summary: ReplayBenchmarkSummary,
    pub fixtures: Vec<ReplayBenchmarkFixtureReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayBenchmarkCloudIngest {
    pub kind: String,
    pub leaderboard_key: String,
    pub report_schema_version: String,
    pub replay_trace_schema_version: String,
    pub artifact_contract: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayBenchmarkSuiteIdentity {
    pub name: String,
    pub fixture_count: usize,
    pub source_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayBenchmarkSummary {
    pub passed: usize,
    pub failed: usize,
    pub deterministic_fixtures: usize,
    pub drifted_fixtures: usize,
    pub mean_replay_fidelity_score: f64,
    pub mean_permission_decision_preservation_score: f64,
    pub tool_call_drift_count: usize,
    pub transcript_drift_count: usize,
    pub observed_interactions: usize,
    pub llm_input_tokens: u64,
    pub llm_output_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayBenchmarkFixtureReport {
    pub path: String,
    pub name: String,
    pub description: Option<String>,
    pub expectation: ReplayExpectation,
    pub passed: bool,
    pub deterministic: bool,
    pub first_run_counts: ReplayTraceRunCounts,
    pub second_run_counts: ReplayTraceRunCounts,
    pub metrics: ReplayBenchmarkMetrics,
    pub first_divergence: Option<ReplayDivergence>,
    pub receipt: ReplayBenchmarkFixtureReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayBenchmarkMetrics {
    pub determinism_score: f64,
    pub replay_fidelity_score: f64,
    pub permission_decision_preservation_score: f64,
    pub tool_call_drift_count: usize,
    pub transcript_drift_count: usize,
    pub runtime_cost: ReplayRuntimeCostMetrics,
    pub debugging_time_to_root_cause_proxy: ReplayDebuggingProxyMetrics,
    pub category_scores: BTreeMap<String, ReplayCategoryMetric>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayCategoryMetric {
    pub compared: bool,
    pub matched: bool,
    pub drift_count: usize,
    pub first_run_count: usize,
    pub second_run_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReplayRuntimeCostMetrics {
    pub observed_interactions: usize,
    pub event_log_entries: usize,
    pub trigger_firings: usize,
    pub llm_interactions: usize,
    pub protocol_interactions: usize,
    pub approval_interactions: usize,
    pub effect_receipts: usize,
    pub persona_runtime_states: usize,
    pub agent_transcript_deltas: usize,
    pub final_artifacts: usize,
    pub policy_decisions: usize,
    /// CH-07 (#1878): channel emit/match audit receipts.
    #[serde(default)]
    pub channel_receipts: usize,
    #[serde(default)]
    pub lifecycle_receipts: usize,
    pub llm_input_tokens: u64,
    pub llm_output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayDebuggingProxyMetrics {
    pub proxy_kind: String,
    pub first_divergence_path: Option<String>,
    pub first_divergence_depth: usize,
    pub drift_surface_count: usize,
    pub estimated_triage_steps: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayBenchmarkFixtureReceipt {
    pub ingest_kind: String,
    pub report_schema_version: String,
    pub replay_trace_schema_version: String,
    pub canonical_first_sha256: String,
    pub canonical_second_sha256: String,
    pub benchmark_receipt_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayBenchmarkError {
    Oracle(ReplayOracleError),
    Adapter(String),
    Serialization(String),
}

impl fmt::Display for ReplayBenchmarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oracle(error) => error.fmt(f),
            Self::Adapter(message) | Self::Serialization(message) => message.fmt(f),
        }
    }
}

impl std::error::Error for ReplayBenchmarkError {}

impl From<ReplayOracleError> for ReplayBenchmarkError {
    fn from(error: ReplayOracleError) -> Self {
        Self::Oracle(error)
    }
}

pub trait ReplayTraceAdapter {
    fn adapter_id(&self) -> &'static str;
    fn input_schema_version(&self) -> &'static str;
    fn adapt_run(&self, input: &str, run_id: &str) -> Result<ReplayTraceRun, ReplayBenchmarkError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenCodeJsonlAdapter;

impl ReplayTraceAdapter for OpenCodeJsonlAdapter {
    fn adapter_id(&self) -> &'static str {
        OPENCODE_JSONL_ADAPTER_ID
    }

    fn input_schema_version(&self) -> &'static str {
        OPENCODE_JSONL_ADAPTER_SCHEMA_VERSION
    }

    fn adapt_run(&self, input: &str, run_id: &str) -> Result<ReplayTraceRun, ReplayBenchmarkError> {
        adapt_opencode_jsonl(input, run_id)
    }
}

pub fn benchmark_replay_trace(
    path: impl Into<String>,
    trace: &ReplayOracleTrace,
) -> Result<ReplayBenchmarkFixtureReport, ReplayBenchmarkError> {
    let path = path.into();
    let oracle = run_replay_oracle_trace(trace)?;
    benchmark_replay_trace_from_oracle(path, trace, oracle)
}

pub fn benchmark_adapted_replay_pair(
    adapter: &dyn ReplayTraceAdapter,
    name: impl Into<String>,
    first_input: &str,
    second_input: &str,
) -> Result<ReplayBenchmarkFixtureReport, ReplayBenchmarkError> {
    let name = name.into();
    let trace = ReplayOracleTrace {
        schema_version: REPLAY_TRACE_SCHEMA_VERSION.to_string(),
        name: name.clone(),
        description: Some(format!(
            "External replay trace pair adapted with {} ({})",
            adapter.adapter_id(),
            adapter.input_schema_version()
        )),
        expect: ReplayExpectation::Match,
        allowlist: vec![ReplayAllowlistRule {
            path: "/run_id".to_string(),
            reason: "external trace runs are imported as separate executions".to_string(),
            replacement: None,
        }],
        first_run: adapter.adapt_run(first_input, "adapted_first_run")?,
        second_run: adapter.adapt_run(second_input, "adapted_second_run")?,
        protocol_fixture_refs: Vec::new(),
    };
    benchmark_replay_trace(format!("adapter:{}:{name}", adapter.adapter_id()), &trace)
}

pub fn build_replay_benchmark_report(
    suite_name: impl Into<String>,
    source_paths: Vec<String>,
    fixtures: Vec<ReplayBenchmarkFixtureReport>,
) -> ReplayBenchmarkReport {
    let suite_name = suite_name.into();
    let summary = summarize_replay_benchmark(&fixtures);
    ReplayBenchmarkReport {
        schema_version: REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION.to_string(),
        cloud_ingest: ReplayBenchmarkCloudIngest {
            kind: REPLAY_BENCHMARK_CLOUD_INGEST_KIND.to_string(),
            leaderboard_key: "replay-determinism".to_string(),
            report_schema_version: REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION.to_string(),
            replay_trace_schema_version: REPLAY_TRACE_SCHEMA_VERSION.to_string(),
            artifact_contract:
                "fixtures[].receipt + fixtures[].metrics are stable Cloud leaderboard inputs"
                    .to_string(),
        },
        suite: ReplayBenchmarkSuiteIdentity {
            name: suite_name,
            fixture_count: fixtures.len(),
            source_paths,
        },
        summary,
        fixtures,
    }
}

fn benchmark_replay_trace_from_oracle(
    path: String,
    trace: &ReplayOracleTrace,
    oracle: ReplayOracleReport,
) -> Result<ReplayBenchmarkFixtureReport, ReplayBenchmarkError> {
    let first = canonicalize_run(&trace.first_run, &trace.allowlist)?;
    let second = canonicalize_run(&trace.second_run, &trace.allowlist)?;
    let category_scores = category_scores(&first, &second, &oracle);
    let metrics = replay_metrics(trace, &oracle, category_scores)?;
    let canonical_first_sha256 = sha256_json(&first)?;
    let canonical_second_sha256 = sha256_json(&second)?;
    let receipt = fixture_receipt(
        &trace.name,
        &path,
        &metrics,
        &canonical_first_sha256,
        &canonical_second_sha256,
    )?;

    Ok(ReplayBenchmarkFixtureReport {
        path,
        name: oracle.name,
        description: trace.description.clone(),
        expectation: oracle.expectation,
        passed: oracle.passed,
        deterministic: oracle.divergence.is_none(),
        first_run_counts: oracle.first_run_counts,
        second_run_counts: oracle.second_run_counts,
        metrics,
        first_divergence: oracle.divergence,
        receipt,
    })
}

fn summarize_replay_benchmark(fixtures: &[ReplayBenchmarkFixtureReport]) -> ReplayBenchmarkSummary {
    let fixture_count = fixtures.len();
    let passed = fixtures.iter().filter(|fixture| fixture.passed).count();
    let deterministic_fixtures = fixtures
        .iter()
        .filter(|fixture| fixture.deterministic)
        .count();
    let runtime = fixtures
        .iter()
        .fold(ReplayRuntimeCostMetrics::default(), |mut acc, fixture| {
            let runtime = &fixture.metrics.runtime_cost;
            acc.observed_interactions += runtime.observed_interactions;
            acc.event_log_entries += runtime.event_log_entries;
            acc.trigger_firings += runtime.trigger_firings;
            acc.llm_interactions += runtime.llm_interactions;
            acc.protocol_interactions += runtime.protocol_interactions;
            acc.approval_interactions += runtime.approval_interactions;
            acc.effect_receipts += runtime.effect_receipts;
            acc.persona_runtime_states += runtime.persona_runtime_states;
            acc.agent_transcript_deltas += runtime.agent_transcript_deltas;
            acc.final_artifacts += runtime.final_artifacts;
            acc.policy_decisions += runtime.policy_decisions;
            acc.channel_receipts += runtime.channel_receipts;
            acc.llm_input_tokens += runtime.llm_input_tokens;
            acc.llm_output_tokens += runtime.llm_output_tokens;
            acc.observed_cost_usd =
                sum_optional_cost(acc.observed_cost_usd, runtime.observed_cost_usd);
            acc
        });
    ReplayBenchmarkSummary {
        passed,
        failed: fixture_count.saturating_sub(passed),
        deterministic_fixtures,
        drifted_fixtures: fixture_count.saturating_sub(deterministic_fixtures),
        mean_replay_fidelity_score: average_metric(fixtures, |fixture| {
            fixture.metrics.replay_fidelity_score
        }),
        mean_permission_decision_preservation_score: average_metric(fixtures, |fixture| {
            fixture.metrics.permission_decision_preservation_score
        }),
        tool_call_drift_count: fixtures
            .iter()
            .map(|fixture| fixture.metrics.tool_call_drift_count)
            .sum(),
        transcript_drift_count: fixtures
            .iter()
            .map(|fixture| fixture.metrics.transcript_drift_count)
            .sum(),
        observed_interactions: runtime.observed_interactions,
        llm_input_tokens: runtime.llm_input_tokens,
        llm_output_tokens: runtime.llm_output_tokens,
    }
}

fn replay_metrics(
    trace: &ReplayOracleTrace,
    oracle: &ReplayOracleReport,
    category_scores: BTreeMap<String, ReplayCategoryMetric>,
) -> Result<ReplayBenchmarkMetrics, ReplayBenchmarkError> {
    let compared_categories = category_scores
        .values()
        .filter(|metric| metric.compared)
        .count();
    let matched_categories = category_scores
        .values()
        .filter(|metric| metric.compared && metric.matched)
        .count();
    let replay_fidelity_score = if compared_categories == 0 {
        0.0
    } else {
        matched_categories as f64 / compared_categories as f64
    };
    let permission_decision_preservation_score =
        section_score(&category_scores, &PERMISSION_SECTIONS);
    let tool_call_drift_count = section_drift_count(&category_scores, &TOOL_DRIFT_SECTIONS);
    let transcript_drift_count =
        section_drift_count(&category_scores, &["agent_transcript_deltas"]);
    let runtime_cost = runtime_cost_metrics(&trace.first_run, &trace.second_run);
    let debugging_time_to_root_cause_proxy =
        debugging_proxy_metrics(oracle.divergence.as_ref(), &category_scores);

    Ok(ReplayBenchmarkMetrics {
        determinism_score: if oracle.divergence.is_none() {
            1.0
        } else {
            0.0
        },
        replay_fidelity_score,
        permission_decision_preservation_score,
        tool_call_drift_count,
        transcript_drift_count,
        runtime_cost,
        debugging_time_to_root_cause_proxy,
        category_scores,
    })
}

fn category_scores(
    first: &JsonValue,
    second: &JsonValue,
    oracle: &ReplayOracleReport,
) -> BTreeMap<String, ReplayCategoryMetric> {
    let first_counts = counts_by_section(&oracle.first_run_counts);
    let second_counts = counts_by_section(&oracle.second_run_counts);
    REPLAY_TRACE_SECTIONS
        .iter()
        .map(|section| {
            let first_value = first.get(*section).unwrap_or(&JsonValue::Null);
            let second_value = second.get(*section).unwrap_or(&JsonValue::Null);
            let first_run_count = first_counts.get(*section).copied().unwrap_or_default();
            let second_run_count = second_counts.get(*section).copied().unwrap_or_default();
            let compared = first_run_count > 0 || second_run_count > 0;
            let drift_count = if compared {
                drift_count(first_value, second_value)
            } else {
                0
            };
            (
                (*section).to_string(),
                ReplayCategoryMetric {
                    compared,
                    matched: drift_count == 0,
                    drift_count,
                    first_run_count,
                    second_run_count,
                },
            )
        })
        .collect()
}

fn counts_by_section(counts: &ReplayTraceRunCounts) -> BTreeMap<&'static str, usize> {
    BTreeMap::from([
        ("event_log_entries", counts.event_log_entries),
        ("trigger_firings", counts.trigger_firings),
        ("llm_interactions", counts.llm_interactions),
        ("protocol_interactions", counts.protocol_interactions),
        ("approval_interactions", counts.approval_interactions),
        ("effect_receipts", counts.effect_receipts),
        ("persona_runtime_states", counts.persona_runtime_states),
        ("agent_transcript_deltas", counts.agent_transcript_deltas),
        ("final_artifacts", counts.final_artifacts),
        ("policy_decisions", counts.policy_decisions),
        // CH-07 (#1878).
        ("channel_receipts", counts.channel_receipts),
        ("lifecycle_receipts", counts.lifecycle_receipts),
    ])
}

fn drift_count(first: &JsonValue, second: &JsonValue) -> usize {
    if first == second {
        return 0;
    }
    match (first, second) {
        (JsonValue::Array(first_items), JsonValue::Array(second_items)) => {
            let shared = first_items.len().min(second_items.len());
            let item_drifts = (0..shared)
                .filter(|index| first_items[*index] != second_items[*index])
                .count();
            item_drifts + first_items.len().abs_diff(second_items.len())
        }
        (JsonValue::Object(first_map), JsonValue::Object(second_map)) => {
            let keys = first_map
                .keys()
                .chain(second_map.keys())
                .collect::<BTreeSet<_>>();
            keys.into_iter()
                .filter(|key| first_map.get(*key) != second_map.get(*key))
                .count()
        }
        _ => 1,
    }
}

fn section_score(
    category_scores: &BTreeMap<String, ReplayCategoryMetric>,
    sections: &[&str],
) -> f64 {
    let compared = sections
        .iter()
        .filter_map(|section| category_scores.get(*section))
        .filter(|metric| metric.compared)
        .collect::<Vec<_>>();
    if compared.is_empty() {
        return 1.0;
    }
    compared.iter().filter(|metric| metric.matched).count() as f64 / compared.len() as f64
}

fn section_drift_count(
    category_scores: &BTreeMap<String, ReplayCategoryMetric>,
    sections: &[&str],
) -> usize {
    sections
        .iter()
        .filter_map(|section| category_scores.get(*section))
        .map(|metric| metric.drift_count)
        .sum()
}

fn runtime_cost_metrics(
    first_run: &ReplayTraceRun,
    second_run: &ReplayTraceRun,
) -> ReplayRuntimeCostMetrics {
    let first = first_run.counts();
    let second = second_run.counts();
    let observed_cost_usd =
        sum_optional_cost(cost_usd_for_run(first_run), cost_usd_for_run(second_run));
    ReplayRuntimeCostMetrics {
        observed_interactions: trace_material_count(&first) + trace_material_count(&second),
        event_log_entries: first.event_log_entries + second.event_log_entries,
        trigger_firings: first.trigger_firings + second.trigger_firings,
        llm_interactions: first.llm_interactions + second.llm_interactions,
        protocol_interactions: first.protocol_interactions + second.protocol_interactions,
        approval_interactions: first.approval_interactions + second.approval_interactions,
        effect_receipts: first.effect_receipts + second.effect_receipts,
        persona_runtime_states: first.persona_runtime_states + second.persona_runtime_states,
        agent_transcript_deltas: first.agent_transcript_deltas + second.agent_transcript_deltas,
        final_artifacts: first.final_artifacts + second.final_artifacts,
        policy_decisions: first.policy_decisions + second.policy_decisions,
        channel_receipts: first.channel_receipts + second.channel_receipts,
        lifecycle_receipts: first.lifecycle_receipts + second.lifecycle_receipts,
        llm_input_tokens: token_total(first_run, "input_tokens")
            + token_total(second_run, "input_tokens"),
        llm_output_tokens: token_total(first_run, "output_tokens")
            + token_total(second_run, "output_tokens"),
        observed_cost_usd,
    }
}

fn trace_material_count(counts: &ReplayTraceRunCounts) -> usize {
    counts.event_log_entries
        + counts.trigger_firings
        + counts.llm_interactions
        + counts.protocol_interactions
        + counts.approval_interactions
        + counts.effect_receipts
        + counts.persona_runtime_states
        + counts.agent_transcript_deltas
        + counts.final_artifacts
        + counts.policy_decisions
        + counts.channel_receipts
        + counts.lifecycle_receipts
}

fn token_total(run: &ReplayTraceRun, token_key: &str) -> u64 {
    run.llm_interactions
        .iter()
        .filter_map(|interaction| {
            interaction
                .get(token_key)
                .and_then(JsonValue::as_u64)
                .or_else(|| {
                    interaction
                        .get("usage")
                        .and_then(|usage| usage.get(token_key))
                        .and_then(JsonValue::as_u64)
                })
        })
        .sum()
}

fn cost_usd_for_run(run: &ReplayTraceRun) -> Option<f64> {
    let mut seen = false;
    let mut total = 0.0;
    for interaction in &run.llm_interactions {
        if let Some(cost) = interaction
            .get("cost_usd")
            .and_then(JsonValue::as_f64)
            .or_else(|| {
                interaction
                    .get("usage")
                    .and_then(|usage| usage.get("cost_usd"))
                    .and_then(JsonValue::as_f64)
            })
        {
            seen = true;
            total += cost;
        }
    }
    seen.then_some(total)
}

fn sum_optional_cost(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn debugging_proxy_metrics(
    divergence: Option<&ReplayDivergence>,
    category_scores: &BTreeMap<String, ReplayCategoryMetric>,
) -> ReplayDebuggingProxyMetrics {
    let first_divergence_path = divergence.map(|divergence| divergence.path.clone());
    let first_divergence_depth = first_divergence_path
        .as_deref()
        .map(json_path_depth)
        .unwrap_or_default();
    let drift_surface_count = category_scores
        .values()
        .filter(|metric| metric.compared && !metric.matched)
        .count();
    ReplayDebuggingProxyMetrics {
        proxy_kind: "first_divergence_depth_plus_drift_surfaces".to_string(),
        first_divergence_path,
        first_divergence_depth,
        drift_surface_count,
        estimated_triage_steps: if drift_surface_count == 0 {
            0
        } else {
            1 + first_divergence_depth + drift_surface_count
        },
    }
}

fn json_path_depth(path: &str) -> usize {
    let path = path.trim();
    if path == "$" {
        return 0;
    }
    if let Some(pointer_path) = path.strip_prefix('/') {
        return pointer_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count();
    }
    path.split('.')
        .filter(|segment| !segment.is_empty() && *segment != "$")
        .count()
}

fn fixture_receipt(
    name: &str,
    path: &str,
    metrics: &ReplayBenchmarkMetrics,
    canonical_first_sha256: &str,
    canonical_second_sha256: &str,
) -> Result<ReplayBenchmarkFixtureReceipt, ReplayBenchmarkError> {
    let receipt_material = json!({
        "ingest_kind": REPLAY_BENCHMARK_CLOUD_INGEST_KIND,
        "report_schema_version": REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION,
        "replay_trace_schema_version": REPLAY_TRACE_SCHEMA_VERSION,
        "name": name,
        "path": path,
        "canonical_first_sha256": canonical_first_sha256,
        "canonical_second_sha256": canonical_second_sha256,
        "metrics": metrics,
    });
    Ok(ReplayBenchmarkFixtureReceipt {
        ingest_kind: REPLAY_BENCHMARK_CLOUD_INGEST_KIND.to_string(),
        report_schema_version: REPLAY_BENCHMARK_REPORT_SCHEMA_VERSION.to_string(),
        replay_trace_schema_version: REPLAY_TRACE_SCHEMA_VERSION.to_string(),
        canonical_first_sha256: canonical_first_sha256.to_string(),
        canonical_second_sha256: canonical_second_sha256.to_string(),
        benchmark_receipt_sha256: sha256_json(&receipt_material)?,
    })
}

fn sha256_json(value: &JsonValue) -> Result<String, ReplayBenchmarkError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| ReplayBenchmarkError::Serialization(error.to_string()))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn sha256_value(value: &JsonValue) -> Result<String, ReplayBenchmarkError> {
    sha256_json(value)
}

fn sha256_text(text: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
}

fn average_metric(
    fixtures: &[ReplayBenchmarkFixtureReport],
    metric: impl Fn(&ReplayBenchmarkFixtureReport) -> f64,
) -> f64 {
    if fixtures.is_empty() {
        0.0
    } else {
        fixtures.iter().map(metric).sum::<f64>() / fixtures.len() as f64
    }
}

fn adapt_opencode_jsonl(input: &str, run_id: &str) -> Result<ReplayTraceRun, ReplayBenchmarkError> {
    let mut run = ReplayTraceRun {
        run_id: run_id.to_string(),
        ..ReplayTraceRun::default()
    };
    for (index, raw_line) in input.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let value: JsonValue = serde_json::from_str(line).map_err(|error| {
            ReplayBenchmarkError::Adapter(format!(
                "invalid {OPENCODE_JSONL_ADAPTER_ID} JSONL line {line_no}: {error}"
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            ReplayBenchmarkError::Adapter(format!(
                "{OPENCODE_JSONL_ADAPTER_ID} JSONL line {line_no} must be an object"
            ))
        })?;
        let event_type = object
            .get("type")
            .or_else(|| object.get("event"))
            .and_then(JsonValue::as_str)
            .unwrap_or("event");
        match event_type {
            "message" | "session.message" => {
                run.agent_transcript_deltas
                    .push(adapt_opencode_message(object, line_no));
            }
            "tool_call" | "tool" | "session.tool_call" => {
                let (protocol, receipt) = adapt_opencode_tool_call(object, line_no)?;
                run.protocol_interactions.push(protocol);
                run.effect_receipts.push(receipt);
            }
            "permission" | "permission_decision" | "session.permission" => {
                let (approval, policy) = adapt_opencode_permission(object, line_no);
                run.approval_interactions.push(approval);
                run.policy_decisions.push(policy);
            }
            "llm" | "model" | "session.llm" => {
                run.llm_interactions
                    .push(adapt_opencode_llm(object, line_no));
            }
            _ => run
                .event_log_entries
                .push(adapt_opencode_event(object, event_type, line_no)),
        }
    }
    if trace_material_count(&run.counts()) == 0 {
        return Err(ReplayBenchmarkError::Adapter(format!(
            "{OPENCODE_JSONL_ADAPTER_ID} input contained no adaptable events"
        )));
    }
    Ok(run)
}

fn adapt_opencode_message(
    object: &serde_json::Map<String, JsonValue>,
    line_no: usize,
) -> JsonValue {
    let content = object.get("content").cloned().unwrap_or(JsonValue::Null);
    json!({
        "delta_id": object_string(object, "id").unwrap_or_else(|| format!("message-{line_no}")),
        "agent": object_string(object, "agent").unwrap_or_else(|| "opencode".to_string()),
        "role": object_string(object, "role").unwrap_or_else(|| "assistant".to_string()),
        "content_sha256": sha256_text(&content.to_string()),
    })
}

fn adapt_opencode_tool_call(
    object: &serde_json::Map<String, JsonValue>,
    line_no: usize,
) -> Result<(JsonValue, JsonValue), ReplayBenchmarkError> {
    let tool = object_string(object, "tool")
        .or_else(|| object_string(object, "name"))
        .unwrap_or_else(|| "unknown_tool".to_string());
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("args"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = object
        .get("result")
        .or_else(|| object.get("output"))
        .cloned()
        .unwrap_or(JsonValue::Null);
    let status = object_string(object, "status").unwrap_or_else(|| "completed".to_string());
    let arguments_sha256 = sha256_value(&arguments)?;
    let result_sha256 = sha256_value(&result)?;
    Ok((
        json!({
            "protocol": "opencode",
            "boundary": "tool_call",
            "tool": tool,
            "call_id": object_string(object, "id").unwrap_or_else(|| format!("tool-{line_no}")),
            "arguments_sha256": arguments_sha256,
            "status": status,
            "result_sha256": result_sha256,
        }),
        json!({
            "receipt_id": object_string(object, "receipt_id").unwrap_or_else(|| format!("tool-receipt-{line_no}")),
            "kind": "tool_call",
            "tool": tool,
            "status": status,
            "arguments_sha256": arguments_sha256,
            "result_sha256": result_sha256,
        }),
    ))
}

fn adapt_opencode_permission(
    object: &serde_json::Map<String, JsonValue>,
    line_no: usize,
) -> (JsonValue, JsonValue) {
    let action = object_string(object, "action").unwrap_or_else(|| "unknown".to_string());
    let decision = object_string(object, "decision")
        .or_else(|| object_string(object, "response"))
        .unwrap_or_else(|| "unknown".to_string());
    (
        json!({
            "request_id": object_string(object, "id").unwrap_or_else(|| format!("permission-{line_no}")),
            "principal": object_string(object, "principal").unwrap_or_else(|| "agent".to_string()),
            "action": action,
            "response": decision,
            "reviewer": object.get("reviewer").cloned().unwrap_or(JsonValue::Null),
        }),
        json!({
            "decision_id": object_string(object, "decision_id").unwrap_or_else(|| format!("policy-{line_no}")),
            "capability": object_string(object, "capability").unwrap_or(action),
            "decision": decision,
            "approval_required": true,
        }),
    )
}

fn adapt_opencode_llm(object: &serde_json::Map<String, JsonValue>, line_no: usize) -> JsonValue {
    let input_tokens = object
        .get("input_tokens")
        .and_then(JsonValue::as_u64)
        .or_else(|| {
            object
                .get("usage")
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(JsonValue::as_u64)
        })
        .unwrap_or_default();
    let output_tokens = object
        .get("output_tokens")
        .and_then(JsonValue::as_u64)
        .or_else(|| {
            object
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(JsonValue::as_u64)
        })
        .unwrap_or_default();
    let messages_sha256 = object
        .get("messages")
        .map(|value| sha256_text(&value.to_string()))
        .unwrap_or_else(|| sha256_text(""));
    let response_sha256 = object
        .get("response")
        .map(|value| sha256_text(&value.to_string()))
        .unwrap_or_else(|| sha256_text(""));
    json!({
        "request_id": object_string(object, "id").unwrap_or_else(|| format!("llm-{line_no}")),
        "provider": object_string(object, "provider").unwrap_or_else(|| "opencode".to_string()),
        "model": object_string(object, "model").unwrap_or_else(|| "unknown".to_string()),
        "messages_sha256": messages_sha256,
        "response_sha256": response_sha256,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        },
    })
}

fn adapt_opencode_event(
    object: &serde_json::Map<String, JsonValue>,
    event_type: &str,
    line_no: usize,
) -> JsonValue {
    json!({
        "event_id": line_no,
        "topic": object_string(object, "topic").unwrap_or_else(|| "opencode.session".to_string()),
        "kind": event_type,
        "payload": object.get("payload").cloned().unwrap_or_else(|| JsonValue::Object(object.clone())),
    })
}

fn object_string(object: &serde_json::Map<String, JsonValue>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_pair(status: (&str, &str)) -> ReplayOracleTrace {
        ReplayOracleTrace {
            schema_version: REPLAY_TRACE_SCHEMA_VERSION.to_string(),
            name: "simple_tool_run".to_string(),
            description: Some("golden replay benchmark fixture".to_string()),
            expect: ReplayExpectation::Match,
            allowlist: vec![ReplayAllowlistRule {
                path: "/run_id".to_string(),
                reason: "run ids are allocated per execution".to_string(),
                replacement: None,
            }],
            first_run: ReplayTraceRun {
                run_id: "first".to_string(),
                protocol_interactions: vec![json!({
                    "protocol": "mcp",
                    "boundary": "tools/call",
                    "tool": "read_file",
                    "status": status.0,
                })],
                policy_decisions: vec![json!({
                    "capability": "fs.read",
                    "decision": "allow",
                })],
                ..ReplayTraceRun::default()
            },
            second_run: ReplayTraceRun {
                run_id: "second".to_string(),
                protocol_interactions: vec![json!({
                    "protocol": "mcp",
                    "boundary": "tools/call",
                    "tool": "read_file",
                    "status": status.1,
                })],
                policy_decisions: vec![json!({
                    "capability": "fs.read",
                    "decision": "allow",
                })],
                ..ReplayTraceRun::default()
            },
            protocol_fixture_refs: Vec::new(),
        }
    }

    #[test]
    fn replay_benchmark_reports_stable_golden_metrics_for_matching_trace() {
        let fixture =
            benchmark_replay_trace("benchmarks/replay/simple.json", &trace_pair(("ok", "ok")))
                .expect("benchmark fixture");

        assert!(fixture.passed);
        assert!(fixture.deterministic);
        assert_eq!(fixture.metrics.determinism_score, 1.0);
        assert_eq!(fixture.metrics.replay_fidelity_score, 1.0);
        assert_eq!(fixture.metrics.permission_decision_preservation_score, 1.0);
        assert_eq!(fixture.metrics.tool_call_drift_count, 0);
        assert!(fixture
            .receipt
            .benchmark_receipt_sha256
            .starts_with("sha256:"));
    }

    #[test]
    fn replay_benchmark_reports_reduced_fidelity_for_meaningful_drift() {
        let fixture =
            benchmark_replay_trace("benchmarks/replay/drift.json", &trace_pair(("ok", "error")))
                .expect("benchmark fixture");

        assert!(!fixture.passed);
        assert!(!fixture.deterministic);
        assert_eq!(fixture.metrics.determinism_score, 0.0);
        assert_eq!(fixture.metrics.replay_fidelity_score, 0.5);
        assert_eq!(fixture.metrics.tool_call_drift_count, 1);
        assert_eq!(
            fixture
                .metrics
                .debugging_time_to_root_cause_proxy
                .first_divergence_path
                .as_deref(),
            Some("/protocol_interactions/0/status")
        );
        assert_eq!(
            fixture
                .metrics
                .debugging_time_to_root_cause_proxy
                .first_divergence_depth,
            3
        );
        assert_eq!(
            fixture
                .metrics
                .debugging_time_to_root_cause_proxy
                .estimated_triage_steps,
            5
        );
    }

    #[test]
    fn replay_benchmark_summary_is_stable_across_repeated_runs() {
        let first = benchmark_replay_trace("fixture.json", &trace_pair(("ok", "ok")))
            .expect("first benchmark");
        let second = benchmark_replay_trace("fixture.json", &trace_pair(("ok", "ok")))
            .expect("second benchmark");

        let first_json = serde_json::to_string(&first).expect("serialize first");
        let second_json = serde_json::to_string(&second).expect("serialize second");
        assert_eq!(first_json, second_json);
    }

    #[test]
    fn opencode_jsonl_adapter_maps_messages_tools_permissions_and_llm_usage() {
        let input = concat!(
            "{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"content\":\"done\"}\n",
            "{\"type\":\"tool_call\",\"id\":\"t1\",\"tool\":\"write_file\",\"arguments\":{\"path\":\"notes.md\"},\"result\":{\"ok\":true}}\n",
            "{\"type\":\"permission\",\"id\":\"p1\",\"action\":\"write_file\",\"decision\":\"approved\"}\n",
            "{\"type\":\"llm\",\"id\":\"l1\",\"model\":\"qwen\",\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}\n"
        );

        let run = OpenCodeJsonlAdapter
            .adapt_run(input, "opencode-run")
            .expect("adapt opencode jsonl");

        assert_eq!(run.run_id, "opencode-run");
        assert_eq!(run.agent_transcript_deltas.len(), 1);
        assert_eq!(run.protocol_interactions.len(), 1);
        assert_eq!(run.effect_receipts.len(), 1);
        assert_eq!(run.approval_interactions.len(), 1);
        assert_eq!(run.policy_decisions.len(), 1);
        assert_eq!(run.llm_interactions.len(), 1);
        assert_eq!(token_total(&run, "input_tokens"), 7);
        assert_eq!(token_total(&run, "output_tokens"), 3);
    }

    #[test]
    fn adapted_trace_pair_can_be_benchmarked() {
        let first = "{\"type\":\"tool_call\",\"tool\":\"read_file\",\"result\":{\"ok\":true}}\n";
        let second = "{\"type\":\"tool_call\",\"tool\":\"read_file\",\"result\":{\"ok\":true}}\n";

        let fixture = benchmark_adapted_replay_pair(
            &OpenCodeJsonlAdapter,
            "external-tool-run",
            first,
            second,
        )
        .expect("benchmark adapted pair");

        assert!(fixture.passed);
        assert_eq!(fixture.name, "external-tool-run");
        assert_eq!(fixture.metrics.tool_call_drift_count, 0);
    }
}
