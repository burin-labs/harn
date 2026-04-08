//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

#[path = "agents_workers.rs"]
pub(super) mod agents_workers;
#[path = "records.rs"]
pub(super) mod records;
#[path = "workflow.rs"]
pub(super) mod workflow;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use self::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, emit_worker_event,
    load_worker_state_snapshot, next_worker_id, parse_worker_config, persist_worker_state_snapshot,
    spawn_worker_task, with_worker_state, worker_id_from_value, worker_snapshot_path,
    worker_summary, WorkerConfig, WorkerState, WORKER_REGISTRY,
};
use crate::orchestration::{
    normalize_workflow_value, pop_execution_policy, push_execution_policy, select_artifacts,
    ArtifactRecord, CapabilityPolicy, ContextPolicy, MutationSessionRecord, TranscriptPolicy,
    WorkflowGraph,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) use self::records::{parse_artifact_list, parse_context_policy};
fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("agents encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

pub(crate) fn parse_transcript_policy(
    value: Option<&VmValue>,
) -> Result<TranscriptPolicy, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("transcript policy parse error: {e}"))),
        None => Ok(TranscriptPolicy::default()),
    }
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    // ── Agent definition/config builtins ─────────────────────────────

    // agent(name, config) -> agent dict
    // config = {system, provider?, model?, tools?, max_iterations?, tool_format?}
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

    // agent_config(agent) -> {prompt, system, options} for passing to agent_loop
    // Usage: let cfg = agent_config(my_agent, "Do something")
    //        let result = agent_loop(cfg.prompt, cfg.system, cfg.options)
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

        // Build options dict from agent config for agent_loop
        let mut options = BTreeMap::new();
        for key in [
            "provider",
            "model",
            "tools",
            "max_iterations",
            "tool_format",
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

    // agent_name(agent) -> string
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

    // ── Worker lifecycle builtins ────────────────────────────────────

    vm.register_async_builtin("spawn_agent", |args| async move {
        let config = args
            .first()
            .ok_or_else(|| VmError::Runtime("spawn_agent: missing config".to_string()))?;
        let init = parse_worker_config(config)?;
        let worker_id = next_worker_id();
        let created_at = uuid::Uuid::now_v7().to_string();
        let mode = match &init.config {
            WorkerConfig::Workflow { .. } => "workflow",
            WorkerConfig::Stage { .. } => "stage",
        }
        .to_string();
        let mut audit = init.audit.clone().normalize();
        audit.worker_id = Some(worker_id.clone());
        audit.execution_kind = Some(mode.clone());
        let state = Rc::new(RefCell::new(WorkerState {
            id: worker_id.clone(),
            name: init.name,
            task: init.task.clone(),
            status: "running".to_string(),
            created_at: created_at.clone(),
            started_at: created_at,
            finished_at: None,
            mode,
            history: vec![init.task],
            config: init.config,
            handle: None,
            cancel_token: Arc::new(AtomicBool::new(false)),
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
            let handle =
                state.borrow_mut().handle.take().ok_or_else(|| {
                    VmError::Runtime("spawn_agent: worker did not start".to_string())
                })?;
            let _ = handle.await.map_err(|error| {
                VmError::Runtime(format!("spawn_agent worker join error: {error}"))
            })??;
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
            worker.cancel_token = Arc::new(AtomicBool::new(false));
            worker.task = next_task.clone();
            worker.history.push(next_task.clone());
            worker.status = "running".to_string();
            worker.started_at = uuid::Uuid::now_v7().to_string();
            worker.finished_at = None;
            worker.latest_error = None;
            worker.latest_payload = None;
            let next_artifacts =
                apply_worker_artifact_policy(&worker.artifacts, &worker.carry_policy);
            let next_transcript = apply_worker_transcript_policy(
                worker.transcript.clone(),
                &worker.carry_policy.transcript_policy,
            );
            let worker_parent = worker.id.clone();
            let resume_workflow = worker.carry_policy.resume_workflow;
            let child_run_path = worker.child_run_path.clone();
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
                        *artifacts = next_artifacts.clone();
                    }
                    *transcript = next_transcript;
                }
            }
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            drop(worker);
            spawn_worker_task(state.clone());
            let summary = worker_summary(&state.borrow())?;
            Ok(summary)
        })
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
                let handle = state.borrow_mut().handle.take();
                if let Some(handle) = handle {
                    let _ = handle.await.map_err(|error| {
                        VmError::Runtime(format!("wait_agent join error: {error}"))
                    })??;
                }
                results.push(worker_summary(&state.borrow())?);
            }
            return Ok(VmValue::List(Rc::new(results)));
        }
        let worker_id = worker_id_from_value(target)?;
        let state = with_worker_state(&worker_id, Ok)?;
        let handle = state.borrow_mut().handle.take();
        if let Some(handle) = handle {
            let _ = handle
                .await
                .map_err(|error| VmError::Runtime(format!("wait_agent join error: {error}")))??;
        }
        let summary = worker_summary(&state.borrow())?;
        Ok(summary)
    });

    vm.register_builtin("close_agent", |args, _out| {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("close_agent: missing worker handle".to_string()))?;
        let worker_id = worker_id_from_value(target)?;
        with_worker_state(&worker_id, |state| {
            let mut worker = state.borrow_mut();
            worker.cancel_token.store(true, Ordering::SeqCst);
            if let Some(handle) = worker.handle.take() {
                handle.abort();
            }
            worker.status = "cancelled".to_string();
            worker.finished_at = Some(uuid::Uuid::now_v7().to_string());
            worker.latest_error = Some("worker cancelled".to_string());
            if worker.carry_policy.persist_state {
                persist_worker_state_snapshot(&worker)?;
            }
            emit_worker_event(&worker, "cancelled");
            let summary = worker_summary(&worker)?;
            Ok(summary)
        })
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

    // ── Delegate to submodule registration ───────────────────────────

    records::register_record_builtins(vm);
    workflow::register_workflow_builtins(vm);
}
