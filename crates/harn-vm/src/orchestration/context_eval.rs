//! Deterministic context-engineering eval primitives.
//!
//! The runner measures whether a task has enough projected context, transcript
//! state, and tool disclosure to be answerable under a named context mode. It
//! intentionally does not call an LLM by default; live/model preprocessing can
//! be layered on top by hosts while keeping this report shape stable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::{
    assemble_context, estimate_chunk_tokens, render_assembled_chunks, ArtifactRecord,
    AssembleDedup, AssembleOptions, AssembleStrategy,
};
use crate::value::VmError;

pub const CONTEXT_EVAL_SCHEMA_VERSION: u32 = 1;
pub const CONTEXT_EVAL_MANIFEST_TYPE: &str = "harn.context_eval.manifest.v1";
pub const CONTEXT_EVAL_REPORT_TYPE: &str = "harn.context_eval.report.v1";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub modes: Vec<ContextEvalMode>,
    pub tasks: Vec<ContextEvalTask>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalMode {
    pub id: String,
    pub name: Option<String>,
    pub kind: String,
    pub description: Option<String>,
    #[serde(default, alias = "artifact-ids")]
    pub artifact_ids: Vec<String>,
    #[serde(default, alias = "include-artifact-kinds")]
    pub include_artifact_kinds: Vec<String>,
    #[serde(default, alias = "exclude-artifact-kinds")]
    pub exclude_artifact_kinds: Vec<String>,
    #[serde(default, alias = "budget-tokens")]
    pub budget_tokens: Option<usize>,
    #[serde(default, alias = "assemble-strategy")]
    pub assemble_strategy: Option<String>,
    pub dedup: Option<String>,
    #[serde(default, alias = "microcompact-threshold")]
    pub microcompact_threshold: Option<usize>,
    #[serde(default, alias = "semantic-overlap")]
    pub semantic_overlap: Option<f64>,
    #[serde(default, alias = "projection-policy")]
    pub projection_policy: Option<String>,
    #[serde(default, alias = "transcript-keep-last")]
    pub transcript_keep_last: Option<usize>,
    #[serde(default, alias = "tool-disclosure")]
    pub tool_disclosure: Option<String>,
    #[serde(default, alias = "tool-allowlist")]
    pub tool_allowlist: Vec<String>,
    #[serde(default, alias = "expected-cache-hit")]
    pub expected_cache_hit: Option<bool>,
    #[serde(default, alias = "cache-namespace")]
    pub cache_namespace: Option<String>,
    #[serde(default, alias = "compaction-policy")]
    pub compaction_policy: Option<JsonValue>,
    pub preprocessing: Option<String>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalTask {
    pub id: String,
    pub name: Option<String>,
    pub objective: String,
    #[serde(default, alias = "reference-answer")]
    pub reference_answer: Option<String>,
    pub artifacts: Vec<ArtifactRecord>,
    pub transcript: Vec<ContextEvalTranscriptMessage>,
    pub tools: Vec<ContextEvalTool>,
    #[serde(default, alias = "tool-events")]
    pub tool_events: Vec<ContextEvalToolEvent>,
    pub expected: ContextEvalExpected,
    pub observed: ContextEvalObserved,
    #[serde(default, alias = "mode-observations")]
    pub mode_observations: BTreeMap<String, ContextEvalObserved>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalTranscriptMessage {
    pub role: String,
    pub content: String,
    #[serde(default, alias = "estimated-tokens")]
    pub estimated_tokens: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalTool {
    pub name: String,
    pub description: Option<String>,
    pub capability: Option<String>,
    pub deterministic: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalToolEvent {
    pub order: Option<usize>,
    pub name: String,
    pub phase: Option<String>,
    pub success: Option<bool>,
    pub quality: Option<String>,
    pub recovery: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalExpected {
    #[serde(default, alias = "required-terms")]
    pub required_terms: Vec<String>,
    #[serde(default, alias = "expected-artifact-ids")]
    pub expected_artifact_ids: Vec<String>,
    #[serde(default, alias = "expected-tools")]
    pub expected_tools: Vec<String>,
    #[serde(default, alias = "max-input-tokens")]
    pub max_input_tokens: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalObserved {
    #[serde(default, alias = "final-response")]
    pub final_response: Option<String>,
    #[serde(default, alias = "latency-ms")]
    pub latency_ms: Option<u64>,
    #[serde(default, alias = "input-tokens")]
    pub input_tokens: Option<usize>,
    #[serde(default, alias = "output-tokens")]
    pub output_tokens: Option<usize>,
    #[serde(default, alias = "cost-usd")]
    pub cost_usd: Option<f64>,
    #[serde(default, alias = "cache-hit")]
    pub cache_hit: Option<bool>,
    #[serde(default, alias = "compaction-count")]
    pub compaction_count: Option<usize>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalReport {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: u32,
    pub manifest_id: String,
    pub manifest_name: Option<String>,
    pub pass: bool,
    pub total_runs: usize,
    pub passed_runs: usize,
    pub failed_runs: usize,
    pub total_tasks: usize,
    pub total_modes: usize,
    pub aggregate: ContextEvalAggregate,
    pub modes: Vec<ContextEvalModeSummary>,
    pub tasks: Vec<ContextEvalTaskSummary>,
    pub runs: Vec<ContextEvalRunReport>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalAggregate {
    pub mean_final_correctness: f64,
    pub mean_tool_call_quality: f64,
    pub total_latency_ms: u64,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cost_usd: f64,
    pub total_compaction_count: usize,
    pub total_error_recovery_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalModeSummary {
    pub id: String,
    pub kind: String,
    pub projection_policy: String,
    pub tool_disclosure: String,
    pub preprocessing: ContextEvalPreprocessing,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalPreprocessing {
    pub mode: String,
    pub llm_enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalTaskSummary {
    pub id: String,
    pub name: Option<String>,
    pub required_terms: Vec<String>,
    pub expected_artifact_ids: Vec<String>,
    pub expected_tools: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalRunReport {
    pub run_id: String,
    pub task_id: String,
    pub mode_id: String,
    pub mode_kind: String,
    pub status: String,
    pub passed: bool,
    pub final_correctness: ContextEvalCorrectness,
    pub reads_before_first_edit: usize,
    pub tool_call_quality: ContextEvalToolQuality,
    pub latency_ms: u64,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cost_usd: f64,
    pub compaction_count: usize,
    pub projection: ContextEvalProjectionReport,
    pub context: ContextEvalContextReport,
    pub cache: ContextEvalCacheReport,
    pub error_recovery_count: usize,
    pub preprocessing: ContextEvalPreprocessing,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalCorrectness {
    pub passed: bool,
    pub score: f64,
    pub required_terms_present: Vec<String>,
    pub required_terms_missing: Vec<String>,
    pub expected_artifact_ids_present: Vec<String>,
    pub expected_artifact_ids_missing: Vec<String>,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextEvalToolQuality {
    pub score: f64,
    pub expected_tools: Vec<String>,
    pub observed_tools: Vec<String>,
    pub matched_tools: Vec<String>,
    pub missing_tools: Vec<String>,
    pub unnecessary_tools: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalProjectionReport {
    pub policy: String,
    pub source_message_count: usize,
    pub retained_message_count: usize,
    pub retained_tokens: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalContextReport {
    pub projection_policy: String,
    pub tool_disclosure: String,
    pub artifact_count: usize,
    pub selected_artifact_ids: Vec<String>,
    pub dropped_artifact_ids: Vec<String>,
    pub rendered_bytes: usize,
    pub rendered_tokens: usize,
    pub budget_tokens: usize,
    pub assemble_strategy: String,
    pub dedup: String,
    pub exposed_tools: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextEvalCacheReport {
    pub namespace: String,
    pub key: String,
    pub stable_input_hash: String,
    pub deterministic_order: bool,
    pub hit: Option<bool>,
}

struct PreparedModeRun {
    artifacts: Vec<ArtifactRecord>,
    rendered_context: String,
    selected_artifact_ids: Vec<String>,
    dropped_artifact_ids: Vec<String>,
    projection: ContextEvalProjectionReport,
    transcript_text: String,
    exposed_tools: Vec<String>,
    visible_tool_events: Vec<ContextEvalToolEvent>,
}

pub fn load_context_eval_manifest(path: &Path) -> Result<ContextEvalManifest, VmError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!("failed to read context eval manifest: {error}"))
    })?;
    let mut manifest: ContextEvalManifest =
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            toml::from_str(&content).map_err(|error| {
                VmError::Runtime(format!("failed to parse context eval TOML: {error}"))
            })?
        } else {
            serde_json::from_str(&content).map_err(|error| {
                VmError::Runtime(format!("failed to parse context eval JSON: {error}"))
            })?
        };
    normalize_context_eval_manifest(&mut manifest)?;
    Ok(manifest)
}

pub fn evaluate_context_eval_manifest(
    manifest: &ContextEvalManifest,
) -> Result<ContextEvalReport, VmError> {
    let mut manifest = manifest.clone();
    normalize_context_eval_manifest(&mut manifest)?;

    let modes = manifest
        .modes
        .iter()
        .map(|mode| ContextEvalModeSummary {
            id: mode.id.clone(),
            kind: mode_kind(mode),
            projection_policy: projection_policy(mode),
            tool_disclosure: tool_disclosure(mode),
            preprocessing: preprocessing_report(mode),
        })
        .collect::<Vec<_>>();
    let tasks = manifest
        .tasks
        .iter()
        .map(|task| ContextEvalTaskSummary {
            id: task.id.clone(),
            name: task.name.clone(),
            required_terms: sorted_strings(&task.expected.required_terms),
            expected_artifact_ids: sorted_strings(&task.expected.expected_artifact_ids),
            expected_tools: sorted_strings(&task.expected.expected_tools),
        })
        .collect::<Vec<_>>();

    let mut runs = Vec::new();
    for task in &manifest.tasks {
        for mode in &manifest.modes {
            runs.push(evaluate_task_mode(task, mode)?);
        }
    }

    let total_runs = runs.len();
    let passed_runs = runs.iter().filter(|run| run.passed).count();
    let failed_runs = total_runs.saturating_sub(passed_runs);
    let aggregate = aggregate_runs(&runs);
    Ok(ContextEvalReport {
        type_name: CONTEXT_EVAL_REPORT_TYPE.to_string(),
        schema_version: CONTEXT_EVAL_SCHEMA_VERSION,
        manifest_id: manifest.id,
        manifest_name: manifest.name,
        pass: failed_runs == 0,
        total_runs,
        passed_runs,
        failed_runs,
        total_tasks: tasks.len(),
        total_modes: modes.len(),
        aggregate,
        modes,
        tasks,
        runs,
        metadata: manifest.metadata,
    })
}

fn normalize_context_eval_manifest(manifest: &mut ContextEvalManifest) -> Result<(), VmError> {
    if manifest.type_name.is_empty() {
        manifest.type_name = CONTEXT_EVAL_MANIFEST_TYPE.to_string();
    }
    if manifest.type_name != CONTEXT_EVAL_MANIFEST_TYPE {
        return Err(VmError::Runtime(format!(
            "context eval manifest _type must be {CONTEXT_EVAL_MANIFEST_TYPE}"
        )));
    }
    if manifest.version == 0 {
        manifest.version = CONTEXT_EVAL_SCHEMA_VERSION;
    }
    if manifest.version != CONTEXT_EVAL_SCHEMA_VERSION {
        return Err(VmError::Runtime(format!(
            "context eval manifest version must be {CONTEXT_EVAL_SCHEMA_VERSION}"
        )));
    }
    if manifest.id.trim().is_empty() {
        manifest.id = "context-eval".to_string();
    }
    if manifest.modes.is_empty() {
        return Err(VmError::Runtime(
            "context eval manifest must declare at least one mode".to_string(),
        ));
    }
    if manifest.tasks.is_empty() {
        return Err(VmError::Runtime(
            "context eval manifest must declare at least one task".to_string(),
        ));
    }
    let mut mode_ids = BTreeSet::new();
    for (index, mode) in manifest.modes.iter_mut().enumerate() {
        if mode.id.trim().is_empty() {
            mode.id = format!("mode_{}", index + 1);
        }
        if !mode_ids.insert(mode.id.clone()) {
            return Err(VmError::Runtime(format!(
                "context eval manifest has duplicate mode id '{}'",
                mode.id
            )));
        }
        if mode.kind.trim().is_empty() {
            mode.kind = mode.id.clone();
        }
    }
    let mut task_ids = BTreeSet::new();
    for (index, task) in manifest.tasks.iter_mut().enumerate() {
        if task.id.trim().is_empty() {
            task.id = format!("task_{}", index + 1);
        }
        if !task_ids.insert(task.id.clone()) {
            return Err(VmError::Runtime(format!(
                "context eval manifest has duplicate task id '{}'",
                task.id
            )));
        }
        if task.objective.trim().is_empty() {
            return Err(VmError::Runtime(format!(
                "context eval task '{}' must declare objective",
                task.id
            )));
        }
        for (artifact_index, artifact) in task.artifacts.iter_mut().enumerate() {
            normalize_eval_artifact(artifact, &task.id, artifact_index);
        }
        task.tools.sort_by(|left, right| left.name.cmp(&right.name));
        task.tool_events.sort_by(|left, right| {
            left.order
                .unwrap_or(usize::MAX)
                .cmp(&right.order.unwrap_or(usize::MAX))
                .then_with(|| left.name.cmp(&right.name))
        });
    }
    Ok(())
}

fn normalize_eval_artifact(artifact: &mut ArtifactRecord, task_id: &str, index: usize) {
    if artifact.type_name.is_empty() {
        artifact.type_name = "artifact".to_string();
    }
    if artifact.id.trim().is_empty() {
        artifact.id = format!("{task_id}_artifact_{}", index + 1);
    }
    if artifact.kind.trim().is_empty() {
        artifact.kind = "artifact".to_string();
    }
    if artifact.created_at.trim().is_empty() {
        artifact.created_at = "1970-01-01T00:00:00Z".to_string();
    }
    if artifact.estimated_tokens.is_none() {
        artifact.estimated_tokens = artifact
            .text
            .as_ref()
            .map(|text| ((text.len() as f64) / 4.0).ceil() as usize);
    }
    if artifact.priority.is_none() {
        artifact.priority = Some(40);
    }
}

fn evaluate_task_mode(
    task: &ContextEvalTask,
    mode: &ContextEvalMode,
) -> Result<ContextEvalRunReport, VmError> {
    let prepared = prepare_mode_run(task, mode)?;
    let mode_id = mode.id.clone();
    let mode_kind = mode_kind(mode);
    let observed = task
        .mode_observations
        .get(&mode_id)
        .unwrap_or(&task.observed);
    let visible_input = visible_input(task, &prepared);
    let (final_surface, correctness_source) = observed
        .final_response
        .as_ref()
        .map(|response| (response.as_str(), "final_response"))
        .unwrap_or_else(|| (visible_input.as_str(), "context_projection"));
    let final_correctness = score_correctness(
        &task.expected,
        final_surface,
        &prepared.selected_artifact_ids,
        correctness_source,
    );
    let tool_call_quality =
        score_tools(&task.expected.expected_tools, &prepared.visible_tool_events);
    let reads_before_first_edit = reads_before_first_edit(&prepared.visible_tool_events);
    let error_recovery_count = error_recovery_count(&prepared.visible_tool_events);
    let input_tokens = observed
        .input_tokens
        .unwrap_or_else(|| estimate_chunk_tokens(&visible_input));
    let output_tokens = observed
        .output_tokens
        .or_else(|| {
            task.reference_answer
                .as_ref()
                .map(|text| estimate_chunk_tokens(text))
        })
        .unwrap_or(0);
    let compaction_count = observed.compaction_count.unwrap_or(0) + mode_compaction_count(mode);
    let latency_ms = observed.latency_ms.unwrap_or(0);
    let cost_usd = observed.cost_usd.unwrap_or(0.0);
    let mut failures = Vec::new();
    if !final_correctness.required_terms_missing.is_empty() {
        failures.push(format!(
            "missing required terms: {}",
            final_correctness.required_terms_missing.join(", ")
        ));
    }
    if !final_correctness.expected_artifact_ids_missing.is_empty() {
        failures.push(format!(
            "missing expected artifacts: {}",
            final_correctness.expected_artifact_ids_missing.join(", ")
        ));
    }
    if !tool_call_quality.missing_tools.is_empty() {
        failures.push(format!(
            "missing expected tools: {}",
            tool_call_quality.missing_tools.join(", ")
        ));
    }
    if let Some(max) = task.expected.max_input_tokens {
        if input_tokens > max {
            failures.push(format!("input tokens {input_tokens} exceed max {max}"));
        }
    }
    let passed = failures.is_empty();
    let stable_input_hash = stable_hash(&[
        task.id.as_str(),
        mode.id.as_str(),
        &prepared.selected_artifact_ids.join("\n"),
        prepared.rendered_context.as_str(),
        prepared.transcript_text.as_str(),
        &prepared.exposed_tools.join("\n"),
    ]);
    let cache_namespace = mode
        .cache_namespace
        .clone()
        .unwrap_or_else(|| "harn.context_eval".to_string());
    Ok(ContextEvalRunReport {
        run_id: format!("{}__{}", task.id, mode.id),
        task_id: task.id.clone(),
        mode_id,
        mode_kind,
        status: if passed { "pass" } else { "fail" }.to_string(),
        passed,
        final_correctness,
        reads_before_first_edit,
        tool_call_quality,
        latency_ms,
        input_tokens,
        output_tokens,
        cost_usd,
        compaction_count,
        projection: prepared.projection,
        context: ContextEvalContextReport {
            projection_policy: projection_policy(mode),
            tool_disclosure: tool_disclosure(mode),
            artifact_count: prepared.artifacts.len(),
            selected_artifact_ids: prepared.selected_artifact_ids,
            dropped_artifact_ids: prepared.dropped_artifact_ids,
            rendered_bytes: prepared.rendered_context.len(),
            rendered_tokens: estimate_chunk_tokens(&prepared.rendered_context),
            budget_tokens: mode_budget_tokens(mode),
            assemble_strategy: assemble_strategy(mode)?.as_str().to_string(),
            dedup: assemble_dedup(mode)?.as_str().to_string(),
            exposed_tools: prepared.exposed_tools,
        },
        cache: ContextEvalCacheReport {
            namespace: cache_namespace.clone(),
            key: format!("{}:{}", cache_namespace, &stable_input_hash[..32]),
            stable_input_hash,
            deterministic_order: true,
            hit: mode.expected_cache_hit.or(observed.cache_hit),
        },
        error_recovery_count,
        preprocessing: preprocessing_report(mode),
        failures,
    })
}

fn prepare_mode_run(
    task: &ContextEvalTask,
    mode: &ContextEvalMode,
) -> Result<PreparedModeRun, VmError> {
    let filtered = filter_artifacts(task, mode);
    let options = AssembleOptions {
        budget_tokens: mode_budget_tokens(mode),
        dedup: assemble_dedup(mode)?,
        strategy: assemble_strategy(mode)?,
        query: Some(task.objective.clone()),
        microcompact_threshold: mode.microcompact_threshold.unwrap_or(2_000),
        semantic_overlap: mode.semantic_overlap.unwrap_or(0.85),
    };
    let assembled = assemble_context(&filtered, &options, None);
    let selected_artifact_ids = sorted_strings(
        &assembled
            .included
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect::<Vec<_>>(),
    );
    let dropped_artifact_ids = sorted_strings(
        &assembled
            .dropped
            .iter()
            .map(|item| item.artifact_id.clone())
            .collect::<Vec<_>>(),
    );
    let rendered_context = if assembled.chunks.is_empty() {
        String::new()
    } else {
        render_assembled_chunks(&assembled)
    };
    let (projection, transcript_text) = project_transcript(task, mode);
    let exposed_tools = exposed_tools(task, mode);
    let visible_tool_events = visible_tool_events(task, &exposed_tools, mode);
    Ok(PreparedModeRun {
        artifacts: filtered,
        rendered_context,
        selected_artifact_ids,
        dropped_artifact_ids,
        projection,
        transcript_text,
        exposed_tools,
        visible_tool_events,
    })
}

fn filter_artifacts(task: &ContextEvalTask, mode: &ContextEvalMode) -> Vec<ArtifactRecord> {
    if mode_kind(mode) == "cold" || mode_budget_tokens(mode) == 0 {
        return Vec::new();
    }
    let include_ids: BTreeSet<&str> = mode.artifact_ids.iter().map(String::as_str).collect();
    let include_kinds: BTreeSet<&str> = mode
        .include_artifact_kinds
        .iter()
        .map(String::as_str)
        .collect();
    let exclude_kinds: BTreeSet<&str> = mode
        .exclude_artifact_kinds
        .iter()
        .map(String::as_str)
        .collect();
    let kind = mode_kind(mode);
    task.artifacts
        .iter()
        .filter(|artifact| include_ids.is_empty() || include_ids.contains(artifact.id.as_str()))
        .filter(|artifact| {
            include_kinds.is_empty()
                || include_kinds.contains(artifact.kind.as_str())
                || include_kinds.contains(
                    artifact
                        .metadata
                        .get("context_tier")
                        .and_then(JsonValue::as_str)
                        .unwrap_or(""),
                )
        })
        .filter(|artifact| !exclude_kinds.contains(artifact.kind.as_str()))
        .filter(|artifact| {
            default_mode_allows_artifact(
                &kind,
                artifact,
                include_ids.is_empty() && include_kinds.is_empty(),
            )
        })
        .cloned()
        .collect()
}

fn default_mode_allows_artifact(
    kind: &str,
    artifact: &ArtifactRecord,
    using_default_filter: bool,
) -> bool {
    if !using_default_filter {
        return true;
    }
    match kind {
        "cold" => false,
        "scanned" => artifact_matches_tier(artifact, &["scan", "scanned", "tier1_scan"]),
        "enriched" => artifact_matches_tier(
            artifact,
            &[
                "scan",
                "scanned",
                "tier1_scan",
                "enrichment",
                "enriched",
                "tier2_enrichment",
            ],
        ),
        _ => true,
    }
}

fn artifact_matches_tier(artifact: &ArtifactRecord, labels: &[&str]) -> bool {
    labels.iter().any(|label| {
        artifact.kind == *label
            || artifact
                .metadata
                .get("context_tier")
                .and_then(JsonValue::as_str)
                == Some(*label)
    })
}

fn project_transcript(
    task: &ContextEvalTask,
    mode: &ContextEvalMode,
) -> (ContextEvalProjectionReport, String) {
    let policy = projection_policy(mode);
    let keep_last = mode.transcript_keep_last.unwrap_or(match policy.as_str() {
        "none" => 0,
        "summary" | "compacted" => 1,
        "last_n" | "projected" => 2,
        _ => task.transcript.len(),
    });
    let retained: Vec<&ContextEvalTranscriptMessage> = match policy.as_str() {
        "none" => Vec::new(),
        "full" => task.transcript.iter().collect(),
        "summary" | "compacted" | "last_n" | "projected" => task
            .transcript
            .iter()
            .rev()
            .take(keep_last)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        _ => task.transcript.iter().collect(),
    };
    let transcript_text = retained
        .iter()
        .map(|message| format!("{}: {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n");
    let retained_tokens = retained
        .iter()
        .map(|message| {
            message
                .estimated_tokens
                .unwrap_or_else(|| estimate_chunk_tokens(&message.content))
        })
        .sum();
    (
        ContextEvalProjectionReport {
            policy,
            source_message_count: task.transcript.len(),
            retained_message_count: retained.len(),
            retained_tokens,
        },
        transcript_text,
    )
}

fn exposed_tools(task: &ContextEvalTask, mode: &ContextEvalMode) -> Vec<String> {
    let disclosure = tool_disclosure(mode);
    let allowlist: BTreeSet<&str> = mode.tool_allowlist.iter().map(String::as_str).collect();
    let mut names = match disclosure.as_str() {
        "none" => Vec::new(),
        "full" => task.tools.iter().map(|tool| tool.name.clone()).collect(),
        "limited" | "tool_search_limited" => task
            .tools
            .iter()
            .filter(|tool| allowlist.contains(tool.name.as_str()))
            .map(|tool| tool.name.clone())
            .collect(),
        _ => task.tools.iter().map(|tool| tool.name.clone()).collect(),
    };
    if disclosure == "tool_search_limited" && !names.iter().any(|name| name == "tool_search") {
        names.push("tool_search".to_string());
    }
    sorted_strings(&names)
}

fn visible_tool_events(
    task: &ContextEvalTask,
    exposed_tools: &[String],
    mode: &ContextEvalMode,
) -> Vec<ContextEvalToolEvent> {
    if tool_disclosure(mode) == "full" {
        return task.tool_events.clone();
    }
    let exposed: BTreeSet<&str> = exposed_tools.iter().map(String::as_str).collect();
    task.tool_events
        .iter()
        .filter(|event| exposed.contains(event.name.as_str()))
        .cloned()
        .collect()
}

fn visible_input(task: &ContextEvalTask, prepared: &PreparedModeRun) -> String {
    [
        task.objective.as_str(),
        prepared.rendered_context.as_str(),
        prepared.transcript_text.as_str(),
        &prepared.exposed_tools.join("\n"),
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

fn score_correctness(
    expected: &ContextEvalExpected,
    surface: &str,
    selected_artifact_ids: &[String],
    source: &str,
) -> ContextEvalCorrectness {
    let lower_surface = surface.to_ascii_lowercase();
    let mut present_terms = Vec::new();
    let mut missing_terms = Vec::new();
    for term in sorted_strings(&expected.required_terms) {
        if lower_surface.contains(&term.to_ascii_lowercase()) {
            present_terms.push(term);
        } else {
            missing_terms.push(term);
        }
    }
    let selected: BTreeSet<&str> = selected_artifact_ids.iter().map(String::as_str).collect();
    let mut present_artifacts = Vec::new();
    let mut missing_artifacts = Vec::new();
    for id in sorted_strings(&expected.expected_artifact_ids) {
        if selected.contains(id.as_str()) {
            present_artifacts.push(id);
        } else {
            missing_artifacts.push(id);
        }
    }
    let term_score = fraction(
        present_terms.len(),
        present_terms.len() + missing_terms.len(),
    );
    let artifact_score = fraction(
        present_artifacts.len(),
        present_artifacts.len() + missing_artifacts.len(),
    );
    let score = if expected.required_terms.is_empty() && expected.expected_artifact_ids.is_empty() {
        1.0
    } else if expected.required_terms.is_empty() || expected.expected_artifact_ids.is_empty() {
        term_score.max(artifact_score)
    } else {
        f64::midpoint(term_score, artifact_score)
    };
    ContextEvalCorrectness {
        passed: missing_terms.is_empty() && missing_artifacts.is_empty(),
        score: round4(score),
        required_terms_present: present_terms,
        required_terms_missing: missing_terms,
        expected_artifact_ids_present: present_artifacts,
        expected_artifact_ids_missing: missing_artifacts,
        source: source.to_string(),
    }
}

fn score_tools(
    expected_tools: &[String],
    events: &[ContextEvalToolEvent],
) -> ContextEvalToolQuality {
    let expected = sorted_strings(expected_tools);
    let expected_set: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    let observed = sorted_strings(
        &events
            .iter()
            .map(|event| event.name.clone())
            .collect::<Vec<_>>(),
    );
    let observed_set: BTreeSet<&str> = observed.iter().map(String::as_str).collect();
    let matched_tools = expected
        .iter()
        .filter(|tool| observed_set.contains(tool.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let missing_tools = expected
        .iter()
        .filter(|tool| !observed_set.contains(tool.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let unnecessary_tools = observed
        .iter()
        .filter(|tool| !expected_set.contains(tool.as_str()) && !is_edit_tool(tool))
        .cloned()
        .collect::<Vec<_>>();
    let denominator = expected.len() + unnecessary_tools.len();
    let score = if denominator == 0 {
        1.0
    } else {
        matched_tools.len() as f64 / denominator as f64
    };
    ContextEvalToolQuality {
        score: round4(score),
        expected_tools: expected,
        observed_tools: observed,
        matched_tools,
        missing_tools,
        unnecessary_tools,
    }
}

fn reads_before_first_edit(events: &[ContextEvalToolEvent]) -> usize {
    let mut reads = 0;
    for event in events {
        if is_edit_event(event) {
            break;
        }
        if is_read_event(event) {
            reads += 1;
        }
    }
    reads
}

fn error_recovery_count(events: &[ContextEvalToolEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            event.recovery == Some(true)
                || event
                    .phase
                    .as_deref()
                    .is_some_and(|phase| phase.contains("recovery") || phase.contains("error"))
                || event.quality.as_deref() == Some("recovery")
        })
        .count()
}

fn aggregate_runs(runs: &[ContextEvalRunReport]) -> ContextEvalAggregate {
    let total = runs.len();
    let mean_final_correctness = mean(total, runs.iter().map(|run| run.final_correctness.score));
    let mean_tool_call_quality = mean(total, runs.iter().map(|run| run.tool_call_quality.score));
    ContextEvalAggregate {
        mean_final_correctness,
        mean_tool_call_quality,
        total_latency_ms: runs.iter().map(|run| run.latency_ms).sum(),
        total_input_tokens: runs.iter().map(|run| run.input_tokens).sum(),
        total_output_tokens: runs.iter().map(|run| run.output_tokens).sum(),
        total_cost_usd: round6(runs.iter().map(|run| run.cost_usd).sum()),
        total_compaction_count: runs.iter().map(|run| run.compaction_count).sum(),
        total_error_recovery_count: runs.iter().map(|run| run.error_recovery_count).sum(),
    }
}

fn mode_kind(mode: &ContextEvalMode) -> String {
    let value = mode.kind.trim();
    if value.is_empty() {
        mode.id.clone()
    } else {
        value.to_string()
    }
}

fn mode_budget_tokens(mode: &ContextEvalMode) -> usize {
    mode.budget_tokens
        .unwrap_or_else(|| match mode_kind(mode).as_str() {
            "cold" => 0,
            "scanned" => 800,
            "enriched" => 1_200,
            "hud_pack" | "projected" | "compacted" | "tool_search_limited" => 1_600,
            "full" => 64_000,
            _ => 8_000,
        })
}

fn assemble_strategy(mode: &ContextEvalMode) -> Result<AssembleStrategy, VmError> {
    mode.assemble_strategy
        .as_deref()
        .map(AssembleStrategy::parse)
        .transpose()
        .map_err(VmError::Runtime)
        .map(|value| value.unwrap_or(AssembleStrategy::Relevance))
}

fn assemble_dedup(mode: &ContextEvalMode) -> Result<AssembleDedup, VmError> {
    mode.dedup
        .as_deref()
        .map(AssembleDedup::parse)
        .transpose()
        .map_err(VmError::Runtime)
        .map(|value| value.unwrap_or(AssembleDedup::Chunked))
}

fn projection_policy(mode: &ContextEvalMode) -> String {
    mode.projection_policy
        .clone()
        .unwrap_or_else(|| match mode_kind(mode).as_str() {
            "cold" | "scanned" | "enriched" | "hud_pack" | "tool_search_limited" => {
                "none".to_string()
            }
            "projected" => "last_n".to_string(),
            "compacted" => "compacted".to_string(),
            "full" => "full".to_string(),
            _ => "none".to_string(),
        })
}

fn tool_disclosure(mode: &ContextEvalMode) -> String {
    mode.tool_disclosure
        .clone()
        .unwrap_or_else(|| match mode_kind(mode).as_str() {
            "cold" | "scanned" | "enriched" | "hud_pack" => "none".to_string(),
            "tool_search_limited" => "tool_search_limited".to_string(),
            "full" => "full".to_string(),
            _ => "limited".to_string(),
        })
}

fn preprocessing_report(mode: &ContextEvalMode) -> ContextEvalPreprocessing {
    let preprocessing = mode
        .preprocessing
        .clone()
        .unwrap_or_else(|| "deterministic".to_string());
    ContextEvalPreprocessing {
        llm_enabled: preprocessing == "llm",
        mode: preprocessing,
    }
}

fn mode_compaction_count(mode: &ContextEvalMode) -> usize {
    usize::from(mode_kind(mode) == "compacted" || mode.compaction_policy.is_some())
}

fn is_read_event(event: &ContextEvalToolEvent) -> bool {
    event
        .phase
        .as_deref()
        .is_some_and(|phase| phase == "read" || phase == "scan")
        || event.name.starts_with("read")
        || event.name.starts_with("search")
        || event.name.starts_with("list")
}

fn is_edit_event(event: &ContextEvalToolEvent) -> bool {
    event
        .phase
        .as_deref()
        .is_some_and(|phase| phase == "edit" || phase == "write" || phase == "mutation")
        || is_edit_tool(&event.name)
}

fn is_edit_tool(name: &str) -> bool {
    name.starts_with("edit")
        || name.starts_with("write")
        || name.starts_with("apply")
        || name.contains("patch")
}

fn fraction(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn mean(total: usize, values: impl Iterator<Item = f64>) -> f64 {
    if total == 0 {
        0.0
    } else {
        round4(values.sum::<f64>() / total as f64)
    }
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn stable_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

pub fn context_eval_default_output_dir() -> PathBuf {
    PathBuf::from(".harn-runs/context-eval/latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, kind: &str, text: &str) -> ArtifactRecord {
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            title: Some(id.to_string()),
            text: Some(text.to_string()),
            data: None,
            source: Some("fixture".to_string()),
            created_at: "2026-05-23T00:00:00Z".to_string(),
            freshness: Some("fresh".to_string()),
            priority: Some(80),
            lineage: Vec::new(),
            relevance: Some(1.0),
            estimated_tokens: None,
            stage: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn context_eval_scores_modes_deterministically() {
        let manifest = ContextEvalManifest {
            type_name: CONTEXT_EVAL_MANIFEST_TYPE.to_string(),
            version: 1,
            id: "smoke".to_string(),
            modes: vec![
                ContextEvalMode {
                    id: "cold".to_string(),
                    kind: "cold".to_string(),
                    ..Default::default()
                },
                ContextEvalMode {
                    id: "pack".to_string(),
                    kind: "hud_pack".to_string(),
                    artifact_ids: vec!["runbook".to_string()],
                    tool_disclosure: Some("limited".to_string()),
                    tool_allowlist: vec!["read_file".to_string()],
                    ..Default::default()
                },
            ],
            tasks: vec![ContextEvalTask {
                id: "task".to_string(),
                objective: "Find the rollback command".to_string(),
                artifacts: vec![artifact(
                    "runbook",
                    "context_pack",
                    "Use deploy rollback now.",
                )],
                tools: vec![ContextEvalTool {
                    name: "read_file".to_string(),
                    ..Default::default()
                }],
                tool_events: vec![ContextEvalToolEvent {
                    order: Some(1),
                    name: "read_file".to_string(),
                    phase: Some("read".to_string()),
                    success: Some(true),
                    quality: Some("useful".to_string()),
                    recovery: None,
                }],
                expected: ContextEvalExpected {
                    required_terms: vec!["deploy rollback".to_string()],
                    expected_artifact_ids: vec!["runbook".to_string()],
                    expected_tools: vec!["read_file".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };

        let report = evaluate_context_eval_manifest(&manifest).expect("context eval succeeds");
        assert_eq!(report.total_runs, 2);
        assert_eq!(report.passed_runs, 1);
        assert!(!report.runs[0].passed);
        assert!(report.runs[1].passed);
        assert_eq!(report.runs[1].reads_before_first_edit, 1);
        assert_eq!(report.runs[1].tool_call_quality.score, 1.0);
        assert_eq!(report.runs[1].cache.stable_input_hash.len(), 64);
    }
}
