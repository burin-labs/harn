//! Canonical session bundle export/import support.
//!
//! A session bundle is the portable JSON envelope that downstream hosts use
//! for local exports, sanitized shares, and replay-only handoffs. The bundle
//! is intentionally built from existing Harn run-record surfaces so replay,
//! transcripts, tool calls, HITL questions, and observability do not fork into
//! a second persistence model.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::agent_events::AgentEvent;
use crate::event_log::sanitize_topic_component;
use crate::orchestration::{
    derive_run_observability, new_id, now_rfc3339, AgentSessionReplayEvent, ReplayFixture,
    RunCheckpointRecord, RunHitlQuestionRecord, RunObservabilityRecord, RunRecord,
    RunTraceSpanRecord, RunTransitionRecord, RunVerificationOutcomeRecord, ToolCallRecord,
};
use crate::redact::{RedactionPolicy, REDACTED_PLACEHOLDER};
use crate::workspace_anchor::{anchor_from_transcript_metadata_json, MountedRoot, WorkspaceAnchor};

mod schema;
pub use schema::{session_bundle_schema, session_bundle_schema_pretty};

#[cfg(test)]
mod tests;

pub const SESSION_BUNDLE_TYPE: &str = "harn_session_bundle";
pub const SESSION_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const SESSION_BUNDLE_SCHEMA_ID: &str = "https://harnlang.com/schemas/session-bundle.v1.json";
pub const REPLAY_ONLY_PLACEHOLDER: &str = "[withheld]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionBundleExportMode {
    Local,
    Sanitized,
    ReplayOnly,
}

impl SessionBundleExportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sanitized => "sanitized",
            Self::ReplayOnly => "replay_only",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionBundleExportOptions {
    pub mode: SessionBundleExportMode,
    pub include_attachments: bool,
    pub redaction_policy: RedactionPolicy,
}

impl Default for SessionBundleExportOptions {
    fn default() -> Self {
        Self {
            mode: SessionBundleExportMode::Sanitized,
            include_attachments: false,
            redaction_policy: RedactionPolicy::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionBundleValidationOptions {
    pub allow_unsafe_secret_markers: bool,
    pub redaction_policy: RedactionPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionBundle {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: u32,
    pub bundle_id: String,
    pub created_at: String,
    pub producer: BundleProducer,
    pub source: BundleSource,
    pub runtime: BundleRuntime,
    pub workspace: Option<BundleWorkspace>,
    pub transcript: BundleTranscript,
    pub tools: BundleTools,
    pub permissions: Vec<BundlePermission>,
    pub replay: BundleReplay,
    pub redaction: RedactionManifest,
    pub attachments: Vec<BundleAttachment>,
    pub metadata: BTreeMap<String, JsonValue>,
}

impl Default for SessionBundle {
    fn default() -> Self {
        Self {
            type_name: SESSION_BUNDLE_TYPE.to_string(),
            schema_version: SESSION_BUNDLE_SCHEMA_VERSION,
            bundle_id: String::new(),
            created_at: String::new(),
            producer: BundleProducer::default(),
            source: BundleSource::default(),
            runtime: BundleRuntime::default(),
            workspace: None,
            transcript: BundleTranscript::default(),
            tools: BundleTools::default(),
            permissions: Vec::new(),
            replay: BundleReplay::default(),
            redaction: RedactionManifest::default(),
            attachments: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleProducer {
    pub name: String,
    pub version: String,
    pub schema_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleSource {
    pub kind: String,
    pub run_record_id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub persisted_path: Option<String>,
    pub root_run_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub child_run_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BundleRuntime {
    pub harn_version: String,
    pub provider_models: Vec<String>,
    pub usage: Option<BundleUsage>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BundleUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub call_count: i64,
    pub total_duration_ms: i64,
    pub total_cost: f64,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleWorkspace {
    /// Primary workspace path the session is anchored against.
    pub primary: Option<String>,
    /// Additional roots mounted alongside the primary anchor.
    #[serde(default)]
    pub additional_roots: Vec<BundleMountedRoot>,
    /// RFC3339 timestamp when the anchor was set.
    pub anchored_at: Option<String>,
    /// Bundle workspace policy label. Currently always
    /// `"safe_identity_only"`; future revisions may tie this to mount
    /// modes once the PathScope matcher (#2216) lands.
    pub policy: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleMountedRoot {
    pub path: String,
    pub mount_mode: String,
    pub mounted_at: String,
}

impl From<&MountedRoot> for BundleMountedRoot {
    fn from(root: &MountedRoot) -> Self {
        Self {
            path: root.path.to_string_lossy().into_owned(),
            mount_mode: root.mount_mode.as_str().to_string(),
            mounted_at: root.mounted_at.clone(),
        }
    }
}

impl From<&WorkspaceAnchor> for BundleWorkspace {
    fn from(anchor: &WorkspaceAnchor) -> Self {
        Self {
            primary: Some(anchor.primary.to_string_lossy().into_owned()),
            additional_roots: anchor.additional_roots.iter().map(Into::into).collect(),
            anchored_at: Some(anchor.anchored_at.clone()),
            policy: "safe_identity_only".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleTranscript {
    pub sections: Vec<BundleTranscriptSection>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleTranscriptSection {
    pub id: String,
    pub label: String,
    pub scope: String,
    pub location: String,
    pub summary: Option<String>,
    pub messages: Vec<JsonValue>,
    pub events: Vec<JsonValue>,
    pub assets: Vec<JsonValue>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleTools {
    pub schemas: Vec<BundleJsonEntry>,
    pub calls: Vec<BundleToolCall>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleJsonEntry {
    pub source: String,
    pub index: usize,
    pub value: JsonValue,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleToolCall {
    pub tool_name: String,
    pub tool_use_id: String,
    pub args_hash: String,
    pub result: String,
    pub is_rejected: bool,
    pub duration_ms: u64,
    pub iteration: usize,
    pub timestamp: String,
}

impl From<&ToolCallRecord> for BundleToolCall {
    fn from(record: &ToolCallRecord) -> Self {
        Self {
            tool_name: record.tool_name.clone(),
            tool_use_id: record.tool_use_id.clone(),
            args_hash: record.args_hash.clone(),
            result: record.result.clone(),
            is_rejected: record.is_rejected,
            duration_ms: record.duration_ms,
            iteration: record.iteration,
            timestamp: record.timestamp.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundlePermission {
    pub kind: String,
    pub source: String,
    pub request_id: Option<String>,
    pub agent: Option<String>,
    pub payload: JsonValue,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleReplay {
    pub replay_fixture: Option<ReplayFixture>,
    pub run_record: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observability: Option<RunObservabilityRecord>,
    pub verification_outcomes: Vec<RunVerificationOutcomeRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub worker_snapshots: Vec<BundleWorkerSnapshot>,
    pub event_log_pointers: Vec<BundleEventLogPointer>,
    pub transitions: Vec<RunTransitionRecord>,
    pub checkpoints: Vec<RunCheckpointRecord>,
    pub trace_spans: Vec<RunTraceSpanRecord>,
    pub deterministic_events: Vec<BundleJsonEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleWorkerSnapshot {
    pub worker_id: String,
    pub worker_name: String,
    pub status: String,
    pub snapshot_ref: String,
    pub source_path: Option<String>,
    pub value: JsonValue,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MaterializedWorkerSnapshot {
    pub worker_id: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleEventLogPointer {
    pub kind: String,
    pub topic: Option<String>,
    pub path: Option<String>,
    pub location: String,
    pub available: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RedactionManifest {
    pub mode: String,
    pub policy: String,
    pub placeholder: String,
    pub entries: Vec<RedactionEntry>,
    pub unsafe_secret_markers_rejected: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RedactionEntry {
    pub path: String,
    pub class: String,
    pub action: String,
    pub replacement: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BundleAttachment {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub stage: Option<String>,
    pub text: Option<String>,
    pub data: Option<JsonValue>,
    pub metadata: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBundleError {
    Decode(String),
    Encode(String),
    MissingRequired(String),
    UnsupportedSchemaVersion { found: u64, supported: u32 },
    InvalidType { path: String, expected: String },
    UnsafeSecretMarker { path: String, excerpt: String },
    MissingRunRecord,
    MissingSessionEvents { session_id: String },
}

impl fmt::Display for SessionBundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "failed to decode session bundle: {error}"),
            Self::Encode(error) => write!(f, "failed to encode session bundle: {error}"),
            Self::MissingRequired(path) => {
                write!(f, "session bundle is missing required field {path}")
            }
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "unsupported session bundle schema_version {found}; this build supports <= {supported}"
            ),
            Self::InvalidType { path, expected } => {
                write!(f, "session bundle field {path} must be {expected}")
            }
            Self::UnsafeSecretMarker { path, excerpt } => write!(
                f,
                "session bundle contains an unsafe unredacted secret marker at {path}: {excerpt}"
            ),
            Self::MissingRunRecord => write!(f, "session bundle does not include an importable run record"),
            Self::MissingSessionEvents { session_id } => write!(
                f,
                "event log does not contain replayable events for session_id {session_id:?}"
            ),
        }
    }
}

impl std::error::Error for SessionBundleError {}

pub fn export_run_record_bundle(
    run: &RunRecord,
    options: &SessionBundleExportOptions,
) -> Result<SessionBundle, SessionBundleError> {
    let run_record_value =
        serde_json::to_value(run).map_err(|error| SessionBundleError::Encode(error.to_string()))?;
    let mut bundle = raw_bundle_from_run(run, run_record_value, options.include_attachments)?;
    let mut bundle_value = serde_json::to_value(&bundle)
        .map_err(|error| SessionBundleError::Encode(error.to_string()))?;

    let mut manifest = RedactionManifest {
        mode: options.mode.as_str().to_string(),
        policy: if matches!(options.mode, SessionBundleExportMode::Local) {
            "none".to_string()
        } else {
            "harn_vm::redact::RedactionPolicy::default+session_bundle_local_paths".to_string()
        },
        placeholder: REDACTED_PLACEHOLDER.to_string(),
        entries: Vec::new(),
        unsafe_secret_markers_rejected: !matches!(options.mode, SessionBundleExportMode::Local),
    };

    if !matches!(options.mode, SessionBundleExportMode::Local) {
        let redaction_policy = bundle_redaction_policy(&options.redaction_policy);
        redact_json_with_manifest(
            &mut bundle_value,
            "$",
            &redaction_policy,
            &mut manifest.entries,
        );
        redact_bundle_pointer_paths_json(&mut bundle_value, "$", &mut manifest.entries);
    }
    if matches!(options.mode, SessionBundleExportMode::ReplayOnly) {
        withhold_replay_only_json(&mut bundle_value, "$", &mut manifest.entries);
    }
    set_json_path(
        &mut bundle_value,
        &["redaction"],
        serde_json::to_value(&manifest)
            .map_err(|error| SessionBundleError::Encode(error.to_string()))?,
    );
    bundle = serde_json::from_value(bundle_value)
        .map_err(|error| SessionBundleError::Decode(error.to_string()))?;
    Ok(bundle)
}

pub fn validate_session_bundle_value(
    value: &JsonValue,
    options: &SessionBundleValidationOptions,
) -> Result<SessionBundle, SessionBundleError> {
    require_field(value, "_type")?;
    require_field(value, "schema_version")?;
    require_field(value, "bundle_id")?;
    require_field(value, "created_at")?;
    require_field(value, "producer")?;
    require_field(value, "source")?;
    require_field(value, "runtime")?;
    require_field(value, "transcript")?;
    require_field(value, "tools")?;
    require_field(value, "permissions")?;
    require_field(value, "replay")?;
    require_field(value, "redaction")?;
    require_field(value, "attachments")?;
    require_nested_field(value, &["producer", "name"])?;
    require_nested_field(value, &["producer", "version"])?;
    require_nested_field(value, &["producer", "schema_id"])?;
    require_nested_field(value, &["source", "kind"])?;
    require_nested_field(value, &["source", "run_record_id"])?;
    require_nested_field(value, &["source", "workflow_id"])?;
    require_nested_field(value, &["source", "task"])?;
    require_nested_field(value, &["source", "status"])?;
    require_nested_field(value, &["runtime", "harn_version"])?;
    require_nested_field(value, &["runtime", "provider_models"])?;
    require_nested_field(value, &["transcript", "sections"])?;
    require_nested_field(value, &["tools", "schemas"])?;
    require_nested_field(value, &["tools", "calls"])?;
    require_nested_field(value, &["replay", "event_log_pointers"])?;
    require_nested_field(value, &["replay", "transitions"])?;
    require_nested_field(value, &["replay", "checkpoints"])?;
    require_nested_field(value, &["replay", "trace_spans"])?;
    require_nested_field(value, &["replay", "deterministic_events"])?;
    require_nested_field(value, &["redaction", "mode"])?;
    require_nested_field(value, &["redaction", "policy"])?;
    require_nested_field(value, &["redaction", "placeholder"])?;
    require_nested_field(value, &["redaction", "entries"])?;
    require_nested_field(value, &["redaction", "unsafe_secret_markers_rejected"])?;

    let type_name = value
        .get("_type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| SessionBundleError::InvalidType {
            path: "$._type".to_string(),
            expected: "string".to_string(),
        })?;
    if type_name != SESSION_BUNDLE_TYPE {
        return Err(SessionBundleError::InvalidType {
            path: "$._type".to_string(),
            expected: format!("\"{SESSION_BUNDLE_TYPE}\""),
        });
    }

    let version = value
        .get("schema_version")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| SessionBundleError::InvalidType {
            path: "$.schema_version".to_string(),
            expected: "positive integer".to_string(),
        })?;
    if version == 0 || version > u64::from(SESSION_BUNDLE_SCHEMA_VERSION) {
        return Err(SessionBundleError::UnsupportedSchemaVersion {
            found: version,
            supported: SESSION_BUNDLE_SCHEMA_VERSION,
        });
    }

    if !options.allow_unsafe_secret_markers {
        reject_unredacted_secret_markers(value, "$", &options.redaction_policy)?;
    }

    serde_json::from_value::<SessionBundle>(value.clone())
        .map_err(|error| SessionBundleError::Decode(error.to_string()))
}

pub fn validate_session_bundle_str(
    content: &str,
    options: &SessionBundleValidationOptions,
) -> Result<SessionBundle, SessionBundleError> {
    let value: JsonValue = serde_json::from_str(content)
        .map_err(|error| SessionBundleError::Decode(error.to_string()))?;
    validate_session_bundle_value(&value, options)
}

pub fn import_run_record_value(bundle: &SessionBundle) -> Result<JsonValue, SessionBundleError> {
    let replay_observability = replay_observability_for_import(&bundle.replay);
    if let Some(mut run_record) = bundle.replay.run_record.clone() {
        let should_fill_observability = match run_record.get("observability") {
            Some(value) => value.is_null(),
            None => true,
        };
        if should_fill_observability {
            if let (JsonValue::Object(map), Some(observability)) =
                (&mut run_record, replay_observability.as_ref())
            {
                map.insert(
                    "observability".to_string(),
                    serde_json::to_value(observability)
                        .map_err(|error| SessionBundleError::Encode(error.to_string()))?,
                );
            }
        }
        return Ok(run_record);
    }
    if let Some(fixture) = &bundle.replay.replay_fixture {
        let transcript = bundle.transcript.sections.first().map(|section| {
            json!({
                "_type": "transcript",
                "messages": section.messages.clone(),
                "events": section.events.clone(),
                "assets": section.assets.clone(),
                "summary": section.summary.clone(),
                "metadata": section.metadata.clone(),
            })
        });
        let hitl_questions = bundle
            .permissions
            .iter()
            .filter(|permission| permission.kind == "hitl_question")
            .map(|permission| permission.payload.clone())
            .collect::<Vec<_>>();
        return Ok(json!({
            "_type": "run_record",
            "id": bundle.source.run_record_id.clone(),
            "workflow_id": bundle.source.workflow_id.clone(),
            "workflow_name": bundle.source.workflow_name.clone(),
            "task": bundle.source.task.clone(),
            "status": bundle.source.status.clone(),
            "started_at": bundle.source.started_at.clone(),
            "finished_at": bundle.source.finished_at.clone(),
            "stages": [],
            "transitions": bundle.replay.transitions.clone(),
            "checkpoints": bundle.replay.checkpoints.clone(),
            "pending_nodes": [],
            "completed_nodes": [],
            "child_runs": [],
            "artifacts": [],
            "handoffs": [],
            "policy": {},
            "transcript": transcript,
            "usage": bundle.runtime.usage.clone(),
            "replay_fixture": fixture,
            "observability": replay_observability,
            "trace_spans": bundle.replay.trace_spans.clone(),
            "tool_recordings": bundle.tools.calls.clone(),
            "hitl_questions": hitl_questions,
            "persona_runtime": [],
            "metadata": {
                "imported_from_session_bundle": bundle.bundle_id.clone(),
                "session_bundle_schema_version": bundle.schema_version,
                "worker_snapshot_count": bundle.replay.worker_snapshots.len(),
            }
        }));
    }
    Err(SessionBundleError::MissingRunRecord)
}

pub fn import_run_record_value_with_materialized_worker_snapshots(
    bundle: &SessionBundle,
    materialized: &[MaterializedWorkerSnapshot],
) -> Result<JsonValue, SessionBundleError> {
    let mut run_record = import_run_record_value(bundle)?;
    apply_materialized_worker_snapshot_paths(&mut run_record, materialized);
    Ok(run_record)
}

pub fn materialize_worker_snapshots(
    bundle: &SessionBundle,
    out_dir: &Path,
) -> Result<Vec<MaterializedWorkerSnapshot>, SessionBundleError> {
    if bundle.replay.worker_snapshots.is_empty() {
        return Ok(Vec::new());
    }
    fs::create_dir_all(out_dir).map_err(|error| {
        SessionBundleError::Encode(format!(
            "failed to create worker snapshot directory {}: {error}",
            out_dir.display()
        ))
    })?;

    let mut materialized = Vec::new();
    for (index, snapshot) in bundle.replay.worker_snapshots.iter().enumerate() {
        let worker_id = if snapshot.worker_id.trim().is_empty() {
            format!("worker_{index}")
        } else {
            snapshot.worker_id.clone()
        };
        let path = out_dir.join(worker_snapshot_file_name(&worker_id, index));
        let value = worker_snapshot_value_for_import(&snapshot.value, &path);
        let rendered = serde_json::to_string_pretty(&value)
            .map(|json| format!("{json}\n"))
            .map_err(|error| SessionBundleError::Encode(error.to_string()))?;
        fs::write(&path, rendered).map_err(|error| {
            SessionBundleError::Encode(format!(
                "failed to write worker snapshot {}: {error}",
                path.display()
            ))
        })?;
        materialized.push(MaterializedWorkerSnapshot {
            worker_id,
            path: path.to_string_lossy().into_owned(),
        });
    }
    Ok(materialized)
}

fn apply_materialized_worker_snapshot_paths(
    run_record: &mut JsonValue,
    materialized: &[MaterializedWorkerSnapshot],
) {
    if materialized.is_empty() {
        return;
    }

    let paths_by_worker_id = materialized
        .iter()
        .filter(|snapshot| !snapshot.worker_id.is_empty())
        .map(|snapshot| (snapshot.worker_id.as_str(), snapshot.path.as_str()))
        .collect::<BTreeMap<_, _>>();
    if paths_by_worker_id.is_empty() {
        return;
    }

    rewrite_worker_snapshot_paths(run_record.get_mut("child_runs"), &paths_by_worker_id);
    rewrite_worker_snapshot_paths(
        run_record
            .get_mut("observability")
            .and_then(|observability| observability.get_mut("worker_lineage")),
        &paths_by_worker_id,
    );
}

fn rewrite_worker_snapshot_paths(
    records: Option<&mut JsonValue>,
    paths_by_worker_id: &BTreeMap<&str, &str>,
) {
    let Some(records) = records.and_then(JsonValue::as_array_mut) else {
        return;
    };
    for record in records {
        let Some(worker_id) = record.get("worker_id").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(path) = paths_by_worker_id.get(worker_id) else {
            continue;
        };
        if let JsonValue::Object(map) = record {
            map.insert(
                "snapshot_path".to_string(),
                JsonValue::String((*path).to_string()),
            );
        }
    }
}

fn replay_observability_for_import(replay: &BundleReplay) -> Option<RunObservabilityRecord> {
    let mut observability = replay.observability.clone().unwrap_or_default();
    let has_observability = replay.observability.is_some();
    let has_verification_outcomes = !replay.verification_outcomes.is_empty();
    if !has_observability && !has_verification_outcomes {
        return None;
    }
    if observability.schema_version == 0 {
        observability.schema_version = 4;
    }
    if observability.verification_outcomes.is_empty() && has_verification_outcomes {
        observability.verification_outcomes = replay.verification_outcomes.clone();
    }
    Some(observability)
}

pub fn session_bundle_from_agent_session_events(
    session_id: &str,
    events: &[AgentSessionReplayEvent],
) -> Result<SessionBundle, SessionBundleError> {
    if events.is_empty() {
        return Err(SessionBundleError::MissingSessionEvents {
            session_id: session_id.to_string(),
        });
    }

    let stable_id = sanitize_topic_component(session_id);
    let started_at = rfc3339_from_epoch_ms(events[0].occurred_at_ms);
    let finished_at = events.iter().rev().find_map(|entry| match &entry.event {
        AgentEvent::SessionClosed { .. } => Some(rfc3339_from_epoch_ms(entry.occurred_at_ms)),
        _ => None,
    });
    let status = events
        .iter()
        .rev()
        .find_map(|entry| match &entry.event {
            AgentEvent::SessionClosed { status, .. } if !status.is_empty() => Some(status.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "completed".to_string());
    let run_id = session_id.to_string();
    let workflow_id = "agent_session".to_string();
    let created_at = finished_at.clone().unwrap_or_else(|| started_at.clone());
    let transcript_events = transcript_events_from_agent_session(events)?;
    let transcript_messages = transcript_messages_from_agent_session(events);
    let mut transcript_metadata = BTreeMap::new();
    transcript_metadata.insert("session_id".to_string(), json!(session_id));
    transcript_metadata.insert(
        "source".to_string(),
        json!("events.sqlite observability.agent_events topic"),
    );

    let replay_fixture = ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: format!("fixture_from_session_{stable_id}"),
        source_run_id: run_id.clone(),
        workflow_id: workflow_id.clone(),
        workflow_name: Some(format!("Agent session {session_id}")),
        created_at: created_at.clone(),
        eval_kind: Some("replay".to_string()),
        expected_status: status.clone(),
        ..ReplayFixture::default()
    };

    Ok(SessionBundle {
        bundle_id: format!("bundle_from_session_{stable_id}"),
        created_at,
        producer: BundleProducer {
            name: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            schema_id: SESSION_BUNDLE_SCHEMA_ID.to_string(),
        },
        source: BundleSource {
            kind: "event_log_session".to_string(),
            run_record_id: run_id,
            workflow_id,
            workflow_name: Some(format!("Agent session {session_id}")),
            task: task_from_agent_session(events)
                .unwrap_or_else(|| format!("Agent session {session_id}")),
            status,
            started_at,
            finished_at,
            ..BundleSource::default()
        },
        runtime: BundleRuntime {
            harn_version: env!("CARGO_PKG_VERSION").to_string(),
            ..BundleRuntime::default()
        },
        transcript: BundleTranscript {
            sections: vec![BundleTranscriptSection {
                id: "agent_events".to_string(),
                label: "Agent event log".to_string(),
                scope: "session".to_string(),
                location: format!(
                    "observability.agent_events.{}",
                    sanitize_topic_component(session_id)
                ),
                summary: None,
                messages: transcript_messages,
                events: transcript_events,
                assets: Vec::new(),
                metadata: transcript_metadata,
            }],
        },
        permissions: permissions_from_agent_session(events),
        replay: BundleReplay {
            replay_fixture: Some(replay_fixture),
            event_log_pointers: vec![BundleEventLogPointer {
                kind: "agent_events".to_string(),
                topic: Some(format!(
                    "observability.agent_events.{}",
                    sanitize_topic_component(session_id)
                )),
                path: None,
                location: "events.sqlite".to_string(),
                available: true,
            }],
            deterministic_events: deterministic_events_from_agent_session(events)?,
            ..BundleReplay::default()
        },
        ..SessionBundle::default()
    })
}

pub fn import_run_record_from_agent_session_events(
    session_id: &str,
    events: &[AgentSessionReplayEvent],
) -> Result<RunRecord, SessionBundleError> {
    let bundle = session_bundle_from_agent_session_events(session_id, events)?;
    let run_record = import_run_record_value(&bundle)?;
    serde_json::from_value(run_record)
        .map_err(|error| SessionBundleError::Decode(error.to_string()))
}

fn transcript_events_from_agent_session(
    events: &[AgentSessionReplayEvent],
) -> Result<Vec<JsonValue>, SessionBundleError> {
    events
        .iter()
        .map(|entry| {
            let event = serde_json::to_value(&entry.event)
                .map_err(|error| SessionBundleError::Encode(error.to_string()))?;
            Ok(json!({
                "event_id": entry.event_id,
                "kind": entry.kind,
                "occurred_at_ms": entry.occurred_at_ms,
                "event": event,
            }))
        })
        .collect()
}

fn transcript_messages_from_agent_session(events: &[AgentSessionReplayEvent]) -> Vec<JsonValue> {
    events
        .iter()
        .filter_map(|entry| match &entry.event {
            AgentEvent::UserMessage { content, .. } => Some(json!({
                "role": "user",
                "content": content,
            })),
            AgentEvent::AgentMessageChunk { content, .. } if !content.is_empty() => Some(json!({
                "role": "assistant",
                "content": content,
            })),
            _ => None,
        })
        .collect()
}

fn permissions_from_agent_session(events: &[AgentSessionReplayEvent]) -> Vec<BundlePermission> {
    let mut permissions = Vec::new();
    for entry in events {
        if let AgentEvent::HitlRequested {
            request_id,
            kind,
            payload,
            ..
        } = &entry.event
        {
            permissions.push(BundlePermission {
                kind: "hitl_question".to_string(),
                source: "agent_events".to_string(),
                request_id: Some(request_id.clone()),
                agent: None,
                payload: json!({
                    "kind": kind,
                    "payload": payload,
                    "event_id": entry.event_id,
                    "occurred_at_ms": entry.occurred_at_ms,
                }),
            });
        }
    }
    permissions
}

fn deterministic_events_from_agent_session(
    events: &[AgentSessionReplayEvent],
) -> Result<Vec<BundleJsonEntry>, SessionBundleError> {
    transcript_events_from_agent_session(events).map(|entries| {
        entries
            .into_iter()
            .enumerate()
            .map(|(index, value)| BundleJsonEntry {
                source: "events.sqlite.agent_events".to_string(),
                index,
                value,
            })
            .collect()
    })
}

fn task_from_agent_session(events: &[AgentSessionReplayEvent]) -> Option<String> {
    events.iter().find_map(|entry| match &entry.event {
        AgentEvent::UserMessage { content, .. } => user_message_text(content),
        _ => None,
    })
}

fn user_message_text(content: &[JsonValue]) -> Option<String> {
    let parts = content
        .iter()
        .filter_map(|value| {
            value
                .get("text")
                .and_then(JsonValue::as_str)
                .or_else(|| value.as_str())
                .map(str::to_string)
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn rfc3339_from_epoch_ms(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch is valid"))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn raw_bundle_from_run(
    run: &RunRecord,
    run_record_value: JsonValue,
    include_attachments: bool,
) -> Result<SessionBundle, SessionBundleError> {
    let mut bundle = SessionBundle {
        bundle_id: new_id("bundle"),
        created_at: now_rfc3339(),
        producer: BundleProducer {
            name: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            schema_id: SESSION_BUNDLE_SCHEMA_ID.to_string(),
        },
        source: BundleSource {
            kind: "run_record".to_string(),
            run_record_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            workflow_name: run.workflow_name.clone(),
            task: run.task.clone(),
            status: run.status.clone(),
            started_at: run.started_at.clone(),
            finished_at: run.finished_at.clone(),
            persisted_path: run.persisted_path.clone(),
            root_run_id: run.root_run_id.clone(),
            parent_run_id: run.parent_run_id.clone(),
            child_run_count: run.child_runs.len(),
        },
        runtime: BundleRuntime {
            harn_version: env!("CARGO_PKG_VERSION").to_string(),
            provider_models: run
                .usage
                .as_ref()
                .map(|usage| usage.models.clone())
                .unwrap_or_default(),
            usage: run.usage.as_ref().map(|usage| BundleUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                call_count: usage.call_count,
                total_duration_ms: usage.total_duration_ms,
                total_cost: usage.total_cost,
                models: usage.models.clone(),
            }),
            metadata: BTreeMap::new(),
        },
        workspace: workspace_from_run(run),
        transcript: transcript_from_run(run),
        tools: BundleTools {
            schemas: tool_schema_entries(run),
            calls: run
                .tool_recordings
                .iter()
                .map(BundleToolCall::from)
                .collect(),
        },
        permissions: permissions_from_run(run),
        replay: BundleReplay {
            replay_fixture: run.replay_fixture.clone(),
            run_record: Some(run_record_value),
            observability: run.observability.clone(),
            verification_outcomes: verification_outcomes_for_run(run),
            worker_snapshots: worker_snapshots_from_run(run),
            event_log_pointers: event_log_pointers_from_run(run),
            transitions: run.transitions.clone(),
            checkpoints: run.checkpoints.clone(),
            trace_spans: run.trace_spans.clone(),
            deterministic_events: deterministic_events_from_run(run)?,
        },
        redaction: RedactionManifest {
            mode: "sanitized".to_string(),
            policy: "harn_vm::redact::RedactionPolicy::default+session_bundle_local_paths"
                .to_string(),
            placeholder: REDACTED_PLACEHOLDER.to_string(),
            entries: Vec::new(),
            unsafe_secret_markers_rejected: true,
        },
        attachments: if include_attachments {
            attachments_from_run(run)
        } else {
            Vec::new()
        },
        ..SessionBundle::default()
    };
    bundle.metadata.insert(
        "format_note".to_string(),
        json!("Session bundles are portable JSON envelopes; hosted share links should reference sanitized bundles rather than raw run records."),
    );
    Ok(bundle)
}

fn verification_outcomes_for_run(run: &RunRecord) -> Vec<RunVerificationOutcomeRecord> {
    if let Some(observability) = run.observability.as_ref() {
        return observability.verification_outcomes.clone();
    }
    derive_run_observability(run, run.persisted_path.as_deref().map(Path::new))
        .verification_outcomes
}

fn bundle_redaction_policy(base: &RedactionPolicy) -> RedactionPolicy {
    base.clone()
        .with_extra_field("persisted_path")
        .with_extra_field("primary")
        .with_extra_field("run_path")
        .with_extra_field("snapshot_ref")
        .with_extra_field("snapshot_path")
        .with_extra_field("source_path")
}

fn workspace_from_run(run: &RunRecord) -> Option<BundleWorkspace> {
    let anchor = run
        .transcript
        .as_ref()
        .and_then(|transcript| transcript.get("metadata"))
        .and_then(anchor_from_transcript_metadata_json)?;
    Some(BundleWorkspace::from(&anchor))
}

fn transcript_from_run(run: &RunRecord) -> BundleTranscript {
    let mut sections = Vec::new();
    if let Some(transcript) = &run.transcript {
        sections.push(transcript_section(
            "run",
            "Run transcript",
            "run",
            "$.transcript",
            transcript,
        ));
    }
    for (index, stage) in run.stages.iter().enumerate() {
        if let Some(transcript) = &stage.transcript {
            sections.push(transcript_section(
                &stage.id,
                &format!("Stage {}", stage.node_id),
                "stage",
                &format!("$.stages[{index}].transcript"),
                transcript,
            ));
        }
    }
    BundleTranscript { sections }
}

fn transcript_section(
    id: &str,
    label: &str,
    scope: &str,
    location: &str,
    transcript: &JsonValue,
) -> BundleTranscriptSection {
    BundleTranscriptSection {
        id: id.to_string(),
        label: label.to_string(),
        scope: scope.to_string(),
        location: location.to_string(),
        summary: transcript
            .get("summary")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        messages: json_array(transcript.get("messages")),
        events: json_array(transcript.get("events")),
        assets: json_array(transcript.get("assets")),
        metadata: transcript
            .get("metadata")
            .and_then(JsonValue::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn json_array(value: Option<&JsonValue>) -> Vec<JsonValue> {
    value
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
}

fn tool_schema_entries(run: &RunRecord) -> Vec<BundleJsonEntry> {
    let mut entries = Vec::new();
    collect_tool_schema_entries_from_transcript(&mut entries, "run.transcript", &run.transcript);
    for stage in &run.stages {
        collect_tool_schema_entries_from_transcript(
            &mut entries,
            &format!("stage.{}.transcript", stage.node_id),
            &stage.transcript,
        );
        if let Some(tools) = stage
            .metadata
            .get("tool_schemas")
            .or_else(|| stage.metadata.get("tools"))
        {
            entries.push(BundleJsonEntry {
                source: format!("stage.{}.metadata", stage.node_id),
                index: entries.len(),
                value: tools.clone(),
            });
        }
    }
    entries
}

fn collect_tool_schema_entries_from_transcript(
    entries: &mut Vec<BundleJsonEntry>,
    source: &str,
    transcript: &Option<JsonValue>,
) {
    let Some(transcript) = transcript else {
        return;
    };
    for event in transcript
        .get("events")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let kind = event
            .get("type")
            .or_else(|| event.get("kind"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if kind == "tool_schemas" || kind == "tool_schema" {
            entries.push(BundleJsonEntry {
                source: source.to_string(),
                index: entries.len(),
                value: event.clone(),
            });
        }
    }
}

fn permissions_from_run(run: &RunRecord) -> Vec<BundlePermission> {
    let mut permissions = run
        .hitl_questions
        .iter()
        .map(permission_from_hitl_question)
        .collect::<Vec<_>>();
    collect_permission_events(&mut permissions, "run.transcript", &run.transcript);
    for stage in &run.stages {
        collect_permission_events(
            &mut permissions,
            &format!("stage.{}.transcript", stage.node_id),
            &stage.transcript,
        );
        if let Some(worker) = stage.metadata.get("worker") {
            if let Some(policy) = worker
                .get("audit")
                .and_then(|audit| audit.get("approval_policy"))
            {
                permissions.push(BundlePermission {
                    kind: "approval_policy".to_string(),
                    source: format!("stage.{}.worker.audit", stage.node_id),
                    request_id: None,
                    agent: worker
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                    payload: policy.clone(),
                });
            }
        }
    }
    permissions
}

fn permission_from_hitl_question(question: &RunHitlQuestionRecord) -> BundlePermission {
    BundlePermission {
        kind: "hitl_question".to_string(),
        source: "run.hitl_questions".to_string(),
        request_id: Some(question.request_id.clone()),
        agent: if question.agent.is_empty() {
            None
        } else {
            Some(question.agent.clone())
        },
        payload: serde_json::to_value(question).unwrap_or(JsonValue::Null),
    }
}

fn collect_permission_events(
    permissions: &mut Vec<BundlePermission>,
    source: &str,
    transcript: &Option<JsonValue>,
) {
    let Some(transcript) = transcript else {
        return;
    };
    for event in transcript
        .get("events")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let kind = event
            .get("type")
            .or_else(|| event.get("kind"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if kind.contains("permission") || kind.contains("approval") || kind.starts_with("hitl_") {
            permissions.push(BundlePermission {
                kind: kind.to_string(),
                source: source.to_string(),
                request_id: event
                    .get("request_id")
                    .or_else(|| event.get("id"))
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
                agent: event
                    .get("agent")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
                payload: event.clone(),
            });
        }
    }
}

fn event_log_pointers_from_run(run: &RunRecord) -> Vec<BundleEventLogPointer> {
    let mut pointers = Vec::new();
    if let Some(observability) = &run.observability {
        for pointer in &observability.transcript_pointers {
            pointers.push(BundleEventLogPointer {
                kind: pointer.kind.clone(),
                topic: None,
                path: pointer.path.clone(),
                location: pointer.location.clone(),
                available: pointer.available,
            });
        }
        for worker in &observability.worker_lineage {
            if let Some(session_id) = &worker.session_id {
                pointers.push(BundleEventLogPointer {
                    kind: "agent_events".to_string(),
                    topic: Some(format!("observability.agent_events.{session_id}")),
                    path: worker.snapshot_path.clone(),
                    location: format!("worker.{}.session", worker.worker_id),
                    available: worker.snapshot_path.is_some(),
                });
            }
        }
    }
    pointers
}

fn worker_snapshots_from_run(run: &RunRecord) -> Vec<BundleWorkerSnapshot> {
    let mut snapshots = Vec::new();
    let mut seen_paths = BTreeSet::new();
    for child in &run.child_runs {
        let Some(path) = child.snapshot_path.as_deref() else {
            continue;
        };
        if !seen_paths.insert(path.to_string()) {
            continue;
        }
        if let Some(snapshot) = worker_snapshot_from_path(
            &child.worker_id,
            &child.worker_name,
            &child.status,
            Path::new(path),
        ) {
            snapshots.push(snapshot);
        }
    }
    if let Some(observability) = run.observability.as_ref() {
        for worker in &observability.worker_lineage {
            let Some(path) = worker.snapshot_path.as_deref() else {
                continue;
            };
            if !seen_paths.insert(path.to_string()) {
                continue;
            }
            if let Some(snapshot) = worker_snapshot_from_path(
                &worker.worker_id,
                &worker.worker_name,
                &worker.status,
                Path::new(path),
            ) {
                snapshots.push(snapshot);
            }
        }
    }
    snapshots
}

fn worker_snapshot_from_path(
    worker_id: &str,
    worker_name: &str,
    status: &str,
    path: &Path,
) -> Option<BundleWorkerSnapshot> {
    let content = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<JsonValue>(&content).ok()?;
    Some(BundleWorkerSnapshot {
        worker_id: if worker_id.is_empty() {
            value
                .get("id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            worker_id.to_string()
        },
        worker_name: if worker_name.is_empty() {
            value
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("worker")
                .to_string()
        } else {
            worker_name.to_string()
        },
        status: if status.is_empty() {
            value
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            status.to_string()
        },
        snapshot_ref: value
            .get("suspension")
            .and_then(|value| value.get("snapshot_ref"))
            .and_then(JsonValue::as_str)
            .or_else(|| value.get("snapshot_path").and_then(JsonValue::as_str))
            .unwrap_or_else(|| path.to_str().unwrap_or_default())
            .to_string(),
        source_path: Some(path.to_string_lossy().into_owned()),
        value,
    })
}

fn worker_snapshot_file_name(worker_id: &str, index: usize) -> String {
    let component = sanitize_topic_component(worker_id);
    let component = if component.is_empty() {
        format!("worker_{index}")
    } else {
        component
    };
    format!("{component}.json")
}

fn worker_snapshot_value_for_import(value: &JsonValue, path: &Path) -> JsonValue {
    let mut value = value.clone();
    let path = path.to_string_lossy().into_owned();
    if let JsonValue::Object(map) = &mut value {
        map.insert("snapshot_path".to_string(), JsonValue::String(path.clone()));
        if let Some(JsonValue::Object(suspension)) = map.get_mut("suspension") {
            suspension.insert("snapshot_ref".to_string(), JsonValue::String(path));
        }
    }
    value
}

fn deterministic_events_from_run(
    run: &RunRecord,
) -> Result<Vec<BundleJsonEntry>, SessionBundleError> {
    let mut events = Vec::new();
    for (index, transition) in run.transitions.iter().enumerate() {
        events.push(BundleJsonEntry {
            source: "run.transitions".to_string(),
            index,
            value: serde_json::to_value(transition)
                .map_err(|error| SessionBundleError::Encode(error.to_string()))?,
        });
    }
    for (index, checkpoint) in run.checkpoints.iter().enumerate() {
        events.push(BundleJsonEntry {
            source: "run.checkpoints".to_string(),
            index,
            value: serde_json::to_value(checkpoint)
                .map_err(|error| SessionBundleError::Encode(error.to_string()))?,
        });
    }
    Ok(events)
}

fn attachments_from_run(run: &RunRecord) -> Vec<BundleAttachment> {
    run.artifacts
        .iter()
        .map(|artifact| BundleAttachment {
            id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            title: artifact.title.clone(),
            stage: artifact.stage.clone(),
            text: artifact.text.clone(),
            data: artifact.data.clone(),
            metadata: artifact.metadata.clone(),
        })
        .collect()
}

fn redact_json_with_manifest(
    value: &mut JsonValue,
    path: &str,
    policy: &RedactionPolicy,
    entries: &mut Vec<RedactionEntry>,
) {
    match value {
        JsonValue::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let child_path = json_path_child(path, &key);
                if policy.field_is_sensitive(&key) {
                    map.insert(key, JsonValue::String(REDACTED_PLACEHOLDER.to_string()));
                    entries.push(RedactionEntry {
                        path: child_path,
                        class: "sensitive_field".to_string(),
                        action: "replaced".to_string(),
                        replacement: Some(REDACTED_PLACEHOLDER.to_string()),
                    });
                } else if let Some(child) = map.get_mut(&key) {
                    redact_json_with_manifest(child, &child_path, policy, entries);
                }
            }
        }
        JsonValue::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                redact_json_with_manifest(item, &format!("{path}[{index}]"), policy, entries);
            }
        }
        JsonValue::String(text) => {
            let redacted = policy.redact_string(text);
            if let Cow::Owned(replacement) = redacted {
                // Record the actual replacement string (now a named
                // `<redacted:<pattern>:<len>>` placeholder from the
                // OA-06 catalog) in the manifest so audit consumers
                // can attribute the leak to a specific provider.
                let manifest_replacement = replacement.clone();
                *text = replacement;
                entries.push(RedactionEntry {
                    path: path.to_string(),
                    class: "secret_pattern_or_url".to_string(),
                    action: "replaced".to_string(),
                    replacement: Some(manifest_replacement),
                });
            }
        }
        _ => {}
    }
}

fn redact_bundle_pointer_paths_json(
    value: &mut JsonValue,
    path: &str,
    entries: &mut Vec<RedactionEntry>,
) {
    match value {
        JsonValue::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let child_path = json_path_child(path, &key);
                if key == "path" && bundle_pointer_path_should_redact(&child_path) {
                    if !map.get(&key).is_some_and(JsonValue::is_null) {
                        map.insert(key, JsonValue::String(REDACTED_PLACEHOLDER.to_string()));
                        entries.push(RedactionEntry {
                            path: child_path,
                            class: "local_pointer_path".to_string(),
                            action: "replaced".to_string(),
                            replacement: Some(REDACTED_PLACEHOLDER.to_string()),
                        });
                    }
                } else if let Some(child) = map.get_mut(&key) {
                    redact_bundle_pointer_paths_json(child, &child_path, entries);
                }
            }
        }
        JsonValue::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                redact_bundle_pointer_paths_json(item, &format!("{path}[{index}]"), entries);
            }
        }
        _ => {}
    }
}

fn bundle_pointer_path_should_redact(path: &str) -> bool {
    path.contains(".event_log_pointers[") || path.contains(".transcript_pointers[")
}

fn withhold_replay_only_json(value: &mut JsonValue, path: &str, entries: &mut Vec<RedactionEntry>) {
    match value {
        JsonValue::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let child_path = json_path_child(path, &key);
                if replay_only_field_is_prompt_payload(&key) {
                    if !map.get(&key).is_some_and(JsonValue::is_null) {
                        map.insert(key, JsonValue::String(REPLAY_ONLY_PLACEHOLDER.to_string()));
                        entries.push(RedactionEntry {
                            path: child_path,
                            class: "prompt_or_tool_payload".to_string(),
                            action: "withheld".to_string(),
                            replacement: Some(REPLAY_ONLY_PLACEHOLDER.to_string()),
                        });
                    }
                } else if let Some(child) = map.get_mut(&key) {
                    withhold_replay_only_json(child, &child_path, entries);
                }
            }
        }
        JsonValue::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                withhold_replay_only_json(item, &format!("{path}[{index}]"), entries);
            }
        }
        _ => {}
    }
}

fn replay_only_field_is_prompt_payload(key: &str) -> bool {
    matches!(
        key,
        "args"
            | "arguments"
            | "blocks"
            | "content"
            | "data"
            | "private_reasoning"
            | "prompt"
            | "raw_input"
            | "raw_output"
            | "result"
            | "response_text"
            | "summary"
            | "system"
            | "system_prompt"
            | "task"
            | "text"
            | "thinking"
            | "visible_text"
    )
}

fn set_json_path(value: &mut JsonValue, path: &[&str], replacement: JsonValue) {
    let Some((head, tail)) = path.split_first() else {
        *value = replacement;
        return;
    };
    if tail.is_empty() {
        if let JsonValue::Object(map) = value {
            map.insert((*head).to_string(), replacement);
        }
        return;
    }
    if let JsonValue::Object(map) = value {
        if let Some(child) = map.get_mut(*head) {
            set_json_path(child, tail, replacement);
        }
    }
}

fn require_field(value: &JsonValue, field: &str) -> Result<(), SessionBundleError> {
    if value.get(field).is_some() {
        Ok(())
    } else {
        Err(SessionBundleError::MissingRequired(format!("$.{field}")))
    }
}

fn require_nested_field(value: &JsonValue, path: &[&str]) -> Result<(), SessionBundleError> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| SessionBundleError::MissingRequired(json_path_from_segments(path)))?;
    }
    Ok(())
}

fn json_path_from_segments(path: &[&str]) -> String {
    path.iter().fold("$".to_string(), |parent, segment| {
        json_path_child(&parent, segment)
    })
}

fn reject_unredacted_secret_markers(
    value: &JsonValue,
    path: &str,
    policy: &RedactionPolicy,
) -> Result<(), SessionBundleError> {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                reject_unredacted_secret_markers(child, &json_path_child(path, key), policy)?;
            }
        }
        JsonValue::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_unredacted_secret_markers(item, &format!("{path}[{index}]"), policy)?;
            }
        }
        JsonValue::String(text) => {
            if matches!(policy.redact_string(text), Cow::Owned(_)) {
                return Err(SessionBundleError::UnsafeSecretMarker {
                    path: path.to_string(),
                    excerpt: secret_excerpt(text),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn secret_excerpt(text: &str) -> String {
    let excerpt = text.chars().take(80).collect::<String>();
    if text.chars().count() > 80 {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

fn json_path_child(parent: &str, key: &str) -> String {
    if key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_default()
        )
    }
}
