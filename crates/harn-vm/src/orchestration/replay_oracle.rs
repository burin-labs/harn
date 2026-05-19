use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeSet;
use std::fmt;

pub const REPLAY_TRACE_SCHEMA_VERSION: &str = "harn.orchestration.replay_trace.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReplayOracleTrace {
    pub schema_version: String,
    pub name: String,
    pub description: Option<String>,
    pub expect: ReplayExpectation,
    pub protocol_fixture_refs: Vec<String>,
    pub allowlist: Vec<ReplayAllowlistRule>,
    pub first_run: ReplayTraceRun,
    pub second_run: ReplayTraceRun,
}

impl Default for ReplayOracleTrace {
    fn default() -> Self {
        Self {
            schema_version: REPLAY_TRACE_SCHEMA_VERSION.to_string(),
            name: String::new(),
            description: None,
            expect: ReplayExpectation::Match,
            protocol_fixture_refs: Vec::new(),
            allowlist: Vec::new(),
            first_run: ReplayTraceRun::default(),
            second_run: ReplayTraceRun::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayExpectation {
    #[default]
    Match,
    Drift,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReplayAllowlistRule {
    /// JSON-pointer-like path. `*` matches every array element or object value.
    pub path: String,
    pub reason: String,
    pub replacement: Option<JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReplayTraceRun {
    pub run_id: String,
    pub event_log_entries: Vec<JsonValue>,
    pub trigger_firings: Vec<JsonValue>,
    pub llm_interactions: Vec<JsonValue>,
    pub protocol_interactions: Vec<JsonValue>,
    pub approval_interactions: Vec<JsonValue>,
    pub effect_receipts: Vec<JsonValue>,
    pub persona_runtime_states: Vec<JsonValue>,
    pub agent_transcript_deltas: Vec<JsonValue>,
    pub final_artifacts: Vec<JsonValue>,
    pub policy_decisions: Vec<JsonValue>,
    /// CH-07 (#1878): durable channel emit/match receipts captured from
    /// the `lifecycle.channel.audit` topic. First-class replay material
    /// alongside `effect_receipts` so multi-agent channel exchanges
    /// round-trip byte-identically across two runs of the same
    /// workload. Each entry is the JSON-encoded
    /// `ChannelEmitReceipt` / `ChannelMatchReceipt`.
    pub channel_receipts: Vec<JsonValue>,
    /// Lifecycle receipts (suspension / resumption / drain decisions) as
    /// journaled by `crate::orchestration::lifecycle_receipts`. First-class
    /// replay material per #1861 P-08 so the oracle treats a drift in
    /// `input_hash`, `action`, or signed timestamps as a determinism
    /// failure.
    pub lifecycle_receipts: Vec<JsonValue>,
}

impl ReplayTraceRun {
    pub fn counts(&self) -> ReplayTraceRunCounts {
        ReplayTraceRunCounts {
            event_log_entries: self.event_log_entries.len(),
            trigger_firings: self.trigger_firings.len(),
            llm_interactions: self.llm_interactions.len(),
            protocol_interactions: self.protocol_interactions.len(),
            approval_interactions: self.approval_interactions.len(),
            effect_receipts: self.effect_receipts.len(),
            persona_runtime_states: self.persona_runtime_states.len(),
            agent_transcript_deltas: self.agent_transcript_deltas.len(),
            final_artifacts: self.final_artifacts.len(),
            policy_decisions: self.policy_decisions.len(),
            channel_receipts: self.channel_receipts.len(),
            lifecycle_receipts: self.lifecycle_receipts.len(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayTraceRunCounts {
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
    /// CH-07 (#1878).
    pub channel_receipts: usize,
    pub lifecycle_receipts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReplayOracleReport {
    pub name: String,
    pub expectation: ReplayExpectation,
    pub passed: bool,
    pub first_run_counts: ReplayTraceRunCounts,
    pub second_run_counts: ReplayTraceRunCounts,
    pub protocol_fixture_refs: Vec<String>,
    pub divergence: Option<ReplayDivergence>,
}

impl Default for ReplayOracleReport {
    fn default() -> Self {
        Self {
            name: String::new(),
            expectation: ReplayExpectation::Match,
            passed: false,
            first_run_counts: ReplayTraceRunCounts::default(),
            second_run_counts: ReplayTraceRunCounts::default(),
            protocol_fixture_refs: Vec::new(),
            divergence: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReplayDivergence {
    pub path: String,
    pub left: JsonValue,
    pub right: JsonValue,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayOracleError {
    InvalidTrace(String),
    InvalidAllowlistPath(String),
    AllowlistPathMissing(String),
    Serialization(String),
}

impl fmt::Display for ReplayOracleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTrace(message)
            | Self::InvalidAllowlistPath(message)
            | Self::AllowlistPathMissing(message)
            | Self::Serialization(message) => message.fmt(f),
        }
    }
}

impl std::error::Error for ReplayOracleError {}

pub fn run_replay_oracle_trace(
    trace: &ReplayOracleTrace,
) -> Result<ReplayOracleReport, ReplayOracleError> {
    validate_trace(trace)?;
    let first_run_counts = trace.first_run.counts();
    let second_run_counts = trace.second_run.counts();
    let first = canonicalize_run(&trace.first_run, &trace.allowlist)?;
    let second = canonicalize_run(&trace.second_run, &trace.allowlist)?;
    let divergence = first_divergence(&first, &second);
    let passed = match (trace.expect, divergence.is_some()) {
        (ReplayExpectation::Match, false) => true,
        (ReplayExpectation::Match, true) => false,
        (ReplayExpectation::Drift, true) => true,
        (ReplayExpectation::Drift, false) => false,
    };

    Ok(ReplayOracleReport {
        name: trace.name.clone(),
        expectation: trace.expect,
        passed,
        first_run_counts,
        second_run_counts,
        protocol_fixture_refs: trace.protocol_fixture_refs.clone(),
        divergence,
    })
}

pub fn canonicalize_run(
    run: &ReplayTraceRun,
    allowlist: &[ReplayAllowlistRule],
) -> Result<JsonValue, ReplayOracleError> {
    let mut value = serde_json::to_value(run)
        .map_err(|error| ReplayOracleError::Serialization(error.to_string()))?;
    for rule in allowlist {
        apply_allowlist_rule(&mut value, rule)?;
    }
    Ok(value)
}

pub fn first_divergence(left: &JsonValue, right: &JsonValue) -> Option<ReplayDivergence> {
    first_divergence_at(left, right, String::new())
}

fn validate_trace(trace: &ReplayOracleTrace) -> Result<(), ReplayOracleError> {
    if trace.schema_version != REPLAY_TRACE_SCHEMA_VERSION {
        return Err(ReplayOracleError::InvalidTrace(format!(
            "unsupported replay trace schema_version {:?}; expected {REPLAY_TRACE_SCHEMA_VERSION}",
            trace.schema_version
        )));
    }
    if trace.name.trim().is_empty() {
        return Err(ReplayOracleError::InvalidTrace(
            "replay trace name cannot be empty".to_string(),
        ));
    }
    if trace.first_run.run_id.trim().is_empty() || trace.second_run.run_id.trim().is_empty() {
        return Err(ReplayOracleError::InvalidTrace(format!(
            "{} must include run ids for both replay executions",
            trace.name
        )));
    }
    if trace_material_count(&trace.first_run) == 0 || trace_material_count(&trace.second_run) == 0 {
        return Err(ReplayOracleError::InvalidTrace(format!(
            "{} must include replay trace material for both executions",
            trace.name
        )));
    }
    for rule in &trace.allowlist {
        if rule.path.trim().is_empty() {
            return Err(ReplayOracleError::InvalidAllowlistPath(
                "allowlist path cannot be empty".to_string(),
            ));
        }
        if rule.reason.trim().is_empty() {
            return Err(ReplayOracleError::InvalidAllowlistPath(format!(
                "allowlist path {} must explain why the field is nondeterministic",
                rule.path
            )));
        }
        parse_pointer_path(&rule.path)?;
    }
    Ok(())
}

fn trace_material_count(run: &ReplayTraceRun) -> usize {
    let counts = run.counts();
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

fn apply_allowlist_rule(
    value: &mut JsonValue,
    rule: &ReplayAllowlistRule,
) -> Result<(), ReplayOracleError> {
    let segments = parse_pointer_path(&rule.path)?;
    let replacement = rule.replacement.clone().unwrap_or_else(|| {
        json!({
            "$harn_replay_allowlisted": rule.path,
        })
    });
    let replaced = replace_matching_paths(value, &segments, &replacement);
    if replaced == 0 {
        return Err(ReplayOracleError::AllowlistPathMissing(format!(
            "allowlist path {} did not match any replay field",
            rule.path
        )));
    }
    Ok(())
}

fn parse_pointer_path(path: &str) -> Result<Vec<String>, ReplayOracleError> {
    if path == "/" {
        return Err(ReplayOracleError::InvalidAllowlistPath(
            "allowlist path cannot replace the whole run".to_string(),
        ));
    }
    if !path.starts_with('/') {
        return Err(ReplayOracleError::InvalidAllowlistPath(format!(
            "allowlist path {path:?} must start with '/'"
        )));
    }
    path.split('/')
        .skip(1)
        .map(|segment| {
            if segment.is_empty() {
                return Err(ReplayOracleError::InvalidAllowlistPath(format!(
                    "allowlist path {path:?} contains an empty segment"
                )));
            }
            Ok(segment.replace("~1", "/").replace("~0", "~"))
        })
        .collect()
}

fn replace_matching_paths(
    value: &mut JsonValue,
    segments: &[String],
    replacement: &JsonValue,
) -> usize {
    if segments.is_empty() {
        *value = replacement.clone();
        return 1;
    }

    let head = segments[0].as_str();
    let tail = &segments[1..];
    if head == "*" {
        return match value {
            JsonValue::Array(items) => items
                .iter_mut()
                .map(|item| replace_matching_paths(item, tail, replacement))
                .sum(),
            JsonValue::Object(map) => map
                .values_mut()
                .map(|item| replace_matching_paths(item, tail, replacement))
                .sum(),
            _ => 0,
        };
    }

    match value {
        JsonValue::Object(map) => map
            .get_mut(head)
            .map(|child| replace_matching_paths(child, tail, replacement))
            .unwrap_or(0),
        JsonValue::Array(items) => head
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get_mut(index))
            .map(|child| replace_matching_paths(child, tail, replacement))
            .unwrap_or(0),
        _ => 0,
    }
}

fn first_divergence_at(
    left: &JsonValue,
    right: &JsonValue,
    path: String,
) -> Option<ReplayDivergence> {
    if left == right {
        return None;
    }
    match (left, right) {
        (JsonValue::Object(left_map), JsonValue::Object(right_map)) => {
            let keys = left_map
                .keys()
                .chain(right_map.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let next_path = pointer_child(&path, &key);
                match (left_map.get(&key), right_map.get(&key)) {
                    (Some(left_child), Some(right_child)) => {
                        if let Some(divergence) =
                            first_divergence_at(left_child, right_child, next_path)
                        {
                            return Some(divergence);
                        }
                    }
                    (Some(left_child), None) => {
                        return Some(divergence(
                            next_path,
                            left_child.clone(),
                            JsonValue::Null,
                            "right run is missing this field",
                        ));
                    }
                    (None, Some(right_child)) => {
                        return Some(divergence(
                            next_path,
                            JsonValue::Null,
                            right_child.clone(),
                            "left run is missing this field",
                        ));
                    }
                    (None, None) => {}
                }
            }
            Some(divergence(
                display_path(&path),
                left.clone(),
                right.clone(),
                "objects differ",
            ))
        }
        (JsonValue::Array(left_items), JsonValue::Array(right_items)) => {
            for index in 0..left_items.len().max(right_items.len()) {
                let next_path = pointer_child(&path, &index.to_string());
                match (left_items.get(index), right_items.get(index)) {
                    (Some(left_child), Some(right_child)) => {
                        if let Some(divergence) =
                            first_divergence_at(left_child, right_child, next_path)
                        {
                            return Some(divergence);
                        }
                    }
                    (Some(left_child), None) => {
                        return Some(divergence(
                            next_path,
                            left_child.clone(),
                            JsonValue::Null,
                            "right run is missing this array element",
                        ));
                    }
                    (None, Some(right_child)) => {
                        return Some(divergence(
                            next_path,
                            JsonValue::Null,
                            right_child.clone(),
                            "left run is missing this array element",
                        ));
                    }
                    (None, None) => {}
                }
            }
            Some(divergence(
                display_path(&path),
                left.clone(),
                right.clone(),
                "arrays differ",
            ))
        }
        _ => Some(divergence(
            display_path(&path),
            left.clone(),
            right.clone(),
            "values differ",
        )),
    }
}

fn pointer_child(parent: &str, child: &str) -> String {
    let escaped = child.replace('~', "~0").replace('/', "~1");
    if parent.is_empty() {
        format!("/{escaped}")
    } else {
        format!("{parent}/{escaped}")
    }
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn divergence(
    path: String,
    left: JsonValue,
    right: JsonValue,
    message: impl Into<String>,
) -> ReplayDivergence {
    ReplayDivergence {
        path,
        left,
        right,
        message: message.into(),
    }
}

/// CH-07 (#1878): typed channel replay diagnostic codes. Surfaced by
/// `diagnose_channel_replay_drift` so audit/compliance tooling can
/// distinguish "missing match cache" from "producer drift" from
/// "batch composition drift" without parsing free-form messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code")]
pub enum ChannelReplayDiagnostic {
    /// `HARN-REP-CHN-001`: replay encountered a match for an `event_id`
    /// that isn't in the recorded emit log. Either the replay generated a
    /// new emit between baseline and replay, or the baseline lost an
    /// emit receipt mid-flight.
    #[serde(rename = "HARN-REP-CHN-001")]
    MatchWithoutEmit {
        event_id: String,
        trigger_id: String,
    },
    /// `HARN-REP-CHN-002`: the replay's emit `payload_hash` doesn't
    /// match the recorded hash for the same `event_id`. Producer code
    /// drifted between runs.
    #[serde(rename = "HARN-REP-CHN-002")]
    PayloadHashMismatch {
        event_id: String,
        recorded_hash: String,
        replay_hash: String,
    },
    /// `HARN-REP-CHN-003`: the replay's batched-match composition
    /// (`constituent_event_ids`) doesn't match the recorded batch.
    /// Aggregation policy or upstream emit ordering drifted.
    #[serde(rename = "HARN-REP-CHN-003")]
    BatchCompositionDrift {
        event_id: String,
        trigger_id: String,
        recorded: Vec<String>,
        replay: Vec<String>,
    },
}

impl ChannelReplayDiagnostic {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MatchWithoutEmit { .. } => "HARN-REP-CHN-001",
            Self::PayloadHashMismatch { .. } => "HARN-REP-CHN-002",
            Self::BatchCompositionDrift { .. } => "HARN-REP-CHN-003",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::MatchWithoutEmit {
                event_id,
                trigger_id,
            } => format!(
                "HARN-REP-CHN-001: replay matched event {event_id} for trigger \
                 {trigger_id} but no corresponding emit receipt was recorded"
            ),
            Self::PayloadHashMismatch {
                event_id,
                recorded_hash,
                replay_hash,
            } => format!(
                "HARN-REP-CHN-002: emit payload drift for event {event_id} \
                 (recorded {recorded_hash}, replay {replay_hash})"
            ),
            Self::BatchCompositionDrift {
                event_id,
                trigger_id,
                recorded,
                replay,
            } => format!(
                "HARN-REP-CHN-003: batched-match composition drift for trigger {trigger_id} \
                 anchor event {event_id} (recorded {recorded:?}, replay {replay:?})"
            ),
        }
    }
}

/// CH-07 (#1878): compare two captured channel-receipt sequences and
/// return the first replay-determinism violation, if any. Designed to
/// run after `run_replay_oracle_trace` so the canonical JSON diff catches
/// structural drift first; this function adds the channel-specific typed
/// codes (HARN-REP-CHN-001/002/003) that downstream audit tooling
/// consumes.
///
/// Returns `Ok(None)` when the two runs are channel-determinism
/// equivalent. Returns `Err(ReplayOracleError::Serialization)` only
/// when a receipt entry can't be parsed.
pub fn diagnose_channel_replay_drift(
    recorded_receipts: &[JsonValue],
    replay_receipts: &[JsonValue],
) -> Result<Option<ChannelReplayDiagnostic>, ReplayOracleError> {
    let recorded = ChannelReceiptIndex::from_entries(recorded_receipts)?;
    let replay = ChannelReceiptIndex::from_entries(replay_receipts)?;

    // HARN-REP-CHN-002: producer drift. Walk each replay emit and check
    // its hash against the recorded one.
    for (event_id, replay_hash) in &replay.emit_hashes {
        if let Some(recorded_hash) = recorded.emit_hashes.get(event_id) {
            if recorded_hash != replay_hash {
                return Ok(Some(ChannelReplayDiagnostic::PayloadHashMismatch {
                    event_id: event_id.clone(),
                    recorded_hash: recorded_hash.clone(),
                    replay_hash: replay_hash.clone(),
                }));
            }
        }
    }

    // HARN-REP-CHN-001: replay matched an event_id that no recorded
    // emit announced. Indicates either an extra emit on replay or a lost
    // emit receipt in the baseline.
    for (event_id, trigger_id) in &replay.match_triggers {
        if !recorded.emit_hashes.contains_key(event_id) {
            return Ok(Some(ChannelReplayDiagnostic::MatchWithoutEmit {
                event_id: event_id.clone(),
                trigger_id: trigger_id.clone(),
            }));
        }
    }

    // HARN-REP-CHN-003: batched-match composition drift. Compare the
    // `constituent_event_ids` for matching (event_id, trigger_id) pairs.
    for ((event_id, trigger_id), recorded_batch) in &recorded.match_batches {
        if let Some(replay_batch) = replay
            .match_batches
            .get(&(event_id.clone(), trigger_id.clone()))
        {
            if recorded_batch != replay_batch {
                return Ok(Some(ChannelReplayDiagnostic::BatchCompositionDrift {
                    event_id: event_id.clone(),
                    trigger_id: trigger_id.clone(),
                    recorded: recorded_batch.clone(),
                    replay: replay_batch.clone(),
                }));
            }
        }
    }

    Ok(None)
}

/// Internal index for `diagnose_channel_replay_drift`. Separates the
/// emit-hash lookup from the match-trigger / match-batch lookups so each
/// diagnostic walks the relevant slice only.
struct ChannelReceiptIndex {
    /// `event_id` -> `payload_hash` for every emit receipt in the run.
    emit_hashes: std::collections::BTreeMap<String, String>,
    /// `event_id` -> first observed `trigger_id` for every match
    /// receipt. Used by HARN-REP-CHN-001 — we only need to know that
    /// *some* match fired, not which one.
    match_triggers: std::collections::BTreeMap<String, String>,
    /// `(event_id, trigger_id)` -> sorted `constituent_event_ids` for
    /// every batched-match receipt. Empty list for non-batched matches
    /// (those don't carry a `batch.constituent_event_ids` array).
    match_batches: std::collections::BTreeMap<(String, String), Vec<String>>,
}

impl ChannelReceiptIndex {
    fn from_entries(entries: &[JsonValue]) -> Result<Self, ReplayOracleError> {
        let mut emit_hashes = std::collections::BTreeMap::new();
        let mut match_triggers = std::collections::BTreeMap::new();
        let mut match_batches = std::collections::BTreeMap::new();
        for entry in entries {
            let map = entry.as_object().ok_or_else(|| {
                ReplayOracleError::Serialization(format!(
                    "channel receipt entry is not an object: {entry}"
                ))
            })?;
            // Tolerate two shapes: the raw receipt JSON
            // (`ChannelEmitReceipt`/`ChannelMatchReceipt`), and an
            // `event_log.subscribe(...)` envelope where the receipt
            // lives under `payload` and `kind` classifies the variant.
            let (kind, payload) = if let Some(kind) = map.get("kind").and_then(|v| v.as_str()) {
                let payload = map.get("payload").cloned().unwrap_or_else(|| entry.clone());
                (Some(kind.to_string()), payload)
            } else if map.contains_key("payload_hash") {
                (Some("channel_emit_receipt".to_string()), entry.clone())
            } else if map.contains_key("matched_at") {
                (Some("channel_match_receipt".to_string()), entry.clone())
            } else {
                (None, entry.clone())
            };
            let Some(kind) = kind else {
                continue;
            };
            let payload_map = payload.as_object();
            match kind.as_str() {
                "channel_emit_receipt" => {
                    let Some(payload_map) = payload_map else {
                        continue;
                    };
                    let event_id = match payload_map.get("event_id").and_then(|v| v.as_str()) {
                        Some(value) => value.to_string(),
                        None => continue,
                    };
                    let hash = payload_map
                        .get("payload_hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    emit_hashes.entry(event_id).or_insert(hash);
                }
                "channel_match_receipt" => {
                    let Some(payload_map) = payload_map else {
                        continue;
                    };
                    let event_id = match payload_map.get("event_id").and_then(|v| v.as_str()) {
                        Some(value) => value.to_string(),
                        None => continue,
                    };
                    let trigger_id = payload_map
                        .get("trigger_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    match_triggers
                        .entry(event_id.clone())
                        .or_insert(trigger_id.clone());
                    let batch_ids = payload_map
                        .get("batch")
                        .and_then(|v| v.as_object())
                        .and_then(|b| b.get("constituent_event_ids"))
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            let mut ids: Vec<String> = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            ids.sort();
                            ids
                        })
                        .unwrap_or_default();
                    if !batch_ids.is_empty() {
                        match_batches
                            .entry((event_id, trigger_id))
                            .or_insert(batch_ids);
                    }
                }
                _ => {}
            }
        }
        Ok(Self {
            emit_hashes,
            match_triggers,
            match_batches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_trace() -> ReplayOracleTrace {
        ReplayOracleTrace {
            name: "fixture".to_string(),
            allowlist: vec![
                ReplayAllowlistRule {
                    path: "/run_id".to_string(),
                    reason: "run ids are allocated per execution".to_string(),
                    replacement: Some(JsonValue::String("<run-id>".to_string())),
                },
                ReplayAllowlistRule {
                    path: "/event_log_entries/*/event_id".to_string(),
                    reason: "event log offsets are backend-local".to_string(),
                    replacement: Some(JsonValue::String("<event-id>".to_string())),
                },
                ReplayAllowlistRule {
                    path: "/event_log_entries/*/occurred_at_ms".to_string(),
                    reason: "append timestamps are wall-clock observations".to_string(),
                    replacement: Some(JsonValue::String("<timestamp-ms>".to_string())),
                },
            ],
            first_run: ReplayTraceRun {
                run_id: "run-a".to_string(),
                event_log_entries: vec![json!({
                    "event_id": 10,
                    "topic": "trigger.outbox",
                    "kind": "dispatch_succeeded",
                    "occurred_at_ms": 1000,
                    "payload": {"binding_id": "demo", "status": "dispatched"}
                })],
                ..ReplayTraceRun::default()
            },
            second_run: ReplayTraceRun {
                run_id: "run-b".to_string(),
                event_log_entries: vec![json!({
                    "event_id": 42,
                    "topic": "trigger.outbox",
                    "kind": "dispatch_succeeded",
                    "occurred_at_ms": 2000,
                    "payload": {"binding_id": "demo", "status": "dispatched"}
                })],
                ..ReplayTraceRun::default()
            },
            ..ReplayOracleTrace::default()
        }
    }

    #[test]
    fn canonical_comparison_allows_explicit_nondeterministic_fields() {
        let report = run_replay_oracle_trace(&base_trace()).expect("oracle succeeds");
        assert!(report.passed, "{report:?}");
        assert_eq!(report.divergence, None);
    }

    #[test]
    fn persona_runtime_states_are_first_class_replay_material() {
        let mut trace = base_trace();
        trace.first_run.persona_runtime_states = vec![json!({
            "name": "merge_captain",
            "state": "idle",
            "queued_work": [],
            "handoff_inbox": [],
            "budget": {"spent_today_usd": 0.01},
        })];
        trace.second_run.persona_runtime_states = trace.first_run.persona_runtime_states.clone();

        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");

        assert!(report.passed, "{report:?}");
        assert_eq!(report.first_run_counts.persona_runtime_states, 1);
    }

    #[test]
    fn meaningful_drift_reports_first_divergent_path() {
        let mut trace = base_trace();
        trace.second_run.event_log_entries[0]["payload"]["status"] = json!("dlq");

        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");

        assert!(!report.passed);
        let divergence = report.divergence.expect("drift is reported");
        assert_eq!(divergence.path, "/event_log_entries/0/payload/status");
        assert_eq!(divergence.left, json!("dispatched"));
        assert_eq!(divergence.right, json!("dlq"));
    }

    #[test]
    fn expected_drift_fixture_passes_only_when_drift_is_detected() {
        let mut trace = base_trace();
        trace.expect = ReplayExpectation::Drift;
        trace.second_run.event_log_entries[0]["payload"]["status"] = json!("dlq");

        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");

        assert!(report.passed);
        assert!(report.divergence.is_some());
    }

    fn channel_emit_receipt(event_id: &str, payload_hash: &str) -> JsonValue {
        json!({
            "kind": "channel_emit_receipt",
            "payload": {
                "event_id": event_id,
                "payload_hash": payload_hash,
                "name_resolved": "tenant:default:ch.test",
                "scope": "tenant",
                "scope_id": "default",
                "topic": "channels.tenant.default.ch.test",
                "inserted": true,
            },
        })
    }

    fn channel_match_receipt(
        event_id: &str,
        trigger_id: &str,
        constituent_ids: Option<Vec<&str>>,
    ) -> JsonValue {
        let mut payload = serde_json::Map::new();
        payload.insert("event_id".to_string(), json!(event_id));
        payload.insert("trigger_id".to_string(), json!(trigger_id));
        payload.insert("handler_kind".to_string(), json!("local"));
        if let Some(ids) = constituent_ids {
            payload.insert(
                "batch".to_string(),
                json!({
                    "count": ids.len(),
                    "constituent_event_ids": ids,
                }),
            );
        }
        json!({
            "kind": "channel_match_receipt",
            "payload": JsonValue::Object(payload),
        })
    }

    #[test]
    fn channel_replay_diagnostic_clean_runs_have_no_drift() {
        let recorded = vec![
            channel_emit_receipt("evt-1", "sha256:a"),
            channel_match_receipt("evt-1", "trig-x", None),
        ];
        let replay = recorded.clone();
        let diagnostic = diagnose_channel_replay_drift(&recorded, &replay).unwrap();
        assert!(diagnostic.is_none(), "{diagnostic:?}");
    }

    #[test]
    fn channel_replay_diagnostic_001_match_without_emit() {
        let recorded = vec![channel_emit_receipt("evt-1", "sha256:a")];
        // Replay matched a different event id with no emit recorded for it.
        let replay = vec![
            channel_emit_receipt("evt-1", "sha256:a"),
            channel_match_receipt("evt-2", "trig-x", None),
        ];
        let diagnostic = diagnose_channel_replay_drift(&recorded, &replay)
            .unwrap()
            .expect("drift");
        assert_eq!(diagnostic.code(), "HARN-REP-CHN-001");
        assert!(matches!(
            diagnostic,
            ChannelReplayDiagnostic::MatchWithoutEmit { ref event_id, .. } if event_id == "evt-2"
        ));
    }

    #[test]
    fn channel_replay_diagnostic_002_payload_hash_drift() {
        let recorded = vec![channel_emit_receipt("evt-1", "sha256:a")];
        let replay = vec![channel_emit_receipt("evt-1", "sha256:b")];
        let diagnostic = diagnose_channel_replay_drift(&recorded, &replay)
            .unwrap()
            .expect("drift");
        assert_eq!(diagnostic.code(), "HARN-REP-CHN-002");
        let message = diagnostic.message();
        assert!(message.contains("HARN-REP-CHN-002"));
        assert!(message.contains("evt-1"));
    }

    #[test]
    fn channel_replay_diagnostic_003_batch_composition_drift() {
        let recorded = vec![
            channel_emit_receipt("evt-1", "sha256:a"),
            channel_emit_receipt("evt-2", "sha256:b"),
            channel_emit_receipt("evt-3", "sha256:c"),
            channel_match_receipt("evt-1", "trig-x", Some(vec!["evt-1", "evt-2", "evt-3"])),
        ];
        let replay = vec![
            channel_emit_receipt("evt-1", "sha256:a"),
            channel_emit_receipt("evt-2", "sha256:b"),
            channel_emit_receipt("evt-3", "sha256:c"),
            // Replay aggregated a different subset.
            channel_match_receipt("evt-1", "trig-x", Some(vec!["evt-1", "evt-2"])),
        ];
        let diagnostic = diagnose_channel_replay_drift(&recorded, &replay)
            .unwrap()
            .expect("drift");
        assert_eq!(diagnostic.code(), "HARN-REP-CHN-003");
    }

    #[test]
    fn channel_receipts_count_first_class_replay_material() {
        let mut trace = base_trace();
        trace.first_run.channel_receipts = vec![channel_emit_receipt("evt-1", "sha256:a")];
        trace.second_run.channel_receipts = trace.first_run.channel_receipts.clone();
        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");
        assert!(report.passed, "{report:?}");
        assert_eq!(report.first_run_counts.channel_receipts, 1);
        assert_eq!(report.second_run_counts.channel_receipts, 1);
    }

    #[test]
    fn lifecycle_receipts_are_first_class_replay_material() {
        let mut trace = base_trace();
        trace.allowlist.push(ReplayAllowlistRule {
            path: "/lifecycle_receipts/*/payload/suspended_at/signature".to_string(),
            reason: "per-process signing salt".to_string(),
            replacement: Some(JsonValue::String("<signature>".to_string())),
        });
        trace.allowlist.push(ReplayAllowlistRule {
            path: "/lifecycle_receipts/*/payload/suspended_at/at_ms".to_string(),
            reason: "wall-clock at_ms varies per record".to_string(),
            replacement: Some(JsonValue::String("<at-ms>".to_string())),
        });
        trace.allowlist.push(ReplayAllowlistRule {
            path: "/lifecycle_receipts/*/payload/suspended_at/at".to_string(),
            reason: "wall-clock at varies per record".to_string(),
            replacement: Some(JsonValue::String("<at>".to_string())),
        });
        let receipt = json!({
            "seq": 1,
            "kind": "suspension_receipt",
            "payload": {
                "handle": "worker://x/1",
                "session_id": null,
                "initiator": "operator",
                "initiator_id": "op-1",
                "reason": "stop",
                "suspended_at": {
                    "at_ms": 100,
                    "at": "1970-01-01T00:00:00.1Z",
                    "algorithm": "hmac-sha256",
                    "key_id": "local-session",
                    "signature": "sha256:deadbeef",
                },
            },
        });
        trace.first_run.lifecycle_receipts = vec![receipt.clone()];
        trace.second_run.lifecycle_receipts = vec![receipt];

        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");

        assert!(report.passed, "{report:?}");
        assert_eq!(report.first_run_counts.lifecycle_receipts, 1);
        assert_eq!(report.second_run_counts.lifecycle_receipts, 1);
    }

    #[test]
    fn lifecycle_receipt_input_hash_drift_is_detected() {
        let mut trace = base_trace();
        trace.first_run.lifecycle_receipts = vec![json!({
            "seq": 1,
            "kind": "resumption_receipt",
            "payload": {
                "handle": "worker://x/1",
                "initiator": "operator",
                "initiator_id": "op-1",
                "input_hash": "sha256:aaaa",
                "continue_transcript": true,
            },
        })];
        trace.second_run.lifecycle_receipts = vec![json!({
            "seq": 1,
            "kind": "resumption_receipt",
            "payload": {
                "handle": "worker://x/1",
                "initiator": "operator",
                "initiator_id": "op-1",
                "input_hash": "sha256:bbbb",
                "continue_transcript": true,
            },
        })];

        let report = run_replay_oracle_trace(&trace).expect("oracle succeeds");

        assert!(!report.passed);
        let divergence = report.divergence.expect("drift is reported");
        assert_eq!(divergence.path, "/lifecycle_receipts/0/payload/input_hash");
    }

    #[test]
    fn allowlist_paths_must_match_real_fields() {
        let mut trace = base_trace();
        trace.allowlist.push(ReplayAllowlistRule {
            path: "/llm_interactions/*/latency_ms".to_string(),
            reason: "latency is nondeterministic".to_string(),
            replacement: None,
        });

        let error = run_replay_oracle_trace(&trace).expect_err("missing path should fail");

        assert!(matches!(error, ReplayOracleError::AllowlistPathMissing(_)));
    }
}
