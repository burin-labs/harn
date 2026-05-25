//! Trajectory tap: ingest `agent_loop` turn-level records as candidate
//! crystallization sources, alongside the Code Mode composition snippets
//! already handled by [`super::api::crystallize_traces`].
//!
//! Background. Closed harn#1622 fed repeated Code Mode composition
//! snippets into the crystallization pipeline. Closed harn#2436 extends
//! the same `bundle → codegen → shadow → normalize` machinery to a
//! second candidate source: turn-level trajectories emitted by
//! `agent_loop`. Trajectory candidates carry source field
//! [`TRAJECTORY_SOURCE`] (`"agent_loop_trajectory"`) so downstream
//! consumers (cloud importers, receipt viewers) can distinguish them
//! from composition-derived candidates.
//!
//! The tap is intentionally narrow: it groups *consecutive successful
//! turns* by their tool-call signature similarity into one or more
//! candidate segments. Each segment becomes a
//! [`CrystallizationTrace`] with `source = "agent_loop_trajectory"`
//! and per-action metadata so the existing mining and shadow pipelines
//! treat the trace identically to any other source. The replay
//! verifier ([`verify_trajectory_candidate`]) re-derives a regenerated
//! fixture from the candidate steps and runs the existing replay oracle
//! against the original trace; if the deterministic outputs diverge the
//! candidate is rejected before it reaches `bundle`.
//!
//! `agent_loop` itself emits per-turn events through the event log
//! (`AgentEvent::IterationEnd`, `ToolCall`, `ToolCallUpdate`, etc.).
//! A future patch can build [`AgentTurnRecord`] values directly from
//! those events; this module takes them already projected so callers
//! (replay drivers, CLI subcommands, integration tests) can construct
//! synthetic trajectories without standing up an entire agent runtime.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use super::super::{
    now_rfc3339, run_replay_oracle_trace, ReplayAllowlistRule, ReplayExpectation, ReplayOracleTrace,
};
use super::api::{crystallize_traces, synthesize_candidate_from_trace};
use super::types::{
    CrystallizationAction, CrystallizationArtifacts, CrystallizationCost,
    CrystallizationSideEffect, CrystallizationTrace, CrystallizeOptions, WorkflowCandidate,
};
use super::util::hash_bytes;
use crate::value::VmError;

/// Source marker stamped on trajectory-derived traces and bundle
/// receipts. Consumers (cloud importers, receipt viewers) use this to
/// distinguish trajectory candidates from `code_mode_composition`
/// candidates and from release-fixture-derived single-trace candidates.
pub const TRAJECTORY_SOURCE: &str = "agent_loop_trajectory";

/// Default similarity threshold for grouping consecutive successful
/// turns. Two adjacent turns are part of the same segment if their
/// tool-call signature multisets overlap by at least this Jaccard
/// coefficient. Tuned for merge-captain-style workloads where the same
/// 2-3 tools recur across turns. Callers can override via
/// [`TrajectoryTap::with_similarity_threshold`].
const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.5;

/// Default minimum length for a segment to be emitted as a trace. Below
/// this we treat the run as too short to crystallize. The crystallize
/// pipeline itself enforces `DEFAULT_MIN_EXAMPLES` across traces, but
/// the per-segment floor keeps single-turn noise out of the trace pool.
const DEFAULT_MIN_SEGMENT_LEN: usize = 2;

/// Maximum length for a segment. Beyond this we split into two segments
/// so a runaway trace can't blow the crystallization budget. Matches the
/// cap in [`super::normalize::best_repeated_sequence`].
const DEFAULT_MAX_SEGMENT_LEN: usize = 12;

/// Tolerance for the replay verifier. We count how many deterministic
/// steps diverge between the regenerated fixture and the original
/// trace; if more than this fraction diverges the candidate is rejected
/// rather than just warned about.
const DEFAULT_DIVERGENCE_TOLERANCE: f64 = 0.0;

/// Per-turn snapshot of an `agent_loop` round-trip. One record covers
/// one model call plus the tool calls dispatched in response to it.
/// Built by replay drivers or by walking
/// [`crate::agent_events::AgentEvent`] streams.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentTurnRecord {
    pub iteration: usize,
    pub session_id: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// `true` when the turn produced no failed tool calls and no
    /// terminal error from the loop. Failed turns split a segment: a
    /// run of successful turns interrupted by a failure becomes two
    /// candidate segments rather than one.
    pub success: bool,
    pub tool_calls: Vec<AgentTurnToolCall>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: Option<i64>,
    /// Final assistant text emitted on the turn, if any. Used as the
    /// observed output for the synthesized `model_call` action so the
    /// shadow oracle can compare deterministic prefixes across replays.
    pub assistant_text: Option<String>,
    /// Free-form metadata copied into the synthesized model_call
    /// action. Trajectory callers commonly inject `goal`,
    /// `success_criteria`, and any policy decisions taken during the
    /// turn. The collector never overwrites the source field —
    /// [`TRAJECTORY_SOURCE`] is always stamped on top regardless.
    pub metadata: BTreeMap<String, JsonValue>,
}

/// Per-tool-call snapshot used by [`AgentTurnRecord`]. Mirrors the
/// fields the existing [`CrystallizationAction`] pipeline already
/// consumes so the trajectory tap stays a thin adapter.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentTurnToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    /// Status string from `ToolCallUpdate.status` (`completed`,
    /// `failed`, …). Anything other than `completed` excludes the
    /// containing turn from the success run.
    pub status: String,
    pub raw_input: JsonValue,
    pub raw_output: Option<JsonValue>,
    pub capabilities: Vec<String>,
    pub side_effects: Vec<CrystallizationSideEffect>,
    pub duration_ms: Option<i64>,
    /// Caller-provided scalar parameter map. The mining pipeline
    /// extracts varying scalar values from `raw_input` automatically;
    /// this map is for callers who already have a typed projection
    /// (e.g. release identity fields, classifier labels) and want
    /// them to surface as first-class workflow parameters.
    pub parameters: BTreeMap<String, JsonValue>,
}

impl AgentTurnToolCall {
    fn is_completed(&self) -> bool {
        self.status.eq_ignore_ascii_case("completed")
    }

    fn signature(&self) -> String {
        // Match `action_signature` shape so two trajectory traces
        // collapse onto the same sequence_signature inside
        // `mine_candidates` when their tool name + parameter keys
        // align.
        let mut parameter_keys = self
            .parameters
            .keys()
            .cloned()
            .chain(json_scalar_keys(&self.raw_input))
            .collect::<Vec<_>>();
        parameter_keys.sort();
        parameter_keys.dedup();
        format!("tool_call:{}:{}", self.tool_name, parameter_keys.join(","))
    }
}

/// Collector that converts a slice of [`AgentTurnRecord`]s into
/// trajectory-derived [`CrystallizationTrace`]s. Held as a struct so
/// callers can configure thresholds without a long argument list.
#[derive(Clone, Debug)]
pub struct TrajectoryTap {
    session_id: String,
    workflow_id: Option<String>,
    similarity_threshold: f64,
    min_segment_len: usize,
    max_segment_len: usize,
}

impl TrajectoryTap {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            workflow_id: None,
            similarity_threshold: DEFAULT_SIMILARITY_THRESHOLD,
            min_segment_len: DEFAULT_MIN_SEGMENT_LEN,
            max_segment_len: DEFAULT_MAX_SEGMENT_LEN,
        }
    }

    pub fn with_workflow_id(mut self, workflow_id: impl Into<String>) -> Self {
        self.workflow_id = Some(workflow_id.into());
        self
    }

    pub fn with_similarity_threshold(mut self, value: f64) -> Self {
        self.similarity_threshold = value.clamp(0.0, 1.0);
        self
    }

    pub fn with_segment_len(mut self, min: usize, max: usize) -> Self {
        self.min_segment_len = min.max(1);
        self.max_segment_len = max.max(self.min_segment_len);
        self
    }

    /// Group consecutive successful turns into one or more trajectory
    /// traces. Returns an empty vec when no run of successful turns
    /// reaches `min_segment_len`.
    pub fn collect(&self, turns: &[AgentTurnRecord]) -> Vec<CrystallizationTrace> {
        let mut traces = Vec::new();
        for segment in self.segment_turns(turns) {
            traces.push(self.trace_from_segment(segment));
        }
        traces
    }

    fn segment_turns<'a>(&self, turns: &'a [AgentTurnRecord]) -> Vec<&'a [AgentTurnRecord]> {
        if turns.is_empty() {
            return Vec::new();
        }
        let mut segments = Vec::new();
        let mut cursor = 0;
        while cursor < turns.len() {
            if !turn_is_successful(&turns[cursor]) {
                cursor += 1;
                continue;
            }
            let mut end = cursor + 1;
            while end < turns.len()
                && end - cursor < self.max_segment_len
                && turn_is_successful(&turns[end])
                && self.adjacent_similarity(&turns[end - 1], &turns[end])
                    >= self.similarity_threshold
            {
                end += 1;
            }
            if end - cursor >= self.min_segment_len {
                segments.push(&turns[cursor..end]);
            }
            cursor = end;
        }
        segments
    }

    fn adjacent_similarity(&self, left: &AgentTurnRecord, right: &AgentTurnRecord) -> f64 {
        jaccard_similarity(
            &tool_signature_multiset(left),
            &tool_signature_multiset(right),
        )
    }

    fn trace_from_segment(&self, turns: &[AgentTurnRecord]) -> CrystallizationTrace {
        let segment_index = turns.first().map(|t| t.iteration).unwrap_or(0);
        let id = format!(
            "{}_trajectory_{}_{}",
            self.session_id,
            segment_index,
            turns.last().map(|t| t.iteration).unwrap_or(segment_index),
        );
        let started_at = turns.first().and_then(|t| t.started_at.clone());
        let finished_at = turns.last().and_then(|t| t.finished_at.clone());
        let mut actions = Vec::with_capacity(turns.iter().map(|t| t.tool_calls.len() + 1).sum());
        for turn in turns {
            actions.push(model_call_action(turn));
            for call in &turn.tool_calls {
                actions.push(tool_call_action(turn.iteration, call));
            }
        }

        let mut metadata = BTreeMap::new();
        metadata.insert("source".to_string(), json!(TRAJECTORY_SOURCE));
        metadata.insert("session_id".to_string(), json!(self.session_id));
        metadata.insert(
            "iteration_span".to_string(),
            json!([
                segment_index,
                turns.last().map(|t| t.iteration).unwrap_or(segment_index)
            ]),
        );
        metadata.insert("turn_count".to_string(), json!(turns.len()));

        let payload = serde_json::to_vec(&actions).unwrap_or_default();
        CrystallizationTrace {
            version: 1,
            id,
            source: Some(TRAJECTORY_SOURCE.to_string()),
            source_hash: Some(hash_bytes(&payload)),
            workflow_id: self.workflow_id.clone(),
            started_at,
            finished_at,
            actions,
            replay_allowlist: default_trajectory_allowlist(),
            metadata,
            ..CrystallizationTrace::default()
        }
    }
}

fn turn_is_successful(turn: &AgentTurnRecord) -> bool {
    turn.success && turn.tool_calls.iter().all(AgentTurnToolCall::is_completed)
}

fn tool_signature_multiset(turn: &AgentTurnRecord) -> Vec<String> {
    let mut sigs = turn
        .tool_calls
        .iter()
        .map(AgentTurnToolCall::signature)
        .collect::<Vec<_>>();
    sigs.sort();
    sigs
}

fn jaccard_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() && right.is_empty() {
        // Two turns with no tool calls (pure assistant chat) count as
        // similar — they trivially share the empty signature set.
        return 1.0;
    }
    let mut union = left.to_vec();
    union.extend(right.iter().cloned());
    union.sort();
    union.dedup();
    let union_len = union.len();
    if union_len == 0 {
        return 1.0;
    }
    let mut intersection = 0usize;
    let mut right_remaining = right.to_vec();
    for sig in left {
        if let Some(pos) = right_remaining.iter().position(|other| other == sig) {
            right_remaining.swap_remove(pos);
            intersection += 1;
        }
    }
    intersection as f64 / union_len as f64
}

fn model_call_action(turn: &AgentTurnRecord) -> CrystallizationAction {
    let mut metadata = turn.metadata.clone();
    metadata.insert("source".to_string(), json!(TRAJECTORY_SOURCE));
    metadata.insert("iteration".to_string(), json!(turn.iteration));
    metadata.insert("session_id".to_string(), json!(turn.session_id));
    if let Some(provider) = &turn.provider {
        metadata.insert("provider".to_string(), json!(provider));
    }
    let output = turn.assistant_text.as_ref().map(|text| json!(text));
    CrystallizationAction {
        id: format!("turn_{}", turn.iteration),
        kind: "model_call".to_string(),
        name: turn
            .model
            .clone()
            .unwrap_or_else(|| "agent_turn".to_string()),
        timestamp: turn.started_at.clone(),
        inputs: JsonValue::Null,
        output: output.clone(),
        observed_output: output,
        parameters: BTreeMap::new(),
        cost: CrystallizationCost {
            model: turn.model.clone(),
            model_calls: 1,
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            total_cost_usd: 0.0,
            wall_ms: turn.duration_ms.unwrap_or_default(),
        },
        duration_ms: turn.duration_ms,
        deterministic: Some(false),
        fuzzy: Some(true),
        metadata,
        ..CrystallizationAction::default()
    }
}

fn tool_call_action(iteration: usize, call: &AgentTurnToolCall) -> CrystallizationAction {
    let mut parameters = call.parameters.clone();
    if let JsonValue::Object(map) = &call.raw_input {
        for (key, value) in map {
            parameters
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
    let mut metadata = BTreeMap::new();
    metadata.insert("source".to_string(), json!(TRAJECTORY_SOURCE));
    metadata.insert("iteration".to_string(), json!(iteration));
    metadata.insert("tool_call_id".to_string(), json!(call.tool_call_id));
    metadata.insert("status".to_string(), json!(call.status));
    CrystallizationAction {
        id: if call.tool_call_id.is_empty() {
            format!("turn_{iteration}_{}", call.tool_name)
        } else {
            call.tool_call_id.clone()
        },
        kind: "tool_call".to_string(),
        name: call.tool_name.clone(),
        inputs: call.raw_input.clone(),
        output: call.raw_output.clone(),
        observed_output: call.raw_output.clone(),
        parameters,
        side_effects: call.side_effects.clone(),
        capabilities: call.capabilities.clone(),
        duration_ms: call.duration_ms,
        deterministic: Some(true),
        fuzzy: Some(false),
        metadata,
        ..CrystallizationAction::default()
    }
}

fn json_scalar_keys(value: &JsonValue) -> Vec<String> {
    match value {
        JsonValue::Object(map) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn default_trajectory_allowlist() -> Vec<ReplayAllowlistRule> {
    vec![
        ReplayAllowlistRule {
            path: "/run_id".to_string(),
            reason: "trajectory replay assigns a fresh run id per regeneration".to_string(),
            replacement: None,
        },
        ReplayAllowlistRule {
            path: "/effect_receipts/*/iteration".to_string(),
            reason: "trajectory regeneration may reseat iteration indices".to_string(),
            replacement: None,
        },
    ]
}

/// Verifier gate for trajectory-derived candidates. Re-derives a
/// "regenerated fixture" from the candidate's deterministic steps and
/// runs the existing replay oracle against the original trace. The
/// oracle catches divergence at the receipt level; this wrapper adds a
/// tolerance check on the action-level outputs so a single noisy
/// fuzzy step doesn't tip the whole candidate to rejected.
///
/// Returns `Ok(())` when the candidate is safe to keep, or an error
/// string suitable for pushing onto
/// [`WorkflowCandidate::rejection_reasons`].
pub fn verify_trajectory_candidate(
    candidate: &WorkflowCandidate,
    original: &CrystallizationTrace,
) -> Result<(), String> {
    verify_trajectory_candidate_with_tolerance(candidate, original, DEFAULT_DIVERGENCE_TOLERANCE)
}

fn verify_trajectory_candidate_with_tolerance(
    candidate: &WorkflowCandidate,
    original: &CrystallizationTrace,
    tolerance: f64,
) -> Result<(), String> {
    // 1. Sequence-signature check: the candidate's signature must
    //    appear inside the original trace at the recorded start index
    //    (or anywhere, if no example points to this trace). If we can't
    //    relocate the sequence the candidate isn't actually derived
    //    from this trace and shouldn't be promoted from it.
    let start_index = candidate
        .examples
        .iter()
        .find(|example| example.trace_id == original.id)
        .map(|example| example.start_index)
        .or_else(|| super::shadow::find_sequence_start(original, &candidate.sequence_signature))
        .ok_or_else(|| {
            format!(
                "trajectory verifier: candidate sequence not found in trace {}",
                original.id
            )
        })?;

    let end = start_index + candidate.steps.len();
    if end > original.actions.len() {
        return Err(format!(
            "trajectory verifier: candidate sequence extends past trace {} actions",
            original.id
        ));
    }

    // 2. Deterministic-output check with tolerance. The shadow path
    //    already does a strict comparison; this is the trajectory
    //    relaxation so an LLM-rewritten transient string doesn't fail
    //    the whole gate.
    let mut deterministic_total = 0usize;
    let mut deterministic_diverged = 0usize;
    for (offset, step) in candidate.steps.iter().enumerate() {
        if !matches!(step.segment, super::types::SegmentKind::Deterministic) {
            continue;
        }
        deterministic_total += 1;
        let Some(expected) = &step.expected_output else {
            continue;
        };
        let actual = original.actions[start_index + offset]
            .observed_output
            .as_ref()
            .or(original.actions[start_index + offset].output.as_ref());
        if actual != Some(expected) {
            deterministic_diverged += 1;
        }
    }
    if deterministic_total > 0 {
        let ratio = deterministic_diverged as f64 / deterministic_total as f64;
        if ratio > tolerance {
            return Err(format!(
                "trajectory verifier: {deterministic_diverged}/{deterministic_total} deterministic \
                 steps diverged from trace {} (tolerance {:.2})",
                original.id, tolerance
            ));
        }
    }

    // 3. Replay oracle on the regenerated fixture. We synthesize a
    //    minimal `ReplayTraceRun` from the candidate's expected
    //    receipts and compare it against the trace's recorded replay
    //    run (when present). Traces with no replay run skip the
    //    oracle — there's nothing to compare against.
    let Some(first_run) = original.replay_run.as_ref() else {
        return Ok(());
    };
    if first_run.effect_receipts.is_empty() && candidate.expected_receipts.is_empty() {
        return Ok(());
    }
    let mut regenerated = first_run.clone();
    regenerated.run_id = format!("trajectory_regen_{}", candidate.id);
    regenerated.effect_receipts = candidate.expected_receipts.clone();
    let oracle = ReplayOracleTrace {
        name: format!("trajectory_verify_{}", candidate.id),
        description: Some(
            "trajectory tap regenerated-fixture replay check against the source trace".to_string(),
        ),
        expect: ReplayExpectation::Match,
        allowlist: original.replay_allowlist.clone(),
        first_run: first_run.clone(),
        second_run: regenerated,
        ..ReplayOracleTrace::default()
    };
    let report = run_replay_oracle_trace(&oracle).map_err(|error| {
        format!(
            "trajectory verifier: oracle error for {}: {error}",
            candidate.id
        )
    })?;
    if !report.passed {
        let detail = report
            .divergence
            .as_ref()
            .map(|div| format!("{}: {}", div.path, div.message))
            .unwrap_or_else(|| "replay oracle reported failure with no divergence".to_string());
        return Err(format!(
            "trajectory verifier: regenerated fixture diverged for {}: {detail}",
            candidate.id
        ));
    }
    Ok(())
}

/// Top-level convenience: collect trajectories from `turns`, feed them
/// through the existing crystallization pipeline, and run the
/// trajectory replay verifier on every accepted candidate. Mirrors
/// [`super::release_fixture::ingest_release_fixture`] in shape so the
/// CLI / orchestrator can route trajectory ingestion through one
/// surface.
///
/// Returns `Ok(None)` when no segments cleared `min_segment_len`. When
/// fewer than `min_examples` segments are produced, falls back to
/// [`synthesize_candidate_from_trace`] using the first trace so a
/// short trajectory still yields a candidate. Any additional segments
/// not picked up by synthesis are still returned in
/// [`TrajectoryIngestResult::traces`] so callers / verifiers / bundle
/// builders can see them, and a `tracing::warn!` is emitted listing the
/// ids of the traces the synthesis path did not consume.
pub fn ingest_agent_loop_trajectory(
    tap: &TrajectoryTap,
    turns: &[AgentTurnRecord],
    options: CrystallizeOptions,
) -> Result<Option<TrajectoryIngestResult>, VmError> {
    let traces = tap.collect(turns);
    if traces.is_empty() {
        return Ok(None);
    }
    let needs_synthesis = traces.len() < options.min_examples.max(2);
    let (mut artifacts, trace_pool) = if needs_synthesis {
        // Synthesis builds a single-trace candidate, but we must not
        // silently discard the other traces — they still belong to
        // the result so the bundle pipeline, verifier, and any
        // downstream auditor can observe the full ingested set.
        let trace_pool = traces.clone();
        let mut iter = traces.into_iter();
        let primary = iter.next().expect("non-empty by check above");
        let dropped_from_synthesis: Vec<String> = iter.map(|t| t.id).collect();
        if !dropped_from_synthesis.is_empty() {
            tracing::warn!(
                target: "harn_vm::crystallize::trajectory",
                primary_trace_id = %primary.id,
                dropped_trace_ids = ?dropped_from_synthesis,
                min_examples = options.min_examples,
                segment_count = trace_pool.len(),
                "trajectory synthesis kept only the first trace; \
                 remaining traces are surfaced via TrajectoryIngestResult.traces \
                 but are not part of the synthesized candidate"
            );
        }
        let artifacts = synthesize_candidate_from_trace(primary, options, Vec::new(), None, None)?;
        (artifacts, trace_pool)
    } else {
        let trace_pool = traces.clone();
        let artifacts = crystallize_traces(traces, options)?;
        (artifacts, trace_pool)
    };

    apply_trajectory_verifier(&mut artifacts, &trace_pool);

    Ok(Some(TrajectoryIngestResult {
        artifacts,
        traces: trace_pool,
    }))
}

/// Bundle of artifacts produced by [`ingest_agent_loop_trajectory`].
/// `traces` is the same slice that should be passed to
/// [`super::bundle::build_crystallization_bundle`] so the bundle's
/// fixtures and source-trace references line up.
#[derive(Clone, Debug)]
pub struct TrajectoryIngestResult {
    pub artifacts: CrystallizationArtifacts,
    pub traces: Vec<CrystallizationTrace>,
}

/// Run [`verify_trajectory_candidate`] across every accepted candidate
/// in `artifacts`. Candidates whose verifier fails move from
/// `candidates` to `rejected_candidates` and gain a rejection reason.
pub fn apply_trajectory_verifier(
    artifacts: &mut CrystallizationArtifacts,
    traces: &[CrystallizationTrace],
) {
    let mut moved_ids = Vec::new();
    for candidate in &mut artifacts.report.candidates {
        // We verify against every trace the candidate references; the
        // first failure is enough to disqualify the candidate.
        for example in candidate.examples.clone() {
            let Some(trace) = traces.iter().find(|trace| trace.id == example.trace_id) else {
                continue;
            };
            if let Err(reason) = verify_trajectory_candidate(candidate, trace) {
                candidate.rejection_reasons.push(reason);
                moved_ids.push(candidate.id.clone());
                break;
            }
        }
    }
    if moved_ids.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    for candidate in std::mem::take(&mut artifacts.report.candidates) {
        if moved_ids.contains(&candidate.id) {
            artifacts.report.rejected_candidates.push(candidate);
        } else {
            keep.push(candidate);
        }
    }
    artifacts.report.candidates = keep;
    if artifacts
        .report
        .selected_candidate_id
        .as_ref()
        .is_some_and(|id| moved_ids.contains(id))
    {
        artifacts.report.selected_candidate_id = artifacts
            .report
            .candidates
            .first()
            .map(|candidate| candidate.id.clone());
        if let Some(candidate) = artifacts.report.candidates.first() {
            artifacts.harn_code = super::codegen::generate_harn_code(candidate);
            artifacts.eval_pack_toml = super::codegen::generate_eval_pack(candidate);
        } else {
            artifacts.harn_code =
                super::codegen::rejected_workflow_stub(&artifacts.report.rejected_candidates);
            artifacts.eval_pack_toml.clear();
        }
    }
}

/// Convenience constructor for an [`AgentTurnRecord`] used by tests
/// and replay drivers that have only the tool calls and want defaults
/// for the rest.
pub fn turn_record(
    iteration: usize,
    session_id: impl Into<String>,
    tool_calls: Vec<AgentTurnToolCall>,
) -> AgentTurnRecord {
    AgentTurnRecord {
        iteration,
        session_id: session_id.into(),
        success: true,
        tool_calls,
        started_at: Some(now_rfc3339()),
        finished_at: Some(now_rfc3339()),
        ..AgentTurnRecord::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, params: &[(&str, JsonValue)]) -> AgentTurnToolCall {
        let mut parameters = BTreeMap::new();
        let mut raw = serde_json::Map::new();
        for (key, value) in params {
            parameters.insert((*key).to_string(), value.clone());
            raw.insert((*key).to_string(), value.clone());
        }
        AgentTurnToolCall {
            tool_call_id: format!("call_{name}"),
            tool_name: name.to_string(),
            status: "completed".to_string(),
            raw_input: JsonValue::Object(raw),
            raw_output: Some(json!({"ok": true})),
            parameters,
            duration_ms: Some(10),
            ..AgentTurnToolCall::default()
        }
    }

    fn turn(iteration: usize, calls: Vec<AgentTurnToolCall>) -> AgentTurnRecord {
        AgentTurnRecord {
            iteration,
            session_id: "session-test".to_string(),
            success: true,
            tool_calls: calls,
            input_tokens: 100,
            output_tokens: 50,
            duration_ms: Some(20),
            assistant_text: Some("ok".to_string()),
            ..AgentTurnRecord::default()
        }
    }

    #[test]
    fn collects_consecutive_successful_turns_into_segments() {
        let turns = vec![
            turn(1, vec![call("git_status", &[("path", json!("."))])]),
            turn(2, vec![call("git_status", &[("path", json!("."))])]),
            AgentTurnRecord {
                success: false,
                ..turn(3, vec![call("git_status", &[("path", json!("."))])])
            },
            turn(4, vec![call("git_log", &[("path", json!("."))])]),
            turn(5, vec![call("git_log", &[("path", json!("."))])]),
        ];
        let tap = TrajectoryTap::new("s1");
        let traces = tap.collect(&turns);
        assert_eq!(traces.len(), 2);
        assert!(traces
            .iter()
            .all(|trace| trace.source.as_deref() == Some(TRAJECTORY_SOURCE)));
        assert!(traces
            .iter()
            .all(|trace| trace.metadata.get("source") == Some(&json!(TRAJECTORY_SOURCE))));
    }

    #[test]
    fn splits_segment_when_signatures_diverge() {
        // The signature of a tool call depends on its name AND its
        // parameter keys; switching from `git_status` to `git_diff`
        // drops similarity below the default threshold.
        let turns = vec![
            turn(1, vec![call("git_status", &[("path", json!("."))])]),
            turn(2, vec![call("git_status", &[("path", json!("."))])]),
            turn(3, vec![call("git_diff", &[("path", json!("."))])]),
            turn(4, vec![call("git_diff", &[("path", json!("."))])]),
        ];
        let tap = TrajectoryTap::new("s2").with_similarity_threshold(1.0);
        let traces = tap.collect(&turns);
        assert_eq!(traces.len(), 2, "expected one segment per signature group");
    }

    #[test]
    fn segment_shorter_than_minimum_is_dropped() {
        let turns = vec![turn(1, vec![call("git_status", &[("path", json!("."))])])];
        let tap = TrajectoryTap::new("s3");
        assert!(tap.collect(&turns).is_empty());
    }

    #[test]
    fn verifier_passes_on_clean_candidate() {
        let turns = vec![
            turn(1, vec![call("git_status", &[("path", json!("."))])]),
            turn(2, vec![call("git_status", &[("path", json!("."))])]),
        ];
        let tap = TrajectoryTap::new("s4");
        let result = ingest_agent_loop_trajectory(
            &tap,
            &turns,
            CrystallizeOptions {
                min_examples: 1,
                workflow_name: Some("verifier_clean".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .expect("ingest")
        .expect("at least one trace");
        assert!(
            !result.artifacts.report.candidates.is_empty(),
            "expected at least one accepted candidate"
        );
    }
}
