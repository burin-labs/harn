//! Run-record I/O, child-run materialization, sidecar extraction, and trigger/trace helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::{default_run_dir, new_id, now_rfc3339, parse_json_payload, sync_run_handoffs};
use super::action_graph::{publish_action_graph_event, refresh_run_observability};
use super::eval_pack::replay_fixture_from_run;
use super::json::json_usize;
use super::types::{
    run_child_record_from_worker_metadata, CompactionEventRecord, DaemonEventRecord,
    RunChildRecord, RunHitlQuestionRecord, RunRecord, RunStageRecord,
};
use crate::agent_events::AgentEvent;
use crate::event_log::{
    active_event_log, sanitize_topic_component, AnyEventLog, EventId, EventLog,
    LogEvent as EventLogRecord, Topic,
};
use crate::llm::vm_value_to_json;
use crate::triggers::{SignatureStatus, TriggerEvent};
use crate::value::{VmError, VmValue};

pub(super) fn run_child_from_stage_metadata(stage: &RunStageRecord) -> Option<RunChildRecord> {
    let parent_stage_id = if stage.id.is_empty() {
        None
    } else {
        Some(stage.id.clone())
    };
    run_child_record_from_worker_metadata(parent_stage_id, stage.metadata.get("worker")?)
}

pub(super) fn fill_missing_child_run_fields(existing: &mut RunChildRecord, child: RunChildRecord) {
    if existing.worker_name.is_empty() {
        existing.worker_name = child.worker_name;
    }
    if existing.parent_stage_id.is_none() {
        existing.parent_stage_id = child.parent_stage_id;
    }
    if existing.session_id.is_none() {
        existing.session_id = child.session_id;
    }
    if existing.parent_session_id.is_none() {
        existing.parent_session_id = child.parent_session_id;
    }
    if existing.mutation_scope.is_none() {
        existing.mutation_scope = child.mutation_scope;
    }
    if existing.approval_policy.is_none() {
        existing.approval_policy = child.approval_policy;
    }
    if existing.task.is_empty() {
        existing.task = child.task;
    }
    if existing.request.is_none() {
        existing.request = child.request;
    }
    if existing.provenance.is_none() {
        existing.provenance = child.provenance;
    }
    if existing.status.is_empty() {
        existing.status = child.status;
    }
    if existing.started_at.is_empty() {
        existing.started_at = child.started_at;
    }
    if existing.finished_at.is_none() {
        existing.finished_at = child.finished_at;
    }
    if existing.run_id.is_none() {
        existing.run_id = child.run_id;
    }
    if existing.run_path.is_none() {
        existing.run_path = child.run_path;
    }
    if existing.snapshot_path.is_none() {
        existing.snapshot_path = child.snapshot_path;
    }
    if existing.execution.is_none() {
        existing.execution = child.execution;
    }
}

pub(super) fn materialize_child_runs_from_stage_metadata(run: &mut RunRecord) {
    for child in run
        .stages
        .iter()
        .filter_map(run_child_from_stage_metadata)
        .collect::<Vec<_>>()
    {
        match run
            .child_runs
            .iter_mut()
            .find(|existing| existing.worker_id == child.worker_id)
        {
            Some(existing) => fill_missing_child_run_fields(existing, child),
            None => run.child_runs.push(child),
        }
    }
}

pub(super) fn read_topic_records(
    log: &AnyEventLog,
    topic: &Topic,
) -> Vec<(crate::event_log::EventId, EventLogRecord)> {
    let mut from = None;
    let mut records = Vec::new();
    loop {
        let batch =
            futures::executor::block_on(log.read_range(topic, from, 256)).unwrap_or_default();
        if batch.is_empty() {
            break;
        }
        from = batch.last().map(|(event_id, _)| *event_id);
        records.extend(batch);
    }
    records
}

#[derive(Clone, Debug)]
pub struct AgentSessionReplayEvent {
    pub event_id: EventId,
    pub event: AgentEvent,
}

pub async fn load_agent_session_replay_events(
    session_id: &str,
) -> Result<Vec<AgentSessionReplayEvent>, VmError> {
    let Some(log) = active_event_log() else {
        return Ok(Vec::new());
    };
    let topic = Topic::new(format!(
        "observability.agent_events.{}",
        sanitize_topic_component(session_id)
    ))
    .map_err(|error| VmError::Runtime(format!("failed to build agent event topic: {error}")))?;

    let mut events = Vec::new();
    let mut from = None;
    loop {
        let batch = log.read_range(&topic, from, 1024).await.map_err(|error| {
            VmError::Runtime(format!(
                "failed to read agent event replay topic {}: {error}",
                topic.as_str()
            ))
        })?;
        let batch_len = batch.len();
        for (event_id, record) in batch {
            from = Some(event_id);
            if record.headers.get("session_id").map(String::as_str) != Some(session_id) {
                continue;
            }
            let Some(event_value) = record.payload.get("event").cloned() else {
                continue;
            };
            let event = serde_json::from_value::<AgentEvent>(event_value).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to decode agent event replay record {event_id}: {error}"
                ))
            })?;
            if event.session_id() == session_id {
                events.push(AgentSessionReplayEvent { event_id, event });
            }
        }
        if batch_len < 1024 {
            break;
        }
    }
    Ok(events)
}

pub(super) fn merge_hitl_questions_from_active_log(run: &mut RunRecord) {
    let Some(log) = active_event_log() else {
        return;
    };
    let topic = Topic::new(crate::HITL_QUESTIONS_TOPIC)
        .expect("static hitl.questions topic should always be valid");
    let mut merged = run
        .hitl_questions
        .iter()
        .cloned()
        .map(|question| (question.request_id.clone(), question))
        .collect::<BTreeMap<_, _>>();

    for (_, event) in read_topic_records(log.as_ref(), &topic) {
        if event.kind != "hitl.question_asked" {
            continue;
        }
        let payload = &event.payload;
        let matches_run = event
            .headers
            .get("run_id")
            .is_some_and(|value| value == &run.id)
            || payload
                .get("run_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == run.id);
        if !matches_run {
            continue;
        }
        let request_id = payload
            .get("request_id")
            .and_then(|value| value.as_str())
            .or_else(|| event.headers.get("request_id").map(String::as_str))
            .unwrap_or_default();
        let prompt = payload
            .get("payload")
            .and_then(|value| value.get("prompt"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if request_id.is_empty() || prompt.is_empty() {
            continue;
        }
        merged.insert(
            request_id.to_string(),
            RunHitlQuestionRecord {
                request_id: request_id.to_string(),
                prompt: prompt.to_string(),
                agent: payload
                    .get("agent")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                trace_id: payload
                    .get("trace_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                asked_at: payload
                    .get("requested_at")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
            },
        );
    }

    run.hitl_questions = merged.into_values().collect();
    run.hitl_questions.sort_by(|left, right| {
        (left.asked_at.as_str(), left.request_id.as_str())
            .cmp(&(right.asked_at.as_str(), right.request_id.as_str()))
    });
}

pub(super) fn signature_status_label(status: &SignatureStatus) -> &'static str {
    match status {
        SignatureStatus::Verified => "verified",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Failed { .. } => "failed",
    }
}

pub(super) fn trigger_event_from_run(run: &RunRecord) -> Option<TriggerEvent> {
    run.metadata
        .get("trigger_event")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(super) fn run_trace_id(
    run: &RunRecord,
    trigger_event: Option<&TriggerEvent>,
) -> Option<String> {
    trigger_event
        .map(|event| event.trace_id.0.clone())
        .or_else(|| {
            run.metadata
                .get("trace_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
}

pub(super) fn replay_of_event_id_from_run(run: &RunRecord) -> Option<String> {
    run.metadata
        .get("replay_of_event_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(super) fn llm_transcript_sidecar_path(run_path: &Path) -> Option<PathBuf> {
    let stem = run_path.file_stem()?.to_str()?;
    let parent = run_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}-llm/llm_transcript.jsonl")))
}

pub(super) fn compaction_events_from_transcript(
    transcript: &serde_json::Value,
    stage_id: Option<&str>,
    node_id: Option<&str>,
    location_prefix: &str,
    persisted_path: Option<&Path>,
) -> Vec<CompactionEventRecord> {
    use std::collections::BTreeSet;
    let transcript_id = transcript
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let asset_ids = transcript
        .get("assets")
        .and_then(|value| value.as_array())
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| {
                    asset
                        .get("id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    transcript
        .get("events")
        .and_then(|value| value.as_array())
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    event.get("kind").and_then(|value| value.as_str()) == Some("compaction")
                })
                .map(|event| {
                    let metadata = event.get("metadata");
                    let snapshot_asset_id = metadata
                        .and_then(|value| value.get("snapshot_asset_id"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let available = snapshot_asset_id
                        .as_ref()
                        .is_some_and(|asset_id| asset_ids.contains(asset_id));
                    let snapshot_location = snapshot_asset_id
                        .as_ref()
                        .map(|asset_id| format!("{location_prefix}.assets[{asset_id}]"))
                        .unwrap_or_else(|| location_prefix.to_string());
                    CompactionEventRecord {
                        id: event
                            .get("id")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        transcript_id: transcript_id.clone(),
                        stage_id: stage_id.map(str::to_string),
                        node_id: node_id.map(str::to_string),
                        mode: metadata
                            .and_then(|value| value.get("mode"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        strategy: metadata
                            .and_then(|value| value.get("strategy"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        archived_messages: json_usize(
                            metadata.and_then(|value| value.get("archived_messages")),
                        ),
                        estimated_tokens_before: json_usize(
                            metadata.and_then(|value| value.get("estimated_tokens_before")),
                        ),
                        estimated_tokens_after: json_usize(
                            metadata.and_then(|value| value.get("estimated_tokens_after")),
                        ),
                        snapshot_asset_id,
                        snapshot_location,
                        snapshot_path: persisted_path
                            .map(|path| path.to_string_lossy().into_owned()),
                        available,
                        instruction_mode: metadata
                            .and_then(|value| value.get("instruction_mode"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        instruction_source: metadata
                            .and_then(|value| value.get("instruction_source"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        compaction_policy: metadata
                            .and_then(|value| value.get("compaction_policy"))
                            .cloned(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn daemon_events_from_sidecar(run_path: &Path) -> Vec<DaemonEventRecord> {
    let Some(sidecar_path) = llm_transcript_sidecar_path(run_path) else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(sidecar_path) else {
        return Vec::new();
    };

    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event.get("type").and_then(|value| value.as_str()) == Some("daemon_event"))
        .filter_map(|event| serde_json::from_value::<DaemonEventRecord>(event).ok())
        .collect()
}

pub fn normalize_run_record(value: &VmValue) -> Result<RunRecord, VmError> {
    let mut run: RunRecord = parse_json_payload(vm_value_to_json(value), "run_record")?;
    if run.type_name.is_empty() {
        run.type_name = "run_record".to_string();
    }
    if run.id.is_empty() {
        run.id = new_id("run");
    }
    if run.started_at.is_empty() {
        run.started_at = now_rfc3339();
    }
    if run.status.is_empty() {
        run.status = "running".to_string();
    }
    if run.root_run_id.is_none() {
        run.root_run_id = Some(run.id.clone());
    }
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    merge_hitl_questions_from_active_log(&mut run);
    materialize_child_runs_from_stage_metadata(&mut run);
    sync_run_handoffs(&mut run);
    if run.observability.is_none() {
        let persisted_path = run.persisted_path.clone();
        let persisted = persisted_path.as_deref().map(Path::new);
        refresh_run_observability(&mut run, persisted);
    }
    Ok(run)
}

pub fn save_run_record(run: &RunRecord, path: Option<&str>) -> Result<String, VmError> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_run_dir().join(format!("{}.json", run.id)));
    let mut materialized = run.clone();
    merge_hitl_questions_from_active_log(&mut materialized);
    materialize_child_runs_from_stage_metadata(&mut materialized);
    if materialized.replay_fixture.is_none() {
        materialized.replay_fixture = Some(replay_fixture_from_run(&materialized));
    }
    materialized.persisted_path = Some(path.to_string_lossy().into_owned());
    sync_run_handoffs(&mut materialized);
    refresh_run_observability(&mut materialized, Some(&path));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("failed to create run directory: {e}")))?;
    }
    let json = serde_json::to_string_pretty(&materialized)
        .map_err(|e| VmError::Runtime(format!("failed to encode run record: {e}")))?;
    crate::atomic_io::atomic_write(&path, json.as_bytes())
        .map_err(|e| VmError::Runtime(format!("failed to persist run record: {e}")))?;
    if let Some(observability) = materialized.observability.as_ref() {
        publish_action_graph_event(&materialized, observability, &path);
    }
    Ok(path.to_string_lossy().into_owned())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    let mut run: RunRecord = serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))?;
    materialize_child_runs_from_stage_metadata(&mut run);
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    run.persisted_path
        .get_or_insert_with(|| path.to_string_lossy().into_owned());
    sync_run_handoffs(&mut run);
    refresh_run_observability(&mut run, Some(path));
    Ok(run)
}
