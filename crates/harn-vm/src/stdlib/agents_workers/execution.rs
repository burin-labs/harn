use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::bridge::{emit_worker_event, worker_snapshot_path};
use super::config::{parse_execution_profile_json, persist_worker_state_snapshot};
use super::worktree::{
    cleanup_worker_execution, ensure_worker_worktree, WorkerMutationSessionResetGuard,
};
use super::{
    clone_worker_state, next_worker_id, worker_provenance, worker_request_for_config,
    WorkerCarryPolicy, WorkerConfig, WorkerExecutionProfile, WorkerExecutionResult, WorkerState,
    WORKER_REGISTRY,
};
use crate::orchestration::{
    current_approval_policy, current_execution_policy, pop_execution_policy, push_execution_policy,
    ArtifactRecord, ContextPolicy, MutationSessionRecord,
};
use crate::value::{VmError, VmValue};

fn execution_record(profile: &WorkerExecutionProfile) -> crate::orchestration::RunExecutionRecord {
    let mut record = crate::orchestration::RunExecutionRecord {
        cwd: profile.cwd.clone(),
        source_dir: None,
        env: profile.env.clone(),
        adapter: None,
        repo_path: None,
        worktree_path: None,
        branch: None,
        base_ref: None,
        cleanup: None,
    };
    if let Some(worktree) = &profile.worktree {
        record.adapter = Some("worktree".to_string());
        record.repo_path = Some(worktree.repo.clone());
        record.worktree_path = worktree.path.clone().or_else(|| profile.cwd.clone());
        record.branch = worktree.branch.clone();
        record.base_ref = worktree.base_ref.clone();
        record.cleanup = worktree.cleanup.clone();
    }
    record
}

async fn execute_worker_config(
    worker_id: String,
    task: String,
    config: WorkerConfig,
    mut execution: WorkerExecutionProfile,
    audit: MutationSessionRecord,
) -> Result<WorkerExecutionResult, VmError> {
    ensure_worker_worktree(&worker_id, &mut execution)?;
    let execution_record = execution_record(&execution);
    crate::stdlib::process::set_thread_execution_context(Some(execution_record.clone()));
    crate::orchestration::install_current_mutation_session(Some(audit));
    let _mutation_guard = WorkerMutationSessionResetGuard;
    match config {
        WorkerConfig::Workflow {
            mut graph,
            artifacts,
            mut options,
        } => {
            if let Some(parent_worker_id) = options
                .get("parent_worker_id")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
            {
                graph.metadata.insert(
                    "parent_worker_id".to_string(),
                    serde_json::json!(parent_worker_id),
                );
            }
            if let Some(parent_stage_id) = options
                .get("parent_stage_id")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())
            {
                graph.metadata.insert(
                    "parent_stage_id".to_string(),
                    serde_json::json!(parent_stage_id),
                );
            }
            options.insert(
                "execution".to_string(),
                crate::stdlib::json_to_vm_value(
                    &serde_json::to_value(&execution_record).unwrap_or_default(),
                ),
            );
            options.insert("delegated".to_string(), VmValue::Bool(true));
            let result =
                super::super::workflow::execute_workflow(task, *graph, artifacts, options).await;
            crate::stdlib::process::set_thread_execution_context(None);
            cleanup_worker_execution(&execution);
            let result = result?;
            let dict = result.as_dict().ok_or_else(|| {
                VmError::Runtime("workflow execution returned a non-dict result".to_string())
            })?;
            let transcript = dict.get("transcript").cloned();
            let artifacts = super::super::parse_artifact_list(dict.get("artifacts"))?;
            Ok(WorkerExecutionResult {
                payload: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(dict.clone()))),
                transcript,
                artifacts,
                execution,
            })
        }
        WorkerConfig::Stage {
            node,
            artifacts,
            transcript,
        } => {
            let _ = transcript;
            let result = crate::orchestration::execute_stage_node(
                "delegated_worker",
                &node,
                &task,
                &artifacts,
            )
            .await;
            crate::stdlib::process::set_thread_execution_context(None);
            cleanup_worker_execution(&execution);
            let (result, produced, next_transcript) = result?;
            Ok(WorkerExecutionResult {
                payload: serde_json::json!({
                    "status": "completed",
                    "mode": "stage",
                    "task": task,
                    "result": result,
                    "artifacts": produced,
                    "transcript": next_transcript.as_ref().map(crate::llm::vm_value_to_json),
                    "execution": execution_record,
                }),
                transcript: next_transcript,
                artifacts: produced,
                execution,
            })
        }
        WorkerConfig::SubAgent { spec } => {
            let result = super::super::execute_sub_agent(spec.as_ref().clone()).await?;
            Ok(WorkerExecutionResult {
                payload: result.payload,
                transcript: Some(result.transcript),
                artifacts: Vec::new(),
                execution,
            })
        }
    }
}

pub(in super::super) fn spawn_worker_task(state: Rc<RefCell<WorkerState>>) {
    let child_vm = crate::vm::clone_async_builtin_child_vm();
    let (worker_id, task, config, execution, cancel_token, worker_policy, audit) = {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker).ok();
        }
        emit_worker_event(&worker, "running");
        (
            worker.id.clone(),
            worker.task.clone(),
            worker.config.clone(),
            worker.execution.clone(),
            worker.cancel_token.clone(),
            worker.carry_policy.policy.clone(),
            worker.audit.clone(),
        )
    };

    let state_for_task = state.clone();
    let handle = tokio::task::spawn_local(async move {
        let _child_vm_guard = child_vm.map(crate::vm::install_async_builtin_child_vm);
        if cancel_token.load(Ordering::SeqCst) {
            return Err(VmError::CategorizedError {
                message: "worker cancelled before start".to_string(),
                category: crate::value::ErrorCategory::Cancelled,
            });
        }

        if let Some(ref policy) = worker_policy {
            push_execution_policy(policy.clone());
        }
        let worker_approval = audit.approval_policy.clone();
        if let Some(ref approval) = worker_approval {
            crate::orchestration::push_approval_policy(approval.clone());
        }
        let result = execute_worker_config(worker_id, task, config, execution, audit).await;
        if worker_approval.is_some() {
            crate::orchestration::pop_approval_policy();
        }
        if worker_policy.is_some() {
            pop_execution_policy();
        }
        {
            let mut worker = state_for_task.borrow_mut();
            worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
            match &result {
                Ok(executed) => {
                    worker.status = "completed".to_string();
                    worker.latest_payload = Some(executed.payload.clone());
                    worker.latest_error = None;
                    worker.transcript = executed.transcript.clone();
                    worker.artifacts = executed.artifacts.clone();
                    worker.execution = executed.execution.clone();
                    worker.child_run_id = executed
                        .payload
                        .get("run")
                        .and_then(|run| run.get("id"))
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    worker.child_run_path = executed
                        .payload
                        .get("path")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    if let Some(run_id) = &worker.child_run_id {
                        worker.audit.run_id = Some(run_id.clone());
                    }
                    if worker.carry_policy.persist_state {
                        persist_worker_state_snapshot(&worker).ok();
                    }
                    emit_worker_event(&worker, "completed");
                }
                Err(error) => {
                    if matches!(
                        error,
                        VmError::CategorizedError {
                            category: crate::value::ErrorCategory::Cancelled,
                            ..
                        }
                    ) {
                        worker.status = "cancelled".to_string();
                    } else {
                        worker.status = "failed".to_string();
                    }
                    worker.latest_error = Some(error.to_string());
                    if worker.carry_policy.persist_state {
                        persist_worker_state_snapshot(&worker).ok();
                    }
                    emit_worker_event(&worker, &worker.status.clone());
                }
            }
        }
        result
    });

    state.borrow_mut().handle = Some(handle);
}

fn worker_result_artifact(
    node_id: &str,
    state: &WorkerState,
    payload: &serde_json::Value,
    produced: &[ArtifactRecord],
    lineage: &[String],
) -> ArtifactRecord {
    let summary = payload
        .get("result")
        .or_else(|| payload.get("visible_text"))
        .or_else(|| payload.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: format!("{node_id}_worker_result_{}", uuid::Uuid::now_v7()),
        kind: "worker_result".to_string(),
        title: Some(format!("worker result {}", state.name)),
        text: if summary.is_empty() { None } else { Some(summary) },
        data: Some(serde_json::json!({
            "worker_id": state.id,
            "worker_name": state.name,
            "request": state.request,
            "provenance": worker_provenance(state),
            "execution": state.execution,
            "payload": payload,
            "produced_artifact_ids": produced.iter().map(|artifact| artifact.id.clone()).collect::<Vec<_>>(),
        })),
        source: Some(node_id.to_string()),
        created_at: uuid::Uuid::now_v7().to_string(),
        freshness: Some("fresh".to_string()),
        priority: Some(95),
        lineage: lineage.to_vec(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata: BTreeMap::from([
            ("worker_id".to_string(), serde_json::json!(state.id)),
            ("worker_name".to_string(), serde_json::json!(state.name)),
            ("delegated".to_string(), serde_json::json!(true)),
        ]),
    }
    .normalize()
}

pub(in super::super) async fn execute_delegated_stage(
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let worker_id = next_worker_id();
    let worker_name = node
        .metadata
        .get("worker_name")
        .and_then(|value| value.as_str())
        .unwrap_or(node_id)
        .to_string();
    let mut stage_node = node.clone();
    stage_node.kind = "stage".to_string();
    let execution = parse_execution_profile_json(node.metadata.get("execution"))?;
    let config = WorkerConfig::Stage {
        node: Box::new(stage_node),
        artifacts: artifacts.to_vec(),
        transcript,
    };
    let original_request = worker_request_for_config(task, &config);
    let state = Rc::new(RefCell::new(WorkerState {
        id: worker_id.clone(),
        name: worker_name.clone(),
        task: task.to_string(),
        status: "running".to_string(),
        created_at: uuid::Uuid::now_v7().to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        mode: "delegated_stage".to_string(),
        history: vec![task.to_string()],
        config,
        handle: None,
        cancel_token: Arc::new(AtomicBool::new(false)),
        request: original_request,
        latest_payload: None,
        latest_error: None,
        transcript: None,
        artifacts: Vec::new(),
        parent_worker_id: None,
        parent_stage_id: Some(node_id.to_string()),
        child_run_id: None,
        child_run_path: None,
        carry_policy: WorkerCarryPolicy {
            artifact_mode: "inherit".to_string(),
            context_policy: ContextPolicy::default(),
            resume_workflow: true,
            persist_state: true,
            policy: current_execution_policy(),
        },
        execution,
        snapshot_path: worker_snapshot_path(&worker_id),
        audit: MutationSessionRecord {
            parent_session_id: Some(node_id.to_string()),
            mutation_scope: "read_only".to_string(),
            approval_policy: current_approval_policy(),
            execution_kind: Some("delegated_stage".to_string()),
            ..Default::default()
        }
        .normalize(),
    }));
    {
        let worker = state.borrow();
        if worker.carry_policy.persist_state {
            persist_worker_state_snapshot(&worker)?;
        }
    }
    WORKER_REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .insert(worker_id.clone(), state.clone());
    });
    spawn_worker_task(state.clone());
    let handle = state
        .borrow_mut()
        .handle
        .take()
        .ok_or_else(|| VmError::Runtime("delegated stage did not start".to_string()))?;
    let executed = handle
        .await
        .map_err(|error| VmError::Runtime(format!("delegated stage join error: {error}")))??;
    let mut result = executed.payload.clone();
    result["worker"] = clone_worker_state(&state.borrow());
    let mut produced = executed.artifacts.clone();
    for artifact in &mut produced {
        artifact
            .metadata
            .insert("worker_id".to_string(), serde_json::json!(worker_id));
        artifact.metadata.insert(
            "worker_name".to_string(),
            serde_json::json!(worker_name.clone()),
        );
        artifact
            .metadata
            .insert("delegated".to_string(), serde_json::json!(true));
    }
    produced.push(worker_result_artifact(
        node_id,
        &state.borrow(),
        &result,
        &executed.artifacts,
        &artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>(),
    ));
    Ok((result, produced, executed.transcript))
}
