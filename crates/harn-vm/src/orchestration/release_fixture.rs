//! Release-harness fixture ingest.
//!
//! `release_harn.harn` (in `harn-bump-fleet`) emits a
//! `release_harn.crystallization_input.v1` fixture bundle for every run:
//!
//! ```text
//! crystallization-input/
//!   manifest.json                # release identity + file map
//!   release-run.json             # full release-harness payload
//!   deterministic-events.jsonl   # harness facts/findings/step records
//!   agent-events.jsonl           # model review/changelog/recovery events
//!   tool-observations.jsonl      # shell/read/tool observations
//!   README.md                    # human-readable description
//! ```
//!
//! This module ingests one of those bundles and synthesizes a single
//! crystallization candidate (no repeated-sequence mining required —
//! the trace IS the workflow). Output is a [`CrystallizationArtifacts`]
//! that downstream tooling (`build_crystallization_bundle`,
//! `validate_crystallization_bundle`, `shadow_replay_bundle`) consumes
//! unchanged.
//!
//! See `harn-bump-fleet/release_harn.harn` for the upstream emitter and
//! `docs/src/workflow-crystallization.md` for the steel-thread story.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

use super::crystallize::{
    synthesize_candidate_from_trace, CrystallizationAction, CrystallizationApproval,
    CrystallizationArtifacts, CrystallizationSideEffect, CrystallizationTrace, CrystallizeOptions,
    RecoveryFeedbackSummary, SegmentSummary, WorkflowCandidateParameter,
};
use crate::value::VmError;

/// Schema marker emitted by `release_harn.harn` on every event/manifest
/// row. The trailing `.vN` carries the version. Importers MUST refuse
/// fixtures with any other schema marker — bumping the suffix is how
/// `release_harn.harn` signals a backwards-incompatible fixture shape
/// change.
pub const RELEASE_FIXTURE_SCHEMA: &str = "release_harn.crystallization_input.v1";

const MANIFEST_FILE: &str = "manifest.json";
const DETERMINISTIC_EVENTS_FILE: &str = "deterministic-events.jsonl";
const AGENT_EVENTS_FILE: &str = "agent-events.jsonl";
const TOOL_OBSERVATIONS_FILE: &str = "tool-observations.jsonl";

/// Decoded release-fixture manifest. We keep this struct intentionally
/// small — only the fields the ingester actually depends on — and
/// preserve unknown fields verbatim in [`ReleaseFixtureManifest::raw`]
/// so future schema additions don't lose information when the fixture
/// is round-tripped.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReleaseFixtureManifest {
    pub schema_version: String,
    pub kind: String,
    pub generated_by: String,
    pub generated_at: String,
    pub run_id: String,
    pub source_issue: Option<String>,
    pub consumer_issue: Option<String>,
    pub release: ReleaseFixtureIdentity,
    /// The full untouched manifest JSON. Useful for downstream
    /// consumers and for round-tripping unknown fields without losing
    /// them.
    #[serde(skip_serializing, skip_deserializing)]
    pub raw: Option<JsonValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReleaseFixtureIdentity {
    pub repo: String,
    pub mode: String,
    #[serde(default)]
    pub mock: bool,
    pub current_version: String,
    pub next_version: String,
    pub starting_branch: String,
    pub target_release_branch: String,
    pub base_branch: String,
    pub latest_tag: String,
}

/// One row from one of the JSONL fixtures (deterministic-events,
/// agent-events, tool-observations).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReleaseFixtureEvent {
    pub schema_version: String,
    pub event_type: String,
    pub source: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub data: JsonValue,
}

/// All decoded data from one ingested fixture bundle. Kept distinct
/// from the eventual [`CrystallizationTrace`] so the importer can run
/// secondary analysis (segment summary, recovery summary) over the
/// original event stream.
#[derive(Clone, Debug, Default)]
pub struct ReleaseFixture {
    pub manifest: ReleaseFixtureManifest,
    pub deterministic_events: Vec<ReleaseFixtureEvent>,
    pub agent_events: Vec<ReleaseFixtureEvent>,
    pub tool_observations: Vec<ReleaseFixtureEvent>,
}

/// Read a release-fixture bundle directory from disk. The manifest is
/// required; missing JSONL streams are treated as empty (so a fixture
/// from a release with no agent review still ingests cleanly).
pub fn load_release_fixture(dir: &Path) -> Result<ReleaseFixture, VmError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read release fixture manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest_json: JsonValue = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to parse release fixture manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let mut manifest: ReleaseFixtureManifest = serde_json::from_value(manifest_json.clone())
        .map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode release fixture manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
    manifest.raw = Some(manifest_json);

    if manifest.schema_version != RELEASE_FIXTURE_SCHEMA {
        return Err(VmError::Runtime(format!(
            "release fixture manifest {} has unrecognized schema_version {:?} (expected {})",
            manifest_path.display(),
            manifest.schema_version,
            RELEASE_FIXTURE_SCHEMA
        )));
    }

    let deterministic_events = load_optional_jsonl(&dir.join(DETERMINISTIC_EVENTS_FILE))?;
    let agent_events = load_optional_jsonl(&dir.join(AGENT_EVENTS_FILE))?;
    let tool_observations = load_optional_jsonl(&dir.join(TOOL_OBSERVATIONS_FILE))?;

    Ok(ReleaseFixture {
        manifest,
        deterministic_events,
        agent_events,
        tool_observations,
    })
}

fn load_optional_jsonl(path: &Path) -> Result<Vec<ReleaseFixtureEvent>, VmError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read release fixture stream {}: {error}",
            path.display()
        ))
    })?;
    let mut events = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: ReleaseFixtureEvent = serde_json::from_str(trimmed).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode release fixture event {} line {}: {error}",
                path.display(),
                line_idx + 1
            ))
        })?;
        if event.schema_version != RELEASE_FIXTURE_SCHEMA {
            return Err(VmError::Runtime(format!(
                "release fixture event {} line {} has unrecognized schema {:?} (expected {})",
                path.display(),
                line_idx + 1,
                event.schema_version,
                RELEASE_FIXTURE_SCHEMA
            )));
        }
        events.push(event);
    }
    Ok(events)
}

/// Convert a release fixture into a single [`CrystallizationTrace`].
/// Each release-step / tool-observation / agent-attempt /
/// recovery-advice event becomes one [`CrystallizationAction`], in
/// timestamp order. Side effects are inferred from
/// `release_step_recorded` events; capabilities and approval boundaries
/// are derived from event type + tool kind.
pub fn release_fixture_to_trace(fixture: &ReleaseFixture) -> CrystallizationTrace {
    let release = &fixture.manifest.release;
    let mut metadata: BTreeMap<String, JsonValue> = BTreeMap::new();
    metadata.insert("release.repo".to_string(), json!(release.repo));
    metadata.insert("release.mode".to_string(), json!(release.mode));
    metadata.insert("release.mock".to_string(), json!(release.mock));
    metadata.insert(
        "release.current_version".to_string(),
        json!(release.current_version),
    );
    metadata.insert(
        "release.next_version".to_string(),
        json!(release.next_version),
    );
    metadata.insert(
        "release.starting_branch".to_string(),
        json!(release.starting_branch),
    );
    metadata.insert(
        "release.target_release_branch".to_string(),
        json!(release.target_release_branch),
    );
    metadata.insert(
        "release.base_branch".to_string(),
        json!(release.base_branch),
    );
    metadata.insert("release.latest_tag".to_string(), json!(release.latest_tag));
    metadata.insert("fixture.run_id".to_string(), json!(fixture.manifest.run_id));
    metadata.insert(
        "fixture.generated_by".to_string(),
        json!(fixture.manifest.generated_by),
    );

    let mut actions = Vec::new();
    for event in &fixture.deterministic_events {
        if let Some(action) = deterministic_event_to_action(event, release) {
            actions.push(action);
        }
    }
    for event in &fixture.tool_observations {
        if let Some(action) = tool_observation_to_action(event, release) {
            actions.push(action);
        }
    }
    for event in &fixture.agent_events {
        if let Some(action) = agent_event_to_action(event, release) {
            actions.push(action);
        }
    }

    actions.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    // Re-key after sort so action ids stay deterministic and ordered.
    for (idx, action) in actions.iter_mut().enumerate() {
        if action.id.trim().is_empty() {
            action.id = format!("event_{}", idx + 1);
        }
    }

    let trace_id = if fixture.manifest.run_id.trim().is_empty() {
        format!(
            "release_fixture_{}",
            hash_short(fixture.manifest.generated_at.as_bytes())
        )
    } else {
        format!("release_fixture_{}", fixture.manifest.run_id)
    };

    CrystallizationTrace {
        version: 1,
        id: trace_id,
        source: Some(format!(
            "release_harn.harn run {} ({} -> {})",
            fixture.manifest.run_id, release.current_version, release.next_version
        )),
        source_hash: None,
        workflow_id: Some("release_harn".to_string()),
        started_at: actions.first().and_then(|action| action.timestamp.clone()),
        finished_at: actions.last().and_then(|action| action.timestamp.clone()),
        flow: None,
        actions,
        replay_run: None,
        replay_allowlist: Vec::new(),
        usage: Default::default(),
        metadata,
    }
}

fn deterministic_event_to_action(
    event: &ReleaseFixtureEvent,
    release: &ReleaseFixtureIdentity,
) -> Option<CrystallizationAction> {
    let mut action = base_action_from_event(event);
    let data = &event.data;
    match event.event_type.as_str() {
        "release_analysis" => {
            action.kind = "release_analysis".to_string();
            action.name = format!(
                "analyze_release[{}->{}]",
                data.get("current_version")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(&release.current_version),
                data.get("next_version")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(&release.next_version),
            );
            action.parameters = release_parameters(release);
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.capabilities = vec!["git.read".to_string(), "fs.read".to_string()];
            action.inputs = data.clone();
        }
        "changelog_audit_inputs" => {
            action.kind = "changelog_audit".to_string();
            action.name = "collect_changelog_audit_inputs".to_string();
            action.parameters = release_parameters(release);
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.capabilities = vec!["fs.read".to_string()];
            action.inputs = data.clone();
        }
        "deterministic_finding" => {
            let finding = data
                .get("finding")
                .and_then(JsonValue::as_str)
                .unwrap_or("finding");
            action.kind = "release_finding".to_string();
            action.name = format!("finding[{}]", short_summary(finding, 32));
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.observed_output = Some(json!({"finding": finding}));
        }
        "release_step_recorded" => {
            let step_name = data
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("step")
                .to_string();
            let command = data
                .get("command")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string();
            let success = data
                .get("success")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true);
            let status = data.get("status").cloned().unwrap_or(json!(0));
            let classification = data
                .get("classification")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let recovery_hint = data
                .get("recovery_hint")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            action.kind = if success {
                "release_step".to_string()
            } else {
                "shell_failure".to_string()
            };
            action.name = sanitize_step_name(&step_name);
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.parameters = BTreeMap::from([
                ("step".to_string(), json!(step_name)),
                ("command".to_string(), json!(command)),
            ]);
            action.inputs = json!({"command": command});
            action.observed_output = Some(json!({"status": status, "success": success}));
            if !success {
                action.side_effects.push(CrystallizationSideEffect {
                    kind: "shell_failure".to_string(),
                    target: step_name.clone(),
                    capability: Some("shell.exec".to_string()),
                    mutation: None,
                    metadata: BTreeMap::from([(
                        "classification".to_string(),
                        json!(classification),
                    )]),
                });
            }
            action
                .metadata
                .insert("classification".to_string(), json!(classification));
            action
                .metadata
                .insert("recovery_hint".to_string(), json!(recovery_hint));
            action
                .metadata
                .insert("success".to_string(), json!(success));
        }
        _ => return None,
    }
    Some(action)
}

fn tool_observation_to_action(
    event: &ReleaseFixtureEvent,
    release: &ReleaseFixtureIdentity,
) -> Option<CrystallizationAction> {
    if event.event_type != "tool_observation" {
        return None;
    }
    let mut action = base_action_from_event(event);
    let data = &event.data;
    let tool = data
        .get("tool")
        .and_then(JsonValue::as_str)
        .unwrap_or("shell");
    let command = data
        .get("command")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let path = data.get("path").and_then(JsonValue::as_str);
    let step_name = data.get("step_name").and_then(JsonValue::as_str);
    let success = data
        .get("success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(true);

    action.kind = format!("tool_observation:{tool}");
    action.name = match (tool, step_name) {
        (_, Some(name)) if !name.is_empty() => sanitize_step_name(name),
        ("read_file", Some(_)) | ("read_file", None) => format!(
            "read_file[{}]",
            short_summary(path.unwrap_or("unknown"), 48)
        ),
        ("shell", _) => format!("shell[{}]", short_summary(command, 48)),
        _ => format!("{tool}[{}]", short_summary(command, 48)),
    };
    action.deterministic = Some(event.source != "agent");
    action.fuzzy = Some(false);
    let _ = release; // release identity only enters via metadata above
    action.inputs = data.clone();
    action.observed_output = Some(json!({
        "stdout": data.get("stdout").cloned().unwrap_or(json!("")),
        "stderr": data.get("stderr").cloned().unwrap_or(json!("")),
        "status": data.get("status").cloned().unwrap_or(json!(0)),
        "success": success,
    }));
    if !success {
        action.side_effects.push(CrystallizationSideEffect {
            kind: "tool_failure".to_string(),
            target: step_name.unwrap_or(tool).to_string(),
            capability: Some("shell.exec".to_string()),
            mutation: None,
            metadata: BTreeMap::default(),
        });
    }
    action.metadata.insert("tool".to_string(), json!(tool));
    Some(action)
}

fn agent_event_to_action(
    event: &ReleaseFixtureEvent,
    release: &ReleaseFixtureIdentity,
) -> Option<CrystallizationAction> {
    let mut action = base_action_from_event(event);
    let data = &event.data;
    match event.event_type.as_str() {
        "agent_review_attempt" => {
            let name = data
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("review");
            action.kind = "agent_review_attempt".to_string();
            action.name = format!("agent_review[{}]", sanitize_step_name(name));
            action.fuzzy = Some(true);
            action.deterministic = Some(false);
            action.cost.model_calls = 1;
            action.parameters = release_parameters(release);
            action.inputs = data.clone();
            action.observed_output = data.get("text").cloned();
            action.approval = Some(CrystallizationApproval {
                prompt: Some(
                    "Human reviewer must accept the agent-authored audit before promotion."
                        .to_string(),
                ),
                approver: None,
                required: true,
                boundary: Some("release_audit_review".to_string()),
            });
        }
        "agent_review_artifacts" => {
            action.kind = "agent_review_artifacts".to_string();
            action.name = "persist_agent_review_artifacts".to_string();
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.observed_output = Some(data.clone());
        }
        "agent_recovery_advice" => {
            let step_name = data
                .get("step_name")
                .and_then(JsonValue::as_str)
                .unwrap_or("recovery");
            action.kind = "agent_recovery_advice".to_string();
            action.name = format!("recovery_advice[{}]", sanitize_step_name(step_name));
            action.fuzzy = Some(true);
            action.deterministic = Some(false);
            action.cost.model_calls = 1;
            action.observed_output = data.get("text").cloned();
            action.parameters = BTreeMap::from([("failed_step".to_string(), json!(step_name))]);
            action.approval = Some(CrystallizationApproval {
                prompt: Some(
                    "Human reviewer must validate recovery advice before re-running the failing step."
                        .to_string(),
                ),
                approver: None,
                required: true,
                boundary: Some("recovery_review".to_string()),
            });
            action
                .metadata
                .insert("failed_step".to_string(), json!(step_name));
        }
        "agent_review_skipped" => {
            action.kind = "agent_review_skipped".to_string();
            action.name = "agent_review_skipped".to_string();
            action.deterministic = Some(true);
            action.fuzzy = Some(false);
            action.observed_output = data.get("reason").cloned();
        }
        _ => return None,
    }
    Some(action)
}

fn base_action_from_event(event: &ReleaseFixtureEvent) -> CrystallizationAction {
    CrystallizationAction {
        id: stable_event_id(event),
        timestamp: event.timestamp.clone(),
        ..CrystallizationAction::default()
    }
}

fn stable_event_id(event: &ReleaseFixtureEvent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(event.event_type.as_bytes());
    hasher.update([0]);
    hasher.update(event.source.as_bytes());
    hasher.update([0]);
    if let Some(timestamp) = &event.timestamp {
        hasher.update(timestamp.as_bytes());
    }
    hasher.update([0]);
    hasher.update(event.data.to_string().as_bytes());
    let hex = hex::encode(hasher.finalize());
    format!(
        "evt_{}_{}",
        sanitize_step_name(&event.event_type),
        hex.chars().take(12).collect::<String>()
    )
}

fn release_parameters(release: &ReleaseFixtureIdentity) -> BTreeMap<String, JsonValue> {
    let mut out = BTreeMap::new();
    out.insert("repo".to_string(), json!(release.repo));
    out.insert("base_branch".to_string(), json!(release.base_branch));
    out.insert(
        "current_version".to_string(),
        json!(release.current_version),
    );
    out.insert("next_version".to_string(), json!(release.next_version));
    out.insert(
        "target_release_branch".to_string(),
        json!(release.target_release_branch),
    );
    out
}

fn sanitize_step_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "step".to_string()
    } else {
        trimmed
    }
}

fn short_summary(raw: &str, max_chars: usize) -> String {
    let cleaned = raw.replace(['\n', '\r', '\t'], " ");
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(max_chars - 1).collect();
        format!("{prefix}…")
    }
}

fn hash_short(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize()).chars().take(12).collect()
}

/// Build a [`SegmentSummary`] from an ingested fixture. Splits steps
/// into "safe to automate" (deterministic events with no shell
/// failures) vs. "requires human review" (agent events, shell
/// failures, recovery advice).
pub fn build_segment_summary(fixture: &ReleaseFixture) -> SegmentSummary {
    let mut safe = Vec::new();
    let mut review = Vec::new();
    for event in &fixture.deterministic_events {
        match event.event_type.as_str() {
            "release_analysis" | "changelog_audit_inputs" => safe.push(event.event_type.clone()),
            "release_step_recorded" => {
                let name = event
                    .data
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("release_step")
                    .to_string();
                let success = event
                    .data
                    .get("success")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(true);
                if success {
                    safe.push(format!("step:{name}"));
                } else {
                    review.push(format!(
                        "failed step:{name} (deterministic recovery required before re-run)"
                    ));
                }
            }
            "deterministic_finding" => {
                let finding = event
                    .data
                    .get("finding")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("finding");
                review.push(format!("finding requires reviewer attention: {finding}"));
            }
            _ => {}
        }
    }
    for event in &fixture.agent_events {
        match event.event_type.as_str() {
            "agent_review_attempt" => {
                let name = event
                    .data
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("agent_review");
                review.push(format!(
                    "agent review attempt:{name} (model-authored, advisory only)"
                ));
            }
            "agent_recovery_advice" => {
                let step = event
                    .data
                    .get("step_name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("step");
                review.push(format!(
                    "agent recovery advice for {step} (must be validated before re-run)"
                ));
            }
            _ => {}
        }
    }
    let plain = format!(
        "Safe to automate: {} deterministic event(s) (release analysis, changelog inputs, \
         successful release steps). Requires human/agent review: {} item(s) including \
         agent-authored audits, model recovery advice, and any failed deterministic steps. \
         The crystallized workflow keeps all agent steps behind explicit approval boundaries; \
         hosts should not promote this candidate to fully autonomous execution without first \
         resolving the review-required entries.",
        safe.len(),
        review.len()
    );
    SegmentSummary {
        deterministic_count: safe.len(),
        agentic_count: review.len(),
        safe_to_automate: safe,
        requires_human_review: review,
        plain_language: plain,
    }
}

/// Build a [`RecoveryFeedbackSummary`] from an ingested fixture. Counts
/// failed `release_step_recorded` events (the canonical failure record
/// in `release_harn.harn`) and recovery-advice runs, then emits a
/// plain-language description of how recovery is represented.
///
/// The mirrored `tool_observation` rows for the same release steps are
/// only consulted for failures whose step name does not already appear
/// in the deterministic stream — that way a per-step failure is counted
/// exactly once, but a tool observation that is not paired with a
/// release-step row (e.g. a one-off shell read failure) still counts.
pub fn build_recovery_summary(fixture: &ReleaseFixture) -> RecoveryFeedbackSummary {
    let mut shell_failures = 0usize;
    let mut failed_steps: Vec<String> = Vec::new();
    let mut counted_step_names: BTreeSet<String> = BTreeSet::new();

    for event in fixture
        .deterministic_events
        .iter()
        .chain(fixture.tool_observations.iter())
    {
        let success = event
            .data
            .get("success")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true);
        if success {
            continue;
        }
        let name = event
            .data
            .get("name")
            .or_else(|| event.data.get("step_name"))
            .and_then(JsonValue::as_str);
        if let Some(name) = name {
            if !counted_step_names.insert(name.to_string()) {
                continue; // already counted from the deterministic stream
            }
            if !failed_steps.iter().any(|n| n == name) {
                failed_steps.push(name.to_string());
            }
        }
        shell_failures += 1;
    }
    let recovery_runs = fixture
        .agent_events
        .iter()
        .filter(|event| event.event_type == "agent_recovery_advice")
        .count();
    let fed = recovery_runs > 0 && shell_failures > 0;
    let representation = if fed {
        format!(
            "{} shell/tool failure(s) were observed and {} of them triggered an `agent_loop` \
             recovery advice run. Failure stdout/stderr, classification, and recovery hints from \
             the failing release step were embedded in the model prompt; the model produced \
             advisory text only — no automatic re-run or live mutation. Reviewers must validate \
             the advice before re-running the failing step.",
            shell_failures, recovery_runs
        )
    } else if shell_failures > 0 {
        format!(
            "{} shell/tool failure(s) were observed but no agent recovery loop was invoked. \
             Failure context was recorded for human review only.",
            shell_failures
        )
    } else if recovery_runs > 0 {
        format!(
            "No shell/tool failures were observed in this run, but {} agent recovery loop(s) \
             ran (likely from a previous attempt or a non-shell error path).",
            recovery_runs
        )
    } else {
        "No shell/tool failures were observed and no recovery loops ran. The release \
         executed end-to-end on the deterministic path."
            .to_string()
    };
    RecoveryFeedbackSummary {
        shell_failures_seen: shell_failures,
        recovery_advice_runs: recovery_runs,
        failures_fed_into_agent: fed,
        failed_steps,
        representation,
    }
}

/// Top-level convenience: read a fixture from disk and synthesize a
/// crystallization candidate ready for bundle-build / validate /
/// shadow-replay. This is what the CLI subcommand wires up.
pub fn ingest_release_fixture(
    dir: &Path,
    options: CrystallizeOptions,
) -> Result<
    (
        CrystallizationArtifacts,
        ReleaseFixture,
        CrystallizationTrace,
    ),
    VmError,
> {
    let fixture = load_release_fixture(dir)?;
    let mut trace = release_fixture_to_trace(&fixture);
    if trace.actions.is_empty() {
        return Err(VmError::Runtime(format!(
            "release fixture {} produced zero actions; nothing to crystallize",
            dir.display()
        )));
    }
    // Stamp a content hash on the trace so the manifest's
    // source_trace_hashes line up with downstream eval pack rubric
    // expectations even before normalize_trace re-fills it.
    let payload = serde_json::to_vec(&trace.actions).unwrap_or_default();
    trace.source_hash = Some(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(payload.as_slice()))
    ));

    let segment_summary = build_segment_summary(&fixture);
    let recovery_summary = build_recovery_summary(&fixture);
    let release = &fixture.manifest.release;
    let extra_parameters = release_parameter_definitions(release);

    let artifacts = synthesize_candidate_from_trace(
        trace.clone(),
        options,
        extra_parameters,
        Some(segment_summary),
        Some(recovery_summary),
    )?;
    Ok((artifacts, fixture, trace))
}

fn release_parameter_definitions(
    release: &ReleaseFixtureIdentity,
) -> Vec<WorkflowCandidateParameter> {
    fn parameter(name: &str, value: &str) -> WorkflowCandidateParameter {
        WorkflowCandidateParameter {
            name: name.to_string(),
            source_paths: vec![format!("manifest.release.{name}")],
            examples: if value.trim().is_empty() {
                Vec::new()
            } else {
                vec![value.to_string()]
            },
            required: true,
        }
    }
    vec![
        parameter("repo", &release.repo),
        parameter("base_branch", &release.base_branch),
        parameter("current_version", &release.current_version),
        parameter("next_version", &release.next_version),
        parameter("target_release_branch", &release.target_release_branch),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path) {
        let manifest = json!({
            "schema_version": RELEASE_FIXTURE_SCHEMA,
            "kind": "release_harn_crystallization_input",
            "generated_by": "release_harn.harn",
            "generated_at": "2026-05-02T00:00:00Z",
            "run_id": "test-run",
            "source_issue": "https://github.com/burin-labs/harn-bump-fleet/issues/2",
            "consumer_issue": "https://github.com/burin-labs/harn/issues/1146",
            "release": {
                "repo": "/tmp/harn",
                "mode": "audit",
                "mock": true,
                "current_version": "0.7.52",
                "next_version": "0.7.53",
                "starting_branch": "main",
                "target_release_branch": "release/v0.7.53",
                "base_branch": "main",
                "latest_tag": "v0.7.52"
            }
        });
        fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let det = [
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "release_analysis",
                "source": "deterministic",
                "timestamp": "2026-05-02T00:00:00Z",
                "data": {
                    "current_version": "0.7.52",
                    "next_version": "0.7.53",
                    "base_branch": "main"
                }
            }),
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "changelog_audit_inputs",
                "source": "deterministic",
                "timestamp": "2026-05-02T00:00:01Z",
                "data": {"expected_version": "0.7.53"}
            }),
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "release_step_recorded",
                "source": "deterministic",
                "timestamp": "2026-05-02T00:00:02Z",
                "data": {
                    "name": "ensure-clean-tree",
                    "command": "git status --porcelain",
                    "success": true,
                    "status": 0
                }
            }),
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "release_step_recorded",
                "source": "deterministic",
                "timestamp": "2026-05-02T00:00:03Z",
                "data": {
                    "name": "push",
                    "command": "git push -u origin release/v0.7.53",
                    "success": false,
                    "status": 1,
                    "classification": "branch_protection",
                    "recovery_hint": "rerun with --no-verify after green hook budget"
                }
            }),
        ];
        let det_jsonl: String = det
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join(DETERMINISTIC_EVENTS_FILE), det_jsonl + "\n").unwrap();

        let agent = [
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "agent_review_attempt",
                "source": "agent",
                "timestamp": "2026-05-02T00:00:04Z",
                "data": {
                    "index": 0,
                    "name": "release_audit",
                    "status": "ok",
                    "text": "audit passes",
                    "validation_findings": []
                }
            }),
            json!({
                "schema_version": RELEASE_FIXTURE_SCHEMA,
                "event_type": "agent_recovery_advice",
                "source": "agent",
                "timestamp": "2026-05-02T00:00:05Z",
                "data": {
                    "step_name": "push",
                    "status": "ok",
                    "text": "Re-run with --no-verify"
                }
            }),
        ];
        let agent_jsonl: String = agent
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join(AGENT_EVENTS_FILE), agent_jsonl + "\n").unwrap();

        let tools = [json!({
            "schema_version": RELEASE_FIXTURE_SCHEMA,
            "event_type": "tool_observation",
            "source": "deterministic",
            "timestamp": "2026-05-02T00:00:06Z",
            "data": {
                "tool": "shell",
                "command": "git status --porcelain",
                "success": true,
                "status": 0,
                "stdout": "",
                "stderr": ""
            }
        })];
        let tools_jsonl: String = tools
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join(TOOL_OBSERVATIONS_FILE), tools_jsonl + "\n").unwrap();
    }

    #[test]
    fn ingest_emits_a_safe_candidate_with_segment_and_recovery_summary() {
        let temp = TempDir::new().unwrap();
        write_fixture(temp.path());
        let (artifacts, fixture, _trace) = ingest_release_fixture(
            temp.path(),
            CrystallizeOptions {
                workflow_name: Some("release_harn".to_string()),
                package_name: Some("release-harn".to_string()),
                ..CrystallizeOptions::default()
            },
        )
        .expect("ingest");

        // Manifest decoded.
        assert_eq!(fixture.manifest.release.next_version, "0.7.53");

        // Candidate selected and safe to propose.
        assert!(artifacts.report.selected_candidate_id.is_some());
        let candidate = &artifacts.report.candidates[0];
        assert_eq!(candidate.name, "release_harn");
        assert!(candidate.shadow.pass);

        // Segment summary records both deterministic and agentic items.
        let summary = artifacts
            .report
            .segment_summary
            .as_ref()
            .expect("segment summary");
        assert!(summary.deterministic_count >= 3);
        assert!(summary.agentic_count >= 2);
        assert!(summary
            .requires_human_review
            .iter()
            .any(|item| item.contains("failed step:push")));

        // Recovery summary captures the failed push + advice.
        let recovery = artifacts
            .report
            .recovery_summary
            .as_ref()
            .expect("recovery summary");
        assert!(recovery.shell_failures_seen >= 1);
        assert!(recovery.recovery_advice_runs >= 1);
        assert!(recovery.failures_fed_into_agent);
        assert!(recovery.failed_steps.contains(&"push".to_string()));
        assert!(recovery.representation.contains("agent_loop"));

        // Release identity surfaces as parameters with `next_version` etc.
        let names: Vec<&str> = candidate
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert!(names.contains(&"next_version"));
        assert!(names.contains(&"current_version"));
        assert!(names.contains(&"base_branch"));

        // Generated workflow code references the candidate name.
        assert!(artifacts.harn_code.contains("pipeline release_harn"));
    }

    #[test]
    fn rejects_unknown_schema() {
        let temp = TempDir::new().unwrap();
        let manifest = json!({
            "schema_version": "release_harn.crystallization_input.v999",
            "kind": "release_harn_crystallization_input",
            "generated_by": "release_harn.harn",
            "generated_at": "2026-05-02T00:00:00Z",
            "run_id": "test-run",
            "release": {
                "repo": "/tmp/harn",
                "mode": "audit",
                "mock": true,
                "current_version": "0.7.52",
                "next_version": "0.7.53",
                "starting_branch": "main",
                "target_release_branch": "release/v0.7.53",
                "base_branch": "main",
                "latest_tag": "v0.7.52"
            }
        });
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let err = load_release_fixture(temp.path()).unwrap_err();
        assert!(format!("{err}").contains("unrecognized schema"));
    }

    #[test]
    fn missing_jsonl_streams_default_to_empty() {
        let temp = TempDir::new().unwrap();
        let manifest = json!({
            "schema_version": RELEASE_FIXTURE_SCHEMA,
            "kind": "release_harn_crystallization_input",
            "generated_by": "release_harn.harn",
            "generated_at": "2026-05-02T00:00:00Z",
            "run_id": "no-events-run",
            "release": {
                "repo": "/tmp/harn",
                "mode": "audit",
                "mock": true,
                "current_version": "0.7.52",
                "next_version": "0.7.53",
                "starting_branch": "main",
                "target_release_branch": "release/v0.7.53",
                "base_branch": "main",
                "latest_tag": "v0.7.52"
            }
        });
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let fixture = load_release_fixture(temp.path()).expect("load");
        assert!(fixture.deterministic_events.is_empty());
        assert!(fixture.agent_events.is_empty());
        assert!(fixture.tool_observations.is_empty());

        // Without any events, ingest fails closed (no actions to crystallize).
        let err = ingest_release_fixture(temp.path(), CrystallizeOptions::default()).unwrap_err();
        assert!(format!("{err}").contains("zero actions"));
    }
}
