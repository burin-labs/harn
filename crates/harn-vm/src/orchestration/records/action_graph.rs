//! Action-graph node/edge builders and the `derive_run_observability` aggregator.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::json::{
    compact_json_value, json_bool, json_string_array, json_usize, stage_result_payload,
    task_ledger_summary_from_value,
};
use super::persistence::{
    compaction_events_from_transcript, daemon_events_from_sidecar, llm_transcript_sidecar_path,
    replay_of_event_id_from_run, run_trace_id, signature_status_label, trigger_event_from_run,
};
use super::types::{
    CompactionEventRecord, DaemonEventRecord, RunActionGraphEdgeRecord, RunActionGraphNodeRecord,
    RunObservabilityRecord, RunPlannerRoundRecord, RunRecord, RunStageRecord,
    RunTranscriptPointerRecord, RunVerificationOutcomeRecord, RunWorkerLineageRecord,
    ACTION_GRAPH_EDGE_KIND_DELEGATES, ACTION_GRAPH_EDGE_KIND_ENTRY,
    ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE, ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN,
    ACTION_GRAPH_EDGE_KIND_TRANSITION, ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH,
    ACTION_GRAPH_NODE_KIND_PREDICATE, ACTION_GRAPH_NODE_KIND_RUN, ACTION_GRAPH_NODE_KIND_STAGE,
    ACTION_GRAPH_NODE_KIND_TRIGGER, ACTION_GRAPH_NODE_KIND_WORKER,
};
use crate::event_log::{active_event_log, EventLog, LogEvent as EventLogRecord, Topic};
use crate::triggers::TriggerEvent;

pub(super) fn action_graph_kind_for_stage(stage: &RunStageRecord) -> &'static str {
    if stage.kind == "condition" {
        ACTION_GRAPH_NODE_KIND_PREDICATE
    } else {
        ACTION_GRAPH_NODE_KIND_STAGE
    }
}

pub(super) fn trigger_node_metadata(
    trigger_event: &TriggerEvent,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "provider".to_string(),
        serde_json::json!(trigger_event.provider.as_str()),
    );
    metadata.insert(
        "event_kind".to_string(),
        serde_json::json!(trigger_event.kind),
    );
    metadata.insert(
        "dedupe_key".to_string(),
        serde_json::json!(trigger_event.dedupe_key),
    );
    metadata.insert(
        "signature_status".to_string(),
        serde_json::json!(signature_status_label(&trigger_event.signature_status)),
    );
    metadata
}

pub(super) fn stage_node_metadata(stage: &RunStageRecord) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert("stage_kind".to_string(), serde_json::json!(stage.kind));
    if let Some(branch) = stage.branch.as_ref() {
        metadata.insert("branch".to_string(), serde_json::json!(branch));
    }
    if let Some(worker_id) = stage
        .metadata
        .get("worker_id")
        .and_then(|value| value.as_str())
    {
        metadata.insert("worker_id".to_string(), serde_json::json!(worker_id));
    }
    metadata
}

pub(super) fn append_action_graph_node(
    nodes: &mut Vec<RunActionGraphNodeRecord>,
    record: RunActionGraphNodeRecord,
) {
    nodes.push(record);
}

pub async fn append_action_graph_update(
    headers: BTreeMap<String, String>,
    payload: serde_json::Value,
) -> Result<(), crate::event_log::LogError> {
    let Some(log) = active_event_log() else {
        return Ok(());
    };
    let topic = Topic::new("observability.action_graph")
        .expect("static observability.action_graph topic should always be valid");
    let record = EventLogRecord::new("action_graph_update", payload).with_headers(headers);
    log.append(&topic, record).await.map(|_| ())
}

pub(super) fn publish_action_graph_event(
    run: &RunRecord,
    observability: &RunObservabilityRecord,
    path: &Path,
) {
    let trigger_event = trigger_event_from_run(run);
    let mut headers = BTreeMap::new();
    headers.insert("run_id".to_string(), run.id.clone());
    headers.insert("workflow_id".to_string(), run.workflow_id.clone());
    if let Some(trace_id) = run_trace_id(run, trigger_event.as_ref()) {
        headers.insert("trace_id".to_string(), trace_id);
    }
    let payload = serde_json::json!({
        "run_id": run.id,
        "workflow_id": run.workflow_id,
        "persisted_path": path.to_string_lossy(),
        "status": run.status,
        "observability": observability,
    });
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = append_action_graph_update(headers, payload).await;
        });
    } else {
        let _ = futures::executor::block_on(append_action_graph_update(headers, payload));
    }
}

pub fn derive_run_observability(
    run: &RunRecord,
    persisted_path: Option<&Path>,
) -> RunObservabilityRecord {
    let mut action_graph_nodes = Vec::new();
    let mut action_graph_edges = Vec::new();
    let mut verification_outcomes = Vec::new();
    let mut planner_rounds = Vec::new();
    let mut transcript_pointers = Vec::new();
    let mut compaction_events: Vec<CompactionEventRecord> = Vec::new();
    let mut daemon_events: Vec<DaemonEventRecord> = Vec::new();
    let mut research_fact_count = 0usize;

    let root_node_id = format!("run:{}", run.id);
    let trigger_event = trigger_event_from_run(run);
    let propagated_trace_id = run_trace_id(run, trigger_event.as_ref());
    append_action_graph_node(
        &mut action_graph_nodes,
        RunActionGraphNodeRecord {
            id: root_node_id.clone(),
            label: run
                .workflow_name
                .clone()
                .unwrap_or_else(|| run.workflow_id.clone()),
            kind: ACTION_GRAPH_NODE_KIND_RUN.to_string(),
            status: run.status.clone(),
            outcome: run.status.clone(),
            trace_id: propagated_trace_id.clone(),
            stage_id: None,
            node_id: None,
            worker_id: None,
            run_id: Some(run.id.clone()),
            run_path: run.persisted_path.clone(),
            metadata: BTreeMap::from([(
                "workflow_id".to_string(),
                serde_json::json!(run.workflow_id),
            )]),
        },
    );
    let mut entry_node_id = root_node_id.clone();
    if let Some(trigger_event) = trigger_event.as_ref() {
        if let Some(replay_of_event_id) = replay_of_event_id_from_run(run) {
            let replay_source_node_id = format!("trigger:{replay_of_event_id}");
            append_action_graph_node(
                &mut action_graph_nodes,
                RunActionGraphNodeRecord {
                    id: replay_source_node_id.clone(),
                    label: format!(
                        "{}:{} (original {})",
                        trigger_event.provider.as_str(),
                        trigger_event.kind,
                        replay_of_event_id
                    ),
                    kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
                    status: "historical".to_string(),
                    outcome: "replayed_from".to_string(),
                    trace_id: Some(trigger_event.trace_id.0.clone()),
                    stage_id: None,
                    node_id: None,
                    worker_id: None,
                    run_id: Some(run.id.clone()),
                    run_path: run.persisted_path.clone(),
                    metadata: trigger_node_metadata(trigger_event),
                },
            );
            action_graph_edges.push(RunActionGraphEdgeRecord {
                from_id: replay_source_node_id,
                to_id: format!("trigger:{}", trigger_event.id.0),
                kind: ACTION_GRAPH_EDGE_KIND_REPLAY_CHAIN.to_string(),
                label: Some("replay chain".to_string()),
            });
        }
        let trigger_node_id = format!("trigger:{}", trigger_event.id.0);
        append_action_graph_node(
            &mut action_graph_nodes,
            RunActionGraphNodeRecord {
                id: trigger_node_id.clone(),
                label: format!("{}:{}", trigger_event.provider.as_str(), trigger_event.kind),
                kind: ACTION_GRAPH_NODE_KIND_TRIGGER.to_string(),
                status: "received".to_string(),
                outcome: signature_status_label(&trigger_event.signature_status).to_string(),
                trace_id: Some(trigger_event.trace_id.0.clone()),
                stage_id: None,
                node_id: None,
                worker_id: None,
                run_id: Some(run.id.clone()),
                run_path: run.persisted_path.clone(),
                metadata: trigger_node_metadata(trigger_event),
            },
        );
        action_graph_edges.push(RunActionGraphEdgeRecord {
            from_id: root_node_id.clone(),
            to_id: trigger_node_id.clone(),
            kind: ACTION_GRAPH_EDGE_KIND_ENTRY.to_string(),
            label: Some(trigger_event.id.0.clone()),
        });
        entry_node_id = trigger_node_id;
    }

    let stage_node_ids = run
        .stages
        .iter()
        .map(|stage| (stage.id.clone(), format!("stage:{}", stage.id)))
        .collect::<BTreeMap<_, _>>();
    let stage_by_id = run
        .stages
        .iter()
        .map(|stage| (stage.id.as_str(), stage))
        .collect::<BTreeMap<_, _>>();
    let stage_by_node_id = run
        .stages
        .iter()
        .map(|stage| (stage.node_id.clone(), format!("stage:{}", stage.id)))
        .collect::<BTreeMap<_, _>>();

    let incoming_nodes = run
        .transitions
        .iter()
        .map(|transition| transition.to_node_id.clone())
        .collect::<BTreeSet<_>>();

    for stage in &run.stages {
        let graph_node_id = stage_node_ids
            .get(&stage.id)
            .cloned()
            .unwrap_or_else(|| format!("stage:{}", stage.id));
        append_action_graph_node(
            &mut action_graph_nodes,
            RunActionGraphNodeRecord {
                id: graph_node_id.clone(),
                label: stage.node_id.clone(),
                kind: action_graph_kind_for_stage(stage).to_string(),
                status: stage.status.clone(),
                outcome: stage.outcome.clone(),
                trace_id: propagated_trace_id.clone(),
                stage_id: Some(stage.id.clone()),
                node_id: Some(stage.node_id.clone()),
                worker_id: stage
                    .metadata
                    .get("worker_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                run_id: None,
                run_path: None,
                metadata: stage_node_metadata(stage),
            },
        );
        if !incoming_nodes.contains(&stage.node_id) {
            action_graph_edges.push(RunActionGraphEdgeRecord {
                from_id: entry_node_id.clone(),
                to_id: graph_node_id.clone(),
                kind: if trigger_event.is_some() {
                    ACTION_GRAPH_EDGE_KIND_TRIGGER_DISPATCH.to_string()
                } else {
                    ACTION_GRAPH_EDGE_KIND_ENTRY.to_string()
                },
                label: None,
            });
        }

        if stage.kind == "verify" || stage.verification.is_some() {
            let passed = json_bool(
                stage
                    .verification
                    .as_ref()
                    .and_then(|value| value.get("pass")),
            )
            .or_else(|| {
                json_bool(
                    stage
                        .verification
                        .as_ref()
                        .and_then(|value| value.get("success")),
                )
            })
            .or_else(|| {
                if stage.status == "completed" && stage.outcome == "success" {
                    Some(true)
                } else if stage.status == "failed" || stage.outcome == "failed" {
                    Some(false)
                } else {
                    None
                }
            });
            verification_outcomes.push(RunVerificationOutcomeRecord {
                stage_id: stage.id.clone(),
                node_id: stage.node_id.clone(),
                status: stage.status.clone(),
                passed,
                summary: stage
                    .verification
                    .as_ref()
                    .map(compact_json_value)
                    .or_else(|| {
                        stage
                            .visible_text
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .cloned()
                    }),
            });
        }

        if stage.transcript.is_some() {
            transcript_pointers.push(RunTranscriptPointerRecord {
                id: format!("stage:{}:transcript", stage.id),
                label: format!("Stage {} transcript", stage.node_id),
                kind: "embedded_transcript".to_string(),
                location: format!("run.stages[{}].transcript", stage.node_id),
                path: run.persisted_path.clone(),
                available: true,
            });
            if let Some(transcript) = stage.transcript.as_ref() {
                compaction_events.extend(compaction_events_from_transcript(
                    transcript,
                    Some(&stage.id),
                    Some(&stage.node_id),
                    &format!("run.stages[{}].transcript", stage.node_id),
                    persisted_path,
                ));
            }
        }

        if let Some(payload) = stage_result_payload(stage) {
            let trace = payload.get("trace");
            let task_ledger = payload
                .get("task_ledger")
                .and_then(task_ledger_summary_from_value);
            let research_facts = task_ledger
                .as_ref()
                .map(|ledger| ledger.observations.clone())
                .unwrap_or_default();
            research_fact_count += research_facts.len();
            let tools_payload = payload.get("tools");
            let tools_used = json_string_array(
                tools_payload
                    .and_then(|tools| tools.get("calls"))
                    .or_else(|| trace.and_then(|trace| trace.get("tools_used"))),
            );
            let successful_tools =
                json_string_array(tools_payload.and_then(|tools| tools.get("successful")));
            let planner_round = RunPlannerRoundRecord {
                stage_id: stage.id.clone(),
                node_id: stage.node_id.clone(),
                stage_kind: stage.kind.clone(),
                status: stage.status.clone(),
                outcome: stage.outcome.clone(),
                iteration_count: json_usize(trace.and_then(|trace| trace.get("iterations"))),
                llm_call_count: json_usize(trace.and_then(|trace| trace.get("llm_calls"))),
                tool_execution_count: json_usize(
                    trace.and_then(|trace| trace.get("tool_executions")),
                ),
                tool_rejection_count: json_usize(
                    trace.and_then(|trace| trace.get("tool_rejections")),
                ),
                intervention_count: json_usize(trace.and_then(|trace| trace.get("interventions"))),
                compaction_count: json_usize(trace.and_then(|trace| trace.get("compactions"))),
                native_text_tool_fallback_count: json_usize(
                    trace.and_then(|trace| trace.get("native_text_tool_fallbacks")),
                ),
                native_text_tool_fallback_rejection_count: json_usize(
                    trace.and_then(|trace| trace.get("native_text_tool_fallback_rejections")),
                ),
                empty_completion_retry_count: json_usize(
                    trace.and_then(|trace| trace.get("empty_completion_retries")),
                ),
                tools_used,
                successful_tools,
                ledger_done_rejections: json_usize(payload.get("ledger_done_rejections")),
                task_ledger,
                research_facts,
            };
            let has_agentic_detail = planner_round.iteration_count > 0
                || planner_round.llm_call_count > 0
                || planner_round.tool_execution_count > 0
                || planner_round.native_text_tool_fallback_count > 0
                || planner_round.native_text_tool_fallback_rejection_count > 0
                || planner_round.empty_completion_retry_count > 0
                || planner_round.ledger_done_rejections > 0
                || planner_round.task_ledger.is_some()
                || !planner_round.tools_used.is_empty()
                || !planner_round.successful_tools.is_empty();
            if has_agentic_detail {
                planner_rounds.push(planner_round);
            }
        }
    }

    for transition in &run.transitions {
        let Some(to_id) = stage_by_node_id.get(&transition.to_node_id).cloned() else {
            continue;
        };
        let from_stage = transition
            .from_stage_id
            .as_deref()
            .and_then(|stage_id| stage_by_id.get(stage_id).copied());
        let from_id = transition
            .from_stage_id
            .as_ref()
            .and_then(|stage_id| stage_node_ids.get(stage_id))
            .cloned()
            .or_else(|| {
                transition
                    .from_node_id
                    .as_ref()
                    .and_then(|node_id| stage_by_node_id.get(node_id))
                    .cloned()
            })
            .unwrap_or_else(|| root_node_id.clone());
        action_graph_edges.push(RunActionGraphEdgeRecord {
            from_id,
            to_id,
            kind: if from_stage.is_some_and(|stage| stage.kind == "condition") {
                ACTION_GRAPH_EDGE_KIND_PREDICATE_GATE.to_string()
            } else {
                ACTION_GRAPH_EDGE_KIND_TRANSITION.to_string()
            },
            label: transition.branch.clone(),
        });
    }

    let worker_lineage = run
        .child_runs
        .iter()
        .map(|child| {
            let worker_node_id = format!("worker:{}", child.worker_id);
            append_action_graph_node(
                &mut action_graph_nodes,
                RunActionGraphNodeRecord {
                    id: worker_node_id.clone(),
                    label: child.worker_name.clone(),
                    kind: ACTION_GRAPH_NODE_KIND_WORKER.to_string(),
                    status: child.status.clone(),
                    outcome: child.status.clone(),
                    trace_id: propagated_trace_id.clone(),
                    stage_id: child.parent_stage_id.clone(),
                    node_id: None,
                    worker_id: Some(child.worker_id.clone()),
                    run_id: child.run_id.clone(),
                    run_path: child.run_path.clone(),
                    metadata: BTreeMap::from([
                        (
                            "worker_name".to_string(),
                            serde_json::json!(child.worker_name),
                        ),
                        ("task".to_string(), serde_json::json!(child.task)),
                    ]),
                },
            );
            if let Some(parent_stage_id) = child.parent_stage_id.as_ref() {
                if let Some(stage_node_id) = stage_node_ids.get(parent_stage_id) {
                    action_graph_edges.push(RunActionGraphEdgeRecord {
                        from_id: stage_node_id.clone(),
                        to_id: worker_node_id,
                        kind: ACTION_GRAPH_EDGE_KIND_DELEGATES.to_string(),
                        label: Some(child.worker_name.clone()),
                    });
                }
            }
            RunWorkerLineageRecord {
                worker_id: child.worker_id.clone(),
                worker_name: child.worker_name.clone(),
                parent_stage_id: child.parent_stage_id.clone(),
                task: child.task.clone(),
                status: child.status.clone(),
                session_id: child.session_id.clone(),
                parent_session_id: child.parent_session_id.clone(),
                run_id: child.run_id.clone(),
                run_path: child.run_path.clone(),
                snapshot_path: child.snapshot_path.clone(),
            }
        })
        .collect::<Vec<_>>();

    if run.transcript.is_some() {
        transcript_pointers.push(RunTranscriptPointerRecord {
            id: "run:transcript".to_string(),
            label: "Run transcript".to_string(),
            kind: "embedded_transcript".to_string(),
            location: "run.transcript".to_string(),
            path: run.persisted_path.clone(),
            available: true,
        });
        if let Some(transcript) = run.transcript.as_ref() {
            compaction_events.extend(compaction_events_from_transcript(
                transcript,
                None,
                None,
                "run.transcript",
                persisted_path,
            ));
        }
    }

    if let Some(path) = persisted_path {
        if let Some(sidecar_path) = llm_transcript_sidecar_path(path) {
            transcript_pointers.push(RunTranscriptPointerRecord {
                id: "run:llm_transcript".to_string(),
                label: "LLM transcript sidecar".to_string(),
                kind: "llm_jsonl".to_string(),
                location: "run sidecar".to_string(),
                path: Some(sidecar_path.to_string_lossy().into_owned()),
                available: sidecar_path.exists(),
            });
        }
        daemon_events.extend(daemon_events_from_sidecar(path));
    }

    RunObservabilityRecord {
        schema_version: 4,
        planner_rounds,
        research_fact_count,
        action_graph_nodes,
        action_graph_edges,
        worker_lineage,
        verification_outcomes,
        transcript_pointers,
        compaction_events,
        daemon_events,
    }
}

pub(super) fn refresh_run_observability(run: &mut RunRecord, persisted_path: Option<&Path>) {
    run.observability = Some(derive_run_observability(run, persisted_path));
}
