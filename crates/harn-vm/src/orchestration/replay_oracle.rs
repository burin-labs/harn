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
    pub agent_transcript_deltas: Vec<JsonValue>,
    pub final_artifacts: Vec<JsonValue>,
    pub policy_decisions: Vec<JsonValue>,
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
            agent_transcript_deltas: self.agent_transcript_deltas.len(),
            final_artifacts: self.final_artifacts.len(),
            policy_decisions: self.policy_decisions.len(),
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
    pub agent_transcript_deltas: usize,
    pub final_artifacts: usize,
    pub policy_decisions: usize,
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
        + counts.agent_transcript_deltas
        + counts.final_artifacts
        + counts.policy_decisions
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
