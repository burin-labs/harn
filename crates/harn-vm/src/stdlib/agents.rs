//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

#[path = "agents_workers/mod.rs"]
pub(super) mod agents_workers;
#[path = "records.rs"]
pub(super) mod records;
#[path = "agents_sub_agent.rs"]
mod sub_agent;
#[path = "workflow/mod.rs"]
pub(super) mod workflow;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use self::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, emit_worker_event,
    ensure_worker_config_session_ids, load_worker_state_snapshot, next_worker_id,
    parse_worker_config, persist_worker_state_snapshot, spawn_worker_task, with_worker_state,
    worker_event_snapshot, worker_id_from_value, worker_request_for_config, worker_snapshot_path,
    worker_summary, worker_trigger_payload_text, worker_wait_blocks, WorkerConfig, WorkerState,
    WORKER_REGISTRY,
};
use self::sub_agent::{execute_sub_agent, parse_sub_agent_request};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) use self::records::{parse_artifact_list, parse_context_policy};
fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("agents encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[derive(Clone, Debug, Default)]
pub(super) struct SubAgentRunSpec {
    pub(super) name: String,
    pub(super) task: String,
    pub(super) system: Option<String>,
    pub(super) options: BTreeMap<String, VmValue>,
    pub(super) returns_schema: Option<VmValue>,
    pub(super) session_id: String,
    pub(super) parent_session_id: Option<String>,
}

pub(super) struct SubAgentExecutionResult {
    pub(super) payload: serde_json::Value,
    pub(super) transcript: VmValue,
}

fn restart_worker_run(
    worker: &mut WorkerState,
    next_task: &str,
    clear_latest_payload: bool,
) -> Result<(), VmError> {
    worker.cancel_token = Arc::new(AtomicBool::new(false));
    worker.task = next_task.to_string();
    worker.history.push(next_task.to_string());
    worker.status = "running".to_string();
    worker.started_at = uuid::Uuid::now_v7().to_string();
    worker.finished_at = None;
    worker.awaiting_started_at = None;
    worker.awaiting_since = None;
    worker.latest_error = None;
    if clear_latest_payload {
        worker.latest_payload = None;
    }
    let next_artifacts = apply_worker_artifact_policy(&worker.artifacts, &worker.carry_policy);
    let next_transcript =
        apply_worker_transcript_policy(worker.transcript.clone(), &worker.carry_policy)?;
    worker.transcript = next_transcript.clone();
    let worker_parent = worker.id.clone();
    let worker_id = worker.id.clone();
    let resume_workflow = worker.carry_policy.resume_workflow;
    let child_run_path = worker.child_run_path.clone();
    ensure_worker_config_session_ids(&mut worker.config, &worker_id);
    match &mut worker.config {
        WorkerConfig::Workflow {
            artifacts, options, ..
        } => {
            if !next_artifacts.is_empty() {
                *artifacts = next_artifacts.clone();
            }
            options.insert(
                "parent_worker_id".to_string(),
                VmValue::String(Rc::from(worker_parent)),
            );
            if let Some(transcript) = next_transcript.clone() {
                options.insert("transcript".to_string(), transcript);
            } else {
                options.remove("transcript");
            }
            if resume_workflow {
                if let Some(child_run_path) = child_run_path {
                    options.insert(
                        "resume_path".to_string(),
                        VmValue::String(Rc::from(child_run_path)),
                    );
                }
            } else {
                options.remove("resume_path");
            }
        }
        WorkerConfig::Stage {
            artifacts,
            transcript,
            ..
        } => {
            if !next_artifacts.is_empty() {
                *artifacts = next_artifacts;
            }
            *transcript = next_transcript;
        }
        WorkerConfig::SubAgent { spec } => {
            spec.task = next_task.to_string();
            if matches!(
                worker.carry_policy.transcript_mode.as_str(),
                "fork" | "reset"
            ) {
                spec.session_id = format!("sub_agent_session_{}", uuid::Uuid::now_v7());
            }
        }
    }
    Ok(())
}

async fn wait_for_worker_terminal(
    state: Rc<RefCell<WorkerState>>,
    context: &str,
) -> Result<(), VmError> {
    loop {
        let handle = state.borrow_mut().handle.take();
        if let Some(handle) = handle {
            let _ = handle
                .await
                .map_err(|error| VmError::Runtime(format!("{context} join error: {error}")))??;
            continue;
        }
        if !worker_wait_blocks(&state.borrow().status) {
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    vm.register_builtin("agent", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let config = match args.get(1) {
            Some(VmValue::Dict(map)) => (**map).clone(),
            Some(_) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent: second argument must be a config dict",
                ))));
            }
            None => BTreeMap::new(),
        };

        let mut agent = config;
        agent.insert("_type".to_string(), VmValue::String(Rc::from("agent")));
        agent.insert("name".to_string(), VmValue::String(Rc::from(name)));

        Ok(VmValue::Dict(Rc::new(agent)))
    });

    vm.register_builtin("agent_config", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "agent_config: requires agent and prompt",
            ))));
        }

        let agent = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent",
                ))));
            }
        };

        match agent.get("_type") {
            Some(VmValue::String(t)) if &**t == "agent" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent (created with agent())",
                ))));
            }
        }

        let mut options = BTreeMap::new();
        for key in [
            "provider",
            "model",
            "thinking",
            "tools",
            "max_iterations",
            "tool_format",
            "structural_experiment",
            "context_callback",
            "context_filter",
            "tool_retries",
            "tool_backoff_ms",
        ] {
            if let Some(val) = agent.get(key) {
                options.insert(key.to_string(), val.clone());
            }
        }

        let prompt = args[1].clone();
        let system = agent.get("system").cloned().unwrap_or(VmValue::Nil);

        let mut result = BTreeMap::new();
        result.insert("prompt".to_string(), prompt);
        result.insert("system".to_string(), system);
        result.insert("options".to_string(), VmValue::Dict(Rc::new(options)));

        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("agent_name", |args, _out| {
        let agent = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_name: argument must be an agent",
                ))));
            }
        };
        Ok(agent.get("name").cloned().unwrap_or(VmValue::Nil))
    });

    vm.register_async_builtin("sub_agent_run", |args| async move {
        let request = parse_sub_agent_request(&args)?;
        if !request.background {
            let result = execute_sub_agent(request.spec).await?;
            return Ok(crate::stdlib::json_to_vm_value(&result.payload));
        }

        let worker_id = next_worker_id();
        let created_at = uuid::Uuid::now_v7().to_string();
        let mut audit = agents_workers::inherited_worker_audit("sub_agent");
        audit.worker_id = Some(worker_id.clone());
        let execution = request.execution;
        let worker_policy = request.worker_policy;
        let mut carry_policy = request.carry_policy;
        carry_policy.policy = worker_policy;
        let spec = request.spec;
        let worker_name = spec.name.clone();
        let worker_task = spec.task.clone();
        let mut config = WorkerConfig::SubAgent {
            spec: Box::new(spec),
        };
        ensure_worker_config_session_ids(&mut config, &worker_id);
        let original_request = worker_request_for_config(&worker_task, &config);
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: worker_name,
            task: worker_task.clone(),
            status: "running".to_string(),
            created_at: created_at.clone(),
            started_at: created_at,
            finished_at: None,
            awaiting_started_at: None,
            awaiting_since: None,
            mode: "sub_agent".to_string(),
            history: vec![worker_task],
            config,
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            request: original_request,
            latest_payload: None,
            latest_error: None,
            transcript: None,
            artifacts: Vec::new(),
            parent_worker_id: None,
            parent_stage_id: None,
            child_run_id: None,
            child_run_path: None,
            carry_policy,
            execution,
            snapshot_path: worker_snapshot_path(&worker_id),
            audit,
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
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("spawn_agent", |args| async move {
        let config = args
            .first()
            .ok_or_else(|| VmError::Runtime("spawn_agent: missing config".to_string()))?;
        let mut init = parse_worker_config(config)?;
        let worker_id = next_worker_id();
        let created_at = uuid::Uuid::now_v7().to_string();
        ensure_worker_config_session_ids(&mut init.config, &worker_id);
        let mode = match &init.config {
            WorkerConfig::Workflow { .. } => "workflow",
            WorkerConfig::Stage { .. } => "stage",
            WorkerConfig::SubAgent { .. } => "sub_agent",
        }
        .to_string();
        let mut audit = init.audit.clone().normalize();
        audit.worker_id = Some(worker_id.clone());
        audit.execution_kind = Some(mode.clone());
        let original_request = worker_request_for_config(&init.task, &init.config);
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: init.name,
            task: init.task.clone(),
            status: "running".to_string(),
            created_at: created_at.clone(),
            started_at: created_at,
            finished_at: None,
            awaiting_started_at: None,
            awaiting_since: None,
            mode,
            history: vec![init.task],
            config: init.config,
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
            request: original_request,
            latest_payload: None,
            latest_error: None,
            transcript: None,
            artifacts: Vec::new(),
            parent_worker_id: None,
            parent_stage_id: None,
            child_run_id: None,
            child_run_path: None,
            carry_policy: init.carry_policy,
            execution: init.execution,
            snapshot_path: worker_snapshot_path(&worker_id),
            audit,
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
        if init.wait {
            wait_for_worker_terminal(state.clone(), "spawn_agent worker").await?;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("send_input", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Runtime(
                "send_input: requires worker handle and task text".to_string(),
            ));
        }
        let worker_id = worker_id_from_value(&args[0])?;
        let next_task = args[1].display();
        if next_task.is_empty() {
            return Err(VmError::Runtime(
                "send_input: task text must not be empty".to_string(),
            ));
        }
        with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            if worker.status == "running" {
                return Err(VmError::Runtime(format!(
                    "send_input: worker {} is still running",
                    worker.id
                )));
            }
            restart_worker_run(&mut worker, &next_task, true)?;
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            drop(worker);
            spawn_worker_task(state.clone());
            let summary = worker_summary(&state.borrow())?;
            Ok(summary)
        })
    });

    vm.register_async_builtin("worker_trigger", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Runtime(
                "worker_trigger: requires worker handle and payload".to_string(),
            ));
        }
        let worker_id = worker_id_from_value(&args[0])?;
        let next_task = worker_trigger_payload_text(&args[1]);
        if next_task.trim().is_empty() {
            return Err(VmError::Runtime(
                "worker_trigger: payload must not be empty".to_string(),
            ));
        }
        // Snapshot the about-to-resume worker state and validate
        // preconditions in a `with_worker_state` borrow that also
        // restarts the run. The borrow has to be dropped before we
        // await the lifecycle event emission, so we pull the snapshot
        // out and run `emit_worker_event` after the closure returns.
        let progressed_snapshot = with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            if !worker.carry_policy.retriggerable {
                return Err(VmError::Runtime(format!(
                    "worker_trigger: worker {} is not retriggerable",
                    worker.id
                )));
            }
            if worker.status == "running" {
                return Err(VmError::Runtime(format!(
                    "worker_trigger: worker {} is still running",
                    worker.id
                )));
            }
            if worker.status != "awaiting" {
                return Err(VmError::Runtime(format!(
                    "worker_trigger: worker {} is not awaiting (status={})",
                    worker.id, worker.status
                )));
            }
            restart_worker_run(&mut worker, &next_task, false)?;
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            let snapshot = worker_event_snapshot(&worker);
            drop(worker);
            spawn_worker_task(state.clone());
            Ok(snapshot)
        })?;
        // Emit the progressed lifecycle *after* the new cycle has been
        // spawned. Hosts see `progressed` (re-arming the run) followed
        // by `running` from the inner `WorkerSpawned` emission.
        emit_worker_event(
            &progressed_snapshot,
            crate::agent_events::WorkerEvent::WorkerProgressed,
        )
        .await?;
        with_worker_state(&worker_id, |state| worker_summary(&state.borrow()))
    });

    vm.register_builtin("resume_agent", |args, _out| {
        let target = args
            .first()
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                VmError::Runtime("resume_agent: missing worker id or snapshot path".to_string())
            })?;
        let state = Rc::new(RefCell::new(load_worker_state_snapshot(&target)?));
        let worker_id = state.borrow().id.clone();
        {
            let mut worker = state.borrow_mut();
            ensure_worker_config_session_ids(&mut worker.config, &worker_id);
        }
        WORKER_REGISTRY.with(|registry| {
            registry.borrow_mut().insert(worker_id, state.clone());
        });
        if state.borrow().carry_policy.persist_state {
            persist_worker_state_snapshot(&state.borrow())?;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("wait_agent", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("wait_agent: missing worker handle".to_string()))?;
        if let VmValue::List(list) = target {
            let mut results = Vec::new();
            for item in list.iter() {
                let worker_id = worker_id_from_value(item)?;
                let state = with_worker_state(&worker_id, Ok)?;
                wait_for_worker_terminal(state.clone(), "wait_agent").await?;
                results.push(worker_summary(&state.borrow())?);
            }
            return Ok(VmValue::List(Rc::new(results)));
        }
        let worker_id = worker_id_from_value(target)?;
        let state = with_worker_state(&worker_id, Ok)?;
        wait_for_worker_terminal(state.clone(), "wait_agent").await?;
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_async_builtin("close_agent", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("close_agent: missing worker handle".to_string()))?;
        let worker_id = worker_id_from_value(target)?;
        let state = with_worker_state(&worker_id, |state| Ok(state.clone()))?;
        let (snapshot, summary) = {
            let mut worker = state.borrow_mut();
            worker.cancel_token.store(true, Ordering::SeqCst);
            if let Some(handle) = worker.handle.take() {
                handle.abort();
            }
            worker.status = "cancelled".to_string();
            worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
            worker.awaiting_started_at = None;
            worker.awaiting_since = None;
            worker.latest_error = Some("worker cancelled".to_string());
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            let snapshot = worker_event_snapshot(&worker);
            let summary = worker_summary(&worker)?;
            (snapshot, summary)
        };
        emit_worker_event(&snapshot, crate::agent_events::WorkerEvent::WorkerCancelled).await?;
        Ok(summary)
    });

    vm.register_builtin("list_agents", |_args, _out| {
        let workers = WORKER_REGISTRY.with(|registry| {
            registry
                .borrow()
                .values()
                .map(|state| worker_summary(&state.borrow()))
                .collect::<Result<Vec<_>, _>>()
        })?;
        Ok(VmValue::List(Rc::new(workers)))
    });

    records::register_record_builtins(vm);
    workflow::register_workflow_builtins(vm);
}
