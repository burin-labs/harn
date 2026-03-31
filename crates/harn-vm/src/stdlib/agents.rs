//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

#[path = "agents_workers.rs"]
mod agents_workers;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use self::agents_workers::{
    apply_worker_artifact_policy, apply_worker_transcript_policy, emit_worker_event,
    execute_delegated_stage, load_worker_state_snapshot, next_worker_id, parse_worker_config,
    persist_worker_state_snapshot, spawn_worker_task, with_worker_state, worker_id_from_value,
    worker_snapshot_path, worker_summary, WorkerConfig, WorkerState, WORKER_REGISTRY,
};
use crate::orchestration::{
    append_audit_entry, builtin_ceiling, diff_run_records, evaluate_run_against_fixture,
    evaluate_run_suite, evaluate_run_suite_manifest, load_run_record, next_nodes_for,
    normalize_artifact, normalize_eval_suite_manifest, normalize_run_record,
    normalize_workflow_value, pop_execution_policy, push_execution_policy,
    render_artifacts_context, render_unified_diff, replay_fixture_from_run, save_run_record,
    select_artifacts, validate_workflow, ArtifactRecord, CapabilityPolicy, ContextPolicy,
    ReplayFixture, RunCheckpointRecord, RunChildRecord, RunExecutionRecord, RunRecord,
    RunStageAttemptRecord, RunStageRecord, RunTransitionRecord, TranscriptPolicy, WorkflowEdge,
    WorkflowGraph,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn parse_transcript_policy(value: Option<&VmValue>) -> Result<TranscriptPolicy, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("transcript policy parse error: {e}"))),
        None => Ok(TranscriptPolicy::default()),
    }
}

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
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

    vm.register_builtin("workflow_graph", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let graph = normalize_workflow_value(&input)?;
        to_vm(&graph)
    });

    vm.register_builtin("workflow_validate", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let graph = normalize_workflow_value(&input)?;
        let ceiling = args.get(1).map(normalize_policy).transpose()?;
        to_vm(&validate_workflow(
            &graph,
            ceiling.as_ref().or(Some(&builtin_ceiling())),
        ))
    });

    vm.register_builtin("workflow_inspect", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let graph = normalize_workflow_value(&input)?;
        let ceiling = args.get(1).map(normalize_policy).transpose()?;
        let builtin = builtin_ceiling();
        let report = validate_workflow(&graph, ceiling.as_ref().or(Some(&builtin)));
        to_vm(&serde_json::json!({
            "graph": graph,
            "validation": report,
            "node_count": graph.nodes.len(),
            "edge_count": graph.edges.len(),
        }))
    });

    vm.register_builtin("workflow_policy_report", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let graph = normalize_workflow_value(&input)?;
        let ceiling = args.get(1).map(normalize_policy).transpose()?;
        let builtin = builtin_ceiling();
        let effective_ceiling = ceiling.unwrap_or(builtin);
        let report = validate_workflow(&graph, Some(&effective_ceiling));
        to_vm(&serde_json::json!({
            "workflow_policy": graph.capability_policy,
            "ceiling": effective_ceiling,
            "validation": report,
            "nodes": graph.nodes.iter().map(|(node_id, node)| serde_json::json!({
                "node_id": node_id,
                "policy": node.capability_policy,
                "tools": node.tools,
            })).collect::<Vec<_>>(),
        }))
    });

    vm.register_builtin("workflow_clone", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let mut graph = normalize_workflow_value(&input)?;
        graph.id = format!("{}_clone", graph.id);
        graph.version += 1;
        append_audit_entry(&mut graph, "clone", None, None, BTreeMap::new());
        to_vm(&graph)
    });

    vm.register_builtin("workflow_insert_node", |args, _out| {
        let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_insert_node: missing workflow".to_string())
        })?)?;
        let node_value = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("workflow_insert_node: missing node".to_string()))?;
        let node_json = crate::llm::vm_value_to_json(node_value);
        let mut node: crate::orchestration::WorkflowNode = serde_json::from_value(node_json)
            .map_err(|e| VmError::Runtime(format!("workflow_insert_node: {e}")))?;
        let node_id = node
            .id
            .clone()
            .or_else(|| {
                node_value
                    .as_dict()
                    .and_then(|d| d.get("id"))
                    .map(|v| v.display())
            })
            .unwrap_or_else(|| format!("node_{}", graph.nodes.len() + 1));
        node.id = Some(node_id.clone());
        graph.nodes.insert(node_id.clone(), node);
        if let Some(VmValue::Dict(edge_dict)) = args.get(2) {
            let edge_json = crate::llm::vm_value_to_json(&VmValue::Dict(edge_dict.clone()));
            let edge: WorkflowEdge = serde_json::from_value(edge_json)
                .map_err(|e| VmError::Runtime(format!("workflow_insert_node edge: {e}")))?;
            graph.edges.push(edge);
        }
        append_audit_entry(
            &mut graph,
            "insert_node",
            Some(node_id),
            None,
            BTreeMap::new(),
        );
        to_vm(&graph)
    });

    vm.register_builtin("workflow_replace_node", |args, _out| {
        let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing workflow".to_string())
        })?)?;
        let node_id = args.get(1).map(|v| v.display()).ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing node id".to_string())
        })?;
        let node_json =
            crate::llm::vm_value_to_json(args.get(2).ok_or_else(|| {
                VmError::Runtime("workflow_replace_node: missing node".to_string())
            })?);
        let mut node: crate::orchestration::WorkflowNode = serde_json::from_value(node_json)
            .map_err(|e| VmError::Runtime(format!("workflow_replace_node: {e}")))?;
        node.id = Some(node_id.clone());
        graph.nodes.insert(node_id.clone(), node);
        append_audit_entry(
            &mut graph,
            "replace_node",
            Some(node_id),
            None,
            BTreeMap::new(),
        );
        to_vm(&graph)
    });

    vm.register_builtin("workflow_rewire", |args, _out| {
        let mut graph =
            normalize_workflow_value(args.first().ok_or_else(|| {
                VmError::Runtime("workflow_rewire: missing workflow".to_string())
            })?)?;
        let from = args
            .get(1)
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("workflow_rewire: missing from".to_string()))?;
        let to = args
            .get(2)
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("workflow_rewire: missing to".to_string()))?;
        let branch = args.get(3).map(|v| v.display()).filter(|s| !s.is_empty());
        graph
            .edges
            .retain(|edge| !(edge.from == from && edge.branch == branch));
        graph.edges.push(WorkflowEdge {
            from: from.clone(),
            to,
            branch,
            label: None,
        });
        append_audit_entry(&mut graph, "rewire", Some(from), None, BTreeMap::new());
        to_vm(&graph)
    });

    vm.register_builtin("workflow_set_model_policy", |args, _out| {
        set_node_policy(args, |node, policy| {
            node.model_policy = serde_json::from_value(policy)
                .map_err(|e| VmError::Runtime(format!("workflow_set_model_policy: {e}")))?;
            Ok(())
        })
    });

    vm.register_builtin("workflow_set_context_policy", |args, _out| {
        set_node_policy(args, |node, policy| {
            node.context_policy = serde_json::from_value(policy)
                .map_err(|e| VmError::Runtime(format!("workflow_set_context_policy: {e}")))?;
            Ok(())
        })
    });

    vm.register_builtin("workflow_set_transcript_policy", |args, _out| {
        set_node_policy(args, |node, policy| {
            node.transcript_policy = serde_json::from_value(policy)
                .map_err(|e| VmError::Runtime(format!("workflow_set_transcript_policy: {e}")))?;
            Ok(())
        })
    });

    vm.register_builtin("workflow_diff", |args, _out| {
        let left = normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing left workflow".to_string())
        })?)?;
        let right = normalize_workflow_value(args.get(1).ok_or_else(|| {
            VmError::Runtime("workflow_diff: missing right workflow".to_string())
        })?)?;
        let left_json = serde_json::to_value(&left).unwrap_or_default();
        let right_json = serde_json::to_value(&right).unwrap_or_default();
        to_vm(&serde_json::json!({
            "changed": left_json != right_json,
            "left": left,
            "right": right,
        }))
    });

    vm.register_builtin("workflow_commit", |args, _out| {
        let mut graph =
            normalize_workflow_value(args.first().ok_or_else(|| {
                VmError::Runtime("workflow_commit: missing workflow".to_string())
            })?)?;
        let reason = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
        let report = validate_workflow(&graph, Some(&builtin_ceiling()));
        if !report.valid {
            return Err(VmError::Runtime(format!(
                "workflow_commit: invalid workflow: {}",
                report.errors.join("; ")
            )));
        }
        append_audit_entry(&mut graph, "commit", None, reason, BTreeMap::new());
        to_vm(&graph)
    });

    vm.register_builtin("artifact", |args, _out| {
        let artifact =
            normalize_artifact(args.first().ok_or_else(|| {
                VmError::Runtime("artifact: missing artifact payload".to_string())
            })?)?;
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_derive", |args, _out| {
        let parent = normalize_artifact(
            args.first()
                .ok_or_else(|| VmError::Runtime("artifact_derive: missing parent".to_string()))?,
        )?;
        let kind = args
            .get(1)
            .map(|v| v.display())
            .unwrap_or_else(|| "artifact".to_string());
        let mut derived = parent.clone();
        derived.id = format!("{}_derived", parent.id);
        derived.kind = kind;
        derived.lineage.push(parent.id);
        if let Some(VmValue::Dict(extra)) = args.get(2) {
            let extra_json = crate::llm::vm_value_to_json(&VmValue::Dict(extra.clone()));
            if let Some(text) = extra_json.get("text").and_then(|v| v.as_str()) {
                derived.text = Some(text.to_string());
            }
        }
        to_vm(&derived.normalize())
    });

    vm.register_builtin("artifact_select", |args, _out| {
        let artifacts = parse_artifact_list(args.first())?;
        let policy = parse_context_policy(args.get(1))?;
        to_vm(&select_artifacts(artifacts, &policy))
    });

    vm.register_builtin("artifact_context", |args, _out| {
        let artifacts = parse_artifact_list(args.first())?;
        let policy = parse_context_policy(args.get(1))?;
        Ok(VmValue::String(Rc::from(render_artifacts_context(
            &select_artifacts(artifacts, &policy),
            &policy,
        ))))
    });

    vm.register_builtin("artifact_workspace_file", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_workspace_file", "path")?;
        let content = require_text_arg(args, 1, "artifact_workspace_file", "content")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "workspace_file",
            Some(path.clone()),
            Some(content.clone()),
            Some(serde_json::json!({"path": path, "content": content})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_workspace_snapshot", |args, _out| {
        let paths = args.first().ok_or_else(|| {
            VmError::Runtime("artifact_workspace_snapshot: missing paths".to_string())
        })?;
        let summary = optional_text_arg(args.get(1));
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("paths".to_string(), crate::llm::vm_value_to_json(paths));
        let artifact = build_helper_artifact(
            "workspace_snapshot",
            Some("workspace snapshot".to_string()),
            summary,
            Some(serde_json::json!({"paths": crate::llm::vm_value_to_json(paths)})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_editor_selection", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_editor_selection", "path")?;
        let text = require_text_arg(args, 1, "artifact_editor_selection", "text")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "editor_selection",
            Some(format!("selection {path}")),
            Some(text.clone()),
            Some(serde_json::json!({"path": path, "text": text})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_verification_result", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_verification_result", "title")?;
        let text = require_text_arg(args, 1, "artifact_verification_result", "text")?;
        let artifact = build_helper_artifact(
            "verification_result",
            Some(title.clone()),
            Some(text.clone()),
            Some(serde_json::json!({"title": title, "text": text})),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_test_result", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_test_result", "title")?;
        let text = require_text_arg(args, 1, "artifact_test_result", "text")?;
        let artifact = build_helper_artifact(
            "test_result",
            Some(title.clone()),
            Some(text.clone()),
            Some(serde_json::json!({"title": title, "text": text})),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_command_result", |args, _out| {
        let command = require_string_arg(args, 0, "artifact_command_result", "command")?;
        let output = args.get(1).ok_or_else(|| {
            VmError::Runtime("artifact_command_result: missing output".to_string())
        })?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options
            .metadata
            .insert("command".to_string(), serde_json::json!(command.clone()));
        let artifact = build_helper_artifact(
            "command_result",
            Some(command.clone()),
            Some(value_to_text(output)),
            Some(serde_json::json!({
                "command": command,
                "output": crate::llm::vm_value_to_json(output)
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_diff", |args, _out| {
        let path = require_string_arg(args, 0, "artifact_diff", "path")?;
        let before = require_text_arg(args, 1, "artifact_diff", "before")?;
        let after = require_text_arg(args, 2, "artifact_diff", "after")?;
        let mut options = parse_artifact_helper_options(args.get(3))?;
        options
            .metadata
            .insert("path".to_string(), serde_json::json!(path.clone()));
        let artifact = build_helper_artifact(
            "diff",
            Some(format!("diff {path}")),
            Some(render_unified_diff(Some(&path), &before, &after)),
            Some(serde_json::json!({"path": path, "before": before, "after": after})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_git_diff", |args, _out| {
        let diff_text = require_text_arg(args, 0, "artifact_git_diff", "diff_text")?;
        let artifact = build_helper_artifact(
            "git_diff",
            Some("git diff".to_string()),
            Some(diff_text.clone()),
            Some(serde_json::json!({"diff": diff_text})),
            parse_artifact_helper_options(args.get(1))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_diff_review", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_diff_review: missing target artifact".to_string())
        })?)?;
        let summary = optional_text_arg(args.get(1));
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "diff_review",
            Some(format!(
                "review {}",
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            summary,
            Some(serde_json::json!({"target_artifact_id": target.id, "target_kind": target.kind})),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_review_decision", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_review_decision: missing target artifact".to_string())
        })?)?;
        let decision = require_string_arg(args, 1, "artifact_review_decision", "decision")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        options
            .metadata
            .insert("decision".to_string(), serde_json::json!(decision.clone()));
        let artifact = build_helper_artifact(
            "review_decision",
            Some(format!(
                "{} {}",
                decision,
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(decision.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "decision": decision
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_patch_proposal", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_patch_proposal: missing target artifact".to_string())
        })?)?;
        let patch = require_text_arg(args, 1, "artifact_patch_proposal", "patch")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "patch_proposal",
            Some(format!(
                "patch for {}",
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(patch.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "patch": patch
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_verification_bundle", |args, _out| {
        let title = require_string_arg(args, 0, "artifact_verification_bundle", "title")?;
        let checks = args.get(1).ok_or_else(|| {
            VmError::Runtime("artifact_verification_bundle: missing checks".to_string())
        })?;
        let artifact = build_helper_artifact(
            "verification_bundle",
            Some(title.clone()),
            Some(value_to_text(checks)),
            Some(serde_json::json!({
                "title": title,
                "checks": crate::llm::vm_value_to_json(checks)
            })),
            parse_artifact_helper_options(args.get(2))?,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("artifact_apply_intent", |args, _out| {
        let target = normalize_artifact(args.first().ok_or_else(|| {
            VmError::Runtime("artifact_apply_intent: missing target artifact".to_string())
        })?)?;
        let intent = require_string_arg(args, 1, "artifact_apply_intent", "intent")?;
        let mut options = parse_artifact_helper_options(args.get(2))?;
        options.lineage.extend(target.lineage.clone());
        options.lineage.push(target.id.clone());
        options.metadata.insert(
            "target_artifact_id".to_string(),
            serde_json::json!(target.id.clone()),
        );
        options.metadata.insert(
            "target_kind".to_string(),
            serde_json::json!(target.kind.clone()),
        );
        let artifact = build_helper_artifact(
            "apply_intent",
            Some(format!(
                "{} {}",
                intent,
                target.title.clone().unwrap_or_else(|| target.id.clone())
            )),
            Some(intent.clone()),
            Some(serde_json::json!({
                "target_artifact_id": target.id,
                "target_kind": target.kind,
                "intent": intent
            })),
            options,
        );
        to_vm(&artifact)
    });

    vm.register_builtin("run_record", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record: missing payload".to_string()))?,
        )?;
        to_vm(&run)
    });

    vm.register_builtin("load_run_tree", |args, _out| {
        let path = require_string_arg(args, 0, "load_run_tree", "path")?;
        let tree = load_run_tree(&path)?;
        to_vm(&tree)
    });

    vm.register_builtin("run_record_save", |args, _out| {
        let mut run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_save: missing run".to_string()))?,
        )?;
        let path = args.get(1).map(|v| v.display()).filter(|s| !s.is_empty());
        let persisted = save_run_record(&run, path.as_deref())?;
        run.persisted_path = Some(persisted.clone());
        to_vm(&serde_json::json!({"path": persisted, "run": run}))
    });

    vm.register_builtin("run_record_load", |args, _out| {
        let path = args
            .first()
            .map(|v| v.display())
            .ok_or_else(|| VmError::Runtime("run_record_load: missing path".to_string()))?;
        to_vm(&load_run_record(std::path::Path::new(&path))?)
    });

    vm.register_builtin("run_record_fixture", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_fixture: missing run".to_string()))?,
        )?;
        to_vm(&replay_fixture_from_run(&run))
    });

    vm.register_builtin("run_record_eval", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record_eval: missing run".to_string()))?,
        )?;
        let fixture: ReplayFixture = match args.get(1) {
            Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
                .map_err(|e| VmError::Runtime(format!("run_record_eval: {e}")))?,
            None => replay_fixture_from_run(&run),
        };
        to_vm(&evaluate_run_against_fixture(&run, &fixture))
    });

    vm.register_builtin("run_record_eval_suite", |args, _out| {
        let items = match args.first() {
            Some(VmValue::List(list)) => list.clone(),
            _ => {
                return Err(VmError::Runtime(
                    "run_record_eval_suite: missing list".to_string(),
                ));
            }
        };
        let mut cases = Vec::new();
        for item in items.iter() {
            let source_path = item
                .as_dict()
                .and_then(|dict| dict.get("path"))
                .map(|value| value.display())
                .filter(|value| !value.is_empty());
            let run = if let Some(dict) = item.as_dict() {
                if let Some(run_value) = dict.get("run") {
                    normalize_run_record(run_value)?
                } else {
                    normalize_run_record(item)?
                }
            } else {
                normalize_run_record(item)?
            };
            let fixture: ReplayFixture = match item.as_dict().and_then(|dict| dict.get("fixture")) {
                Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
                    .map_err(|e| VmError::Runtime(format!("run_record_eval_suite: {e}")))?,
                None => replay_fixture_from_run(&run),
            };
            cases.push((run, fixture, source_path));
        }
        to_vm(&evaluate_run_suite(cases))
    });

    vm.register_builtin("run_record_diff", |args, _out| {
        let left =
            normalize_run_record(args.first().ok_or_else(|| {
                VmError::Runtime("run_record_diff: missing left run".to_string())
            })?)?;
        let right =
            normalize_run_record(args.get(1).ok_or_else(|| {
                VmError::Runtime("run_record_diff: missing right run".to_string())
            })?)?;
        to_vm(&diff_run_records(&left, &right))
    });

    vm.register_builtin("eval_suite_manifest", |args, _out| {
        let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
            VmError::Runtime("eval_suite_manifest: missing manifest payload".to_string())
        })?)?;
        to_vm(&manifest)
    });

    vm.register_builtin("eval_suite_run", |args, _out| {
        let manifest = normalize_eval_suite_manifest(args.first().ok_or_else(|| {
            VmError::Runtime("eval_suite_run: missing manifest payload".to_string())
        })?)?;
        to_vm(&evaluate_run_suite_manifest(&manifest)?)
    });

    vm.register_async_builtin("workflow_execute", |args| async move {
        let task = args.first().map(|v| v.display()).unwrap_or_default();
        let graph =
            normalize_workflow_value(args.get(1).ok_or_else(|| {
                VmError::Runtime("workflow_execute: missing workflow".to_string())
            })?)?;
        let artifacts = parse_artifact_list(args.get(2))?;
        let options = args
            .get(3)
            .and_then(|v| v.as_dict())
            .cloned()
            .unwrap_or_default();
        execute_workflow(task, graph, artifacts, options).await
    });

    // ── Tool lifecycle hooks ──────────────────────────────────────────

    type PostHookFn = Rc<dyn Fn(&str, &str) -> crate::orchestration::PostToolAction>;

    vm.register_builtin("register_tool_hook", |args, _out| {
        let config = args
            .first()
            .and_then(|a| a.as_dict())
            .cloned()
            .unwrap_or_default();
        let pattern = config
            .get("pattern")
            .map(|v| v.display())
            .unwrap_or_else(|| "*".to_string());
        let deny_reason = config.get("deny").map(|v| v.display());
        let max_output = config.get("max_output").and_then(|v| match v {
            VmValue::Int(n) => Some(*n as usize),
            _ => None,
        });

        let pre: Option<crate::orchestration::PreToolHookFn> = deny_reason.map(|reason| {
            Rc::new(move |_name: &str, _args: &serde_json::Value| {
                crate::orchestration::PreToolAction::Deny(reason.clone())
            }) as _
        });

        let post: Option<PostHookFn> = max_output.map(|max| {
            Rc::new(move |_name: &str, result: &str| {
                if result.len() > max {
                    crate::orchestration::PostToolAction::Modify(
                        crate::orchestration::microcompact_tool_output(result, max),
                    )
                } else {
                    crate::orchestration::PostToolAction::Pass
                }
            }) as _
        });

        crate::orchestration::register_tool_hook(crate::orchestration::ToolHook {
            pattern,
            pre,
            post,
        });
        Ok(VmValue::Nil)
    });

    vm.register_builtin("clear_tool_hooks", |_args, _out| {
        crate::orchestration::clear_tool_hooks();
        Ok(VmValue::Nil)
    });

    // ── Context assembly ──────────────────────────────────────────────

    vm.register_builtin("select_artifacts_adaptive", |args, _out| {
        let artifacts_val = args.first().cloned().unwrap_or(VmValue::Nil);
        let policy_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let artifacts: Vec<ArtifactRecord> = parse_artifact_list(Some(&artifacts_val))?;
        let policy: ContextPolicy = parse_context_policy(Some(&policy_val))?;
        let selected = crate::orchestration::select_artifacts_adaptive(artifacts, &policy);
        to_vm(&selected)
    });

    // ── Auto-compaction builtins ──────────────────────────────────────

    vm.register_builtin("estimate_tokens", |args, _out| {
        let messages: Vec<serde_json::Value> = args
            .first()
            .and_then(|a| match a {
                VmValue::List(list) => Some(
                    list.iter()
                        .map(crate::llm::helpers::vm_value_to_json)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let tokens = crate::orchestration::estimate_message_tokens(&messages);
        Ok(VmValue::Int(tokens as i64))
    });

    vm.register_builtin("microcompact", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        let max_chars = args
            .get(1)
            .and_then(|v| match v {
                VmValue::Int(n) => Some(*n as usize),
                _ => None,
            })
            .unwrap_or(20_000);
        Ok(VmValue::String(Rc::from(
            crate::orchestration::microcompact_tool_output(&text, max_chars),
        )))
    });

    vm.register_async_builtin("transcript_auto_compact", |args| async move {
        let mut messages: Vec<serde_json::Value> = match args.first() {
            Some(VmValue::List(list)) => list
                .iter()
                .map(crate::llm::helpers::vm_value_to_json)
                .collect(),
            _ => {
                return Err(VmError::Runtime(
                    "transcript_auto_compact: first argument must be a message list".to_string(),
                ))
            }
        };
        let options = args.get(1).and_then(|v| v.as_dict()).cloned();
        let mut config = crate::orchestration::AutoCompactConfig::default();
        if let Some(v) = options
            .as_ref()
            .and_then(|o| o.get("compact_threshold"))
            .and_then(|v| v.as_int())
        {
            config.token_threshold = v.max(0) as usize;
        }
        if let Some(v) = options
            .as_ref()
            .and_then(|o| o.get("tool_output_max_chars"))
            .and_then(|v| v.as_int())
        {
            config.tool_output_max_chars = v.max(0) as usize;
        }
        if let Some(v) = options
            .as_ref()
            .and_then(|o| o.get("keep_last"))
            .and_then(|v| v.as_int())
        {
            config.keep_last = v.max(0) as usize;
        }
        if let Some(strategy) = options
            .as_ref()
            .and_then(|o| o.get("compact_strategy"))
            .map(|v| v.display())
        {
            config.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
        }
        if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
            config.custom_compactor = Some(callback.clone());
            if !options
                .as_ref()
                .is_some_and(|o| o.contains_key("compact_strategy"))
            {
                config.compact_strategy = crate::orchestration::CompactStrategy::Custom;
            }
        }
        let llm_opts = if config.compact_strategy == crate::orchestration::CompactStrategy::Llm {
            Some(crate::llm::extract_llm_options(&[
                VmValue::String(Rc::from("")),
                VmValue::Nil,
                args.get(1).cloned().unwrap_or(VmValue::Nil),
            ])?)
        } else {
            None
        };
        crate::orchestration::auto_compact_messages(&mut messages, &config, llm_opts.as_ref())
            .await?;
        Ok(VmValue::List(Rc::new(
            messages
                .iter()
                .map(crate::stdlib::json_to_vm_value)
                .collect(),
        )))
    });
}

fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("agents encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

fn normalize_policy(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|e| VmError::Runtime(format!("policy parse error: {e}")))
}

fn parse_context_policy(value: Option<&VmValue>) -> Result<ContextPolicy, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("context policy parse error: {e}"))),
        None => Ok(ContextPolicy::default()),
    }
}

fn parse_artifact_list(value: Option<&VmValue>) -> Result<Vec<ArtifactRecord>, VmError> {
    match value {
        Some(VmValue::List(list)) => list.iter().map(normalize_artifact).collect(),
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(_) => Err(VmError::Runtime(
            "expected artifact list or nil".to_string(),
        )),
    }
}

#[derive(Default)]
struct ArtifactHelperOptions {
    id: Option<String>,
    title: Option<String>,
    text: Option<String>,
    source: Option<String>,
    stage: Option<String>,
    freshness: Option<String>,
    priority: Option<i64>,
    relevance: Option<f64>,
    estimated_tokens: Option<usize>,
    lineage: Vec<String>,
    metadata: BTreeMap<String, serde_json::Value>,
    data: Option<serde_json::Value>,
}

fn parse_artifact_helper_options(
    value: Option<&VmValue>,
) -> Result<ArtifactHelperOptions, VmError> {
    let Some(value) = value else {
        return Ok(ArtifactHelperOptions::default());
    };
    match value {
        VmValue::Nil => Ok(ArtifactHelperOptions::default()),
        VmValue::Dict(_) => {
            let json = crate::llm::vm_value_to_json(value);
            let mut options = ArtifactHelperOptions::default();
            let Some(map) = json.as_object() else {
                return Ok(options);
            };
            options.id = map
                .get("id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.title = map
                .get("title")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.text = map
                .get("text")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.source = map
                .get("source")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.stage = map
                .get("stage")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.freshness = map
                .get("freshness")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            options.priority = map.get("priority").and_then(|value| value.as_i64());
            options.relevance = map.get("relevance").and_then(|value| value.as_f64());
            options.estimated_tokens = map
                .get("estimated_tokens")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize);
            options.lineage = map
                .get("lineage")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(|item| item.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            options.metadata = map
                .get("metadata")
                .and_then(|value| value.as_object())
                .map(|meta| {
                    meta.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            options.data = map.get("data").cloned();
            Ok(options)
        }
        _ => Err(VmError::Runtime(
            "artifact helper options must be a dict or nil".to_string(),
        )),
    }
}

fn require_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn require_text_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    field: &str,
) -> Result<String, VmError> {
    args.get(index)
        .map(value_to_text)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing {field}")))
}

fn optional_text_arg(value: Option<&VmValue>) -> Option<String> {
    value
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(value_to_text)
        .filter(|value| !value.is_empty())
}

fn value_to_text(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.to_string(),
        _ => {
            let json = crate::llm::vm_value_to_json(value);
            if let Some(text) = json.as_str() {
                text.to_string()
            } else {
                json.to_string()
            }
        }
    }
}

fn merge_json_value(
    base: Option<serde_json::Value>,
    overlay: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, overlay) {
        (Some(serde_json::Value::Object(mut base)), Some(serde_json::Value::Object(overlay))) => {
            for (key, value) in overlay {
                base.insert(key, value);
            }
            Some(serde_json::Value::Object(base))
        }
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (Some(_), Some(overlay)) => Some(overlay),
        (None, None) => None,
    }
}

fn build_helper_artifact(
    kind: &str,
    title: Option<String>,
    text: Option<String>,
    data: Option<serde_json::Value>,
    options: ArtifactHelperOptions,
) -> ArtifactRecord {
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: options.id.unwrap_or_default(),
        kind: kind.to_string(),
        title: options.title.or(title),
        text: options.text.or(text),
        data: merge_json_value(data, options.data),
        source: options.source,
        created_at: String::new(),
        freshness: options.freshness,
        priority: options.priority,
        lineage: options.lineage,
        relevance: options.relevance,
        estimated_tokens: options.estimated_tokens,
        stage: options.stage,
        metadata: options.metadata,
    }
    .normalize()
}

fn set_node_policy(
    args: &[VmValue],
    updater: impl Fn(&mut crate::orchestration::WorkflowNode, serde_json::Value) -> Result<(), VmError>,
) -> Result<VmValue, VmError> {
    let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
        VmError::Runtime("workflow policy update: missing workflow".to_string())
    })?)?;
    let node_id = args
        .get(1)
        .map(|v| v.display())
        .ok_or_else(|| VmError::Runtime("workflow policy update: missing node id".to_string()))?;
    let policy =
        crate::llm::vm_value_to_json(args.get(2).ok_or_else(|| {
            VmError::Runtime("workflow policy update: missing policy".to_string())
        })?);
    let node = graph
        .nodes
        .get_mut(&node_id)
        .ok_or_else(|| VmError::Runtime(format!("unknown workflow node: {node_id}")))?;
    updater(node, policy)?;
    append_audit_entry(
        &mut graph,
        "set_policy",
        Some(node_id),
        None,
        BTreeMap::new(),
    );
    to_vm(&graph)
}

fn apply_runtime_node_overrides(
    mut node: crate::orchestration::WorkflowNode,
    options: &BTreeMap<String, VmValue>,
) -> crate::orchestration::WorkflowNode {
    if node.model_policy.provider.is_none() {
        node.model_policy.provider = options
            .get("provider")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.model.is_none() {
        node.model_policy.model = options
            .get("model")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.model_tier.is_none() {
        node.model_policy.model_tier = options
            .get("model_tier")
            .or_else(|| options.get("tier"))
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if node.model_policy.temperature.is_none() {
        node.model_policy.temperature = options.get("temperature").and_then(|value| match value {
            VmValue::Float(number) => Some(*number),
            _ => value.as_int().map(|number| number as f64),
        });
    }
    if node.model_policy.max_tokens.is_none() {
        node.model_policy.max_tokens = options.get("max_tokens").and_then(|value| value.as_int());
    }
    if node.mode.is_none() {
        node.mode = options
            .get("mode")
            .map(|value| value.display())
            .filter(|value| !value.is_empty());
    }
    if !node.capability_policy.tools.is_empty() {
        node.tools
            .retain(|tool| node.capability_policy.tools.contains(tool));
    }
    node
}

#[derive(Debug)]
struct ExecutedStage {
    status: String,
    outcome: String,
    branch: Option<String>,
    result: serde_json::Value,
    artifacts: Vec<ArtifactRecord>,
    transcript: Option<VmValue>,
    verification: Option<serde_json::Value>,
    error: Option<String>,
    attempts: Vec<RunStageAttemptRecord>,
    consumed_artifact_ids: Vec<String>,
}

fn replay_stage(
    current: &str,
    replay_stages: &mut VecDeque<RunStageRecord>,
) -> Result<ExecutedStage, VmError> {
    let Some(stage) = replay_stages.pop_front() else {
        return Err(VmError::Runtime(format!(
            "workflow replay exhausted before node {current}"
        )));
    };
    if stage.node_id != current {
        return Err(VmError::Runtime(format!(
            "workflow replay mismatch: expected node {current}, next replay stage is {}",
            stage.node_id
        )));
    }
    Ok(ExecutedStage {
        status: stage.status.clone(),
        outcome: stage.outcome.clone(),
        branch: stage.branch.clone(),
        result: serde_json::json!({
            "status": stage.status,
            "visible_text": stage.visible_text,
            "private_reasoning": stage.private_reasoning,
        }),
        artifacts: stage.artifacts.clone(),
        transcript: stage
            .transcript
            .as_ref()
            .map(crate::stdlib::json_to_vm_value),
        verification: stage.verification.clone(),
        error: stage
            .metadata
            .get("error")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        attempts: stage.attempts.clone(),
        consumed_artifact_ids: stage.consumed_artifact_ids.clone(),
    })
}

fn evaluate_verification(
    node: &crate::orchestration::WorkflowNode,
    result: &serde_json::Value,
) -> serde_json::Value {
    let visible_text = result
        .get("visible_text")
        .or_else(|| result.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    node.verify
        .as_ref()
        .and_then(|verify| verify.as_object())
        .and_then(|verify| {
            verify
                .get("assert_text")
                .and_then(|value| value.as_str())
                .map(|needle| {
                    serde_json::json!({
                        "kind": "assert_text",
                        "ok": visible_text.contains(needle),
                        "expected": needle,
                    })
                })
        })
        .unwrap_or_else(|| serde_json::json!({"kind": "none", "ok": true}))
}

fn evaluate_condition(
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> (bool, Vec<String>) {
    let consumed = artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect();
    if let Some(needle) = node
        .metadata
        .get("contains_text")
        .and_then(|value| value.as_str())
    {
        let matched = artifacts.iter().any(|artifact| {
            artifact
                .text
                .as_ref()
                .is_some_and(|text| text.contains(needle))
        });
        return (matched, consumed);
    }
    if let Some(expected) = node
        .metadata
        .get("artifact_kind")
        .and_then(|value| value.as_str())
    {
        let matched = artifacts.iter().any(|artifact| artifact.kind == expected);
        return (matched, consumed);
    }
    (!artifacts.is_empty(), consumed)
}

fn artifact_from_value(
    node_id: &str,
    kind: &str,
    index: usize,
    value: serde_json::Value,
    lineage: &[String],
    title: String,
) -> ArtifactRecord {
    ArtifactRecord {
        type_name: "artifact".to_string(),
        id: format!("{node_id}_artifact_{}", uuid::Uuid::now_v7()),
        kind: kind.to_string(),
        title: Some(title),
        text: value.as_str().map(|text| text.to_string()),
        data: Some(value),
        source: Some(node_id.to_string()),
        created_at: uuid::Uuid::now_v7().to_string(),
        freshness: Some("fresh".to_string()),
        priority: None,
        lineage: lineage.to_vec(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata: BTreeMap::from([("index".to_string(), serde_json::json!(index))]),
    }
    .normalize()
}

fn checkpoint_run(
    run: &mut RunRecord,
    ready_nodes: &VecDeque<String>,
    completed_nodes: &BTreeSet<String>,
    last_stage_id: Option<String>,
    reason: &str,
    persist_path: &str,
) -> Result<(), VmError> {
    run.pending_nodes = ready_nodes.iter().cloned().collect();
    run.completed_nodes = completed_nodes.iter().cloned().collect();
    run.checkpoints.push(RunCheckpointRecord {
        id: uuid::Uuid::now_v7().to_string(),
        ready_nodes: run.pending_nodes.clone(),
        completed_nodes: run.completed_nodes.clone(),
        last_stage_id,
        persisted_at: uuid::Uuid::now_v7().to_string(),
        reason: reason.to_string(),
    });
    let persisted_path = save_run_record(run, Some(persist_path))?;
    run.persisted_path = Some(persisted_path);
    Ok(())
}

fn parse_execution_record(value: Option<&VmValue>) -> Result<Option<RunExecutionRecord>, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map(Some)
            .map_err(|e| VmError::Runtime(format!("workflow execution parse error: {e}"))),
        None => Ok(None),
    }
}

fn load_run_tree(path: &str) -> Result<serde_json::Value, VmError> {
    let run = load_run_record(std::path::Path::new(path))?;
    let mut children = Vec::new();
    for child in &run.child_runs {
        if let Some(run_path) = child.run_path.as_deref() {
            if std::path::Path::new(run_path).exists() {
                children.push(load_run_tree(run_path)?);
                continue;
            }
        }
        children.push(serde_json::json!({
            "worker": child,
            "run": serde_json::Value::Null,
            "children": [],
        }));
    }
    Ok(serde_json::json!({
        "run": run,
        "children": children,
    }))
}

fn append_child_run_record(run: &mut RunRecord, stage_id: &str, stage: &serde_json::Value) {
    let Some(worker) = stage.get("worker") else {
        return;
    };
    let worker_id = worker
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if worker_id.is_empty() {
        return;
    }
    let child = RunChildRecord {
        worker_id: worker_id.to_string(),
        worker_name: worker
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("worker")
            .to_string(),
        parent_stage_id: Some(stage_id.to_string()),
        task: worker
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        status: worker
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("completed")
            .to_string(),
        started_at: worker
            .get("started_at")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        finished_at: worker
            .get("finished_at")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        run_id: worker
            .get("child_run_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        run_path: worker
            .get("child_run_path")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        snapshot_path: worker
            .get("snapshot_path")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        execution: worker
            .get("execution")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    };
    run.child_runs
        .retain(|existing| existing.worker_id != child.worker_id);
    run.child_runs.push(child);
}

fn enqueue_unique(queue: &mut VecDeque<String>, node_id: String) {
    if !queue.iter().any(|queued| queued == &node_id) {
        queue.push_back(node_id);
    }
}

fn effective_node_policy(
    graph: &WorkflowGraph,
    node: &crate::orchestration::WorkflowNode,
) -> Result<CapabilityPolicy, VmError> {
    let builtin = builtin_ceiling();
    let graph_policy = builtin
        .intersect(&graph.capability_policy)
        .map_err(VmError::Runtime)?;
    graph_policy
        .intersect(&node.capability_policy)
        .map_err(VmError::Runtime)
}

async fn execute_stage_attempts(
    task: &str,
    node_id: &str,
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<ExecutedStage, VmError> {
    type StageAttemptResult = (
        serde_json::Value,
        Vec<ArtifactRecord>,
        Option<VmValue>,
        String,
        Option<String>,
        Option<serde_json::Value>,
    );
    let consumed_artifact_ids = select_artifacts(artifacts.to_vec(), &node.context_policy)
        .into_iter()
        .map(|artifact| artifact.id)
        .collect::<Vec<_>>();
    let mut attempts = Vec::new();
    let max_attempts = node.retry_policy.max_attempts.max(1);
    let mut last_error = None;

    for attempt in 1..=max_attempts {
        let started_at = uuid::Uuid::now_v7().to_string();
        let attempt_task = if attempt == 1 {
            task.to_string()
        } else {
            format!(
                "{task}\n\nRetry attempt {attempt} of {max_attempts}. Repair the previous failure and produce a corrected result."
            )
        };
        let execution: Result<StageAttemptResult, VmError> = match node.kind.as_str() {
            "fork" => Ok((
                serde_json::json!({"status": "completed", "text": "forked"}),
                Vec::new(),
                transcript.clone(),
                "forked".to_string(),
                Some("fork".to_string()),
                None,
            )),
            "join" => Ok((
                serde_json::json!({"status": "completed", "text": "joined"}),
                Vec::new(),
                transcript.clone(),
                "joined".to_string(),
                Some("join".to_string()),
                None,
            )),
            "condition" => {
                let selected = select_artifacts(artifacts.to_vec(), &node.context_policy);
                let (matched, _) = evaluate_condition(node, &selected);
                Ok((
                    serde_json::json!({"status": "completed", "text": if matched { "true" } else { "false" }}),
                    Vec::new(),
                    transcript.clone(),
                    if matched {
                        "condition_true".to_string()
                    } else {
                        "condition_false".to_string()
                    },
                    Some(if matched {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }),
                    None,
                ))
            }
            "map" => {
                let mut inputs = select_artifacts(artifacts.to_vec(), &node.context_policy);
                if let Some(kind) = &node.map_policy.item_artifact_kind {
                    inputs.retain(|artifact| &artifact.kind == kind);
                }
                let mut items = node.map_policy.items.clone();
                if items.is_empty() {
                    items = inputs
                        .iter()
                        .map(|artifact| {
                            artifact
                                .data
                                .clone()
                                .or_else(|| artifact.text.clone().map(serde_json::Value::String))
                                .unwrap_or(serde_json::Value::Null)
                        })
                        .collect();
                }
                if let Some(max_items) = node.map_policy.max_items {
                    items.truncate(max_items);
                }
                let lineage = inputs
                    .iter()
                    .map(|artifact| artifact.id.clone())
                    .collect::<Vec<_>>();
                let output_kind = node
                    .map_policy
                    .output_kind
                    .clone()
                    .or_else(|| node.output_contract.output_kinds.first().cloned())
                    .unwrap_or_else(|| "artifact".to_string());
                let produced = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        artifact_from_value(
                            node_id,
                            &output_kind,
                            index,
                            item,
                            &lineage,
                            format!("map {} item {}", node_id, index + 1),
                        )
                    })
                    .collect::<Vec<_>>();
                Ok((
                    serde_json::json!({"status": "completed", "text": format!("mapped {} items", produced.len())}),
                    produced,
                    transcript.clone(),
                    "mapped".to_string(),
                    Some("mapped".to_string()),
                    None,
                ))
            }
            "reduce" => {
                let selected = select_artifacts(artifacts.to_vec(), &node.context_policy);
                let separator = node
                    .reduce_policy
                    .separator
                    .clone()
                    .unwrap_or_else(|| "\n\n".to_string());
                let reduced_text = selected
                    .iter()
                    .filter_map(|artifact| artifact.text.clone())
                    .collect::<Vec<_>>()
                    .join(&separator);
                let reduced = artifact_from_value(
                    node_id,
                    node.output_contract
                        .output_kinds
                        .first()
                        .map(|kind| kind.as_str())
                        .unwrap_or("summary"),
                    0,
                    serde_json::Value::String(reduced_text.clone()),
                    &selected
                        .iter()
                        .map(|artifact| artifact.id.clone())
                        .collect::<Vec<_>>(),
                    format!("reduce {} output", node_id),
                );
                Ok((
                    serde_json::json!({"status": "completed", "text": reduced_text}),
                    vec![reduced],
                    transcript.clone(),
                    "reduced".to_string(),
                    Some("reduced".to_string()),
                    None,
                ))
            }
            "escalation" => {
                let reason = node
                    .escalation_policy
                    .reason
                    .clone()
                    .unwrap_or_else(|| "manual review required".to_string());
                let produced = artifact_from_value(
                    node_id,
                    node.output_contract
                        .output_kinds
                        .first()
                        .map(|kind| kind.as_str())
                        .unwrap_or("plan"),
                    0,
                    serde_json::json!({
                        "queue": node.escalation_policy.queue,
                        "level": node.escalation_policy.level,
                        "reason": reason,
                    }),
                    &consumed_artifact_ids,
                    format!("escalation {}", node_id),
                );
                Ok((
                    serde_json::json!({"status": "completed", "text": reason}),
                    vec![produced],
                    transcript.clone(),
                    "escalated".to_string(),
                    Some("escalated".to_string()),
                    None,
                ))
            }
            "subagent" => {
                let (result, produced, next_transcript) = execute_delegated_stage(
                    node_id,
                    node,
                    &attempt_task,
                    artifacts,
                    transcript.clone(),
                )
                .await?;
                Ok((
                    result,
                    produced,
                    next_transcript,
                    "subagent_completed".to_string(),
                    Some("success".to_string()),
                    None,
                ))
            }
            _ => {
                let (result, produced, next_transcript) = crate::orchestration::execute_stage_node(
                    node_id,
                    node,
                    &attempt_task,
                    artifacts,
                    transcript.clone(),
                )
                .await?;
                let verification = evaluate_verification(node, &result);
                let verified_ok = verification
                    .get("ok")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                let outcome = if node.kind == "verify" {
                    if verified_ok {
                        "verified".to_string()
                    } else {
                        "verification_failed".to_string()
                    }
                } else if node.kind == "subagent" {
                    "subagent_completed".to_string()
                } else {
                    "success".to_string()
                };
                let branch = if node.kind == "verify" {
                    Some(if verified_ok {
                        "passed".to_string()
                    } else {
                        "failed".to_string()
                    })
                } else {
                    Some("success".to_string())
                };
                Ok((
                    result,
                    produced,
                    next_transcript,
                    outcome,
                    branch,
                    Some(verification),
                ))
            }
        };

        match execution {
            Ok((result, produced, next_transcript, outcome, branch, verification)) => {
                let success = !matches!(branch.as_deref(), Some("failed"));
                attempts.push(RunStageAttemptRecord {
                    attempt,
                    status: if success {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    },
                    outcome: outcome.clone(),
                    branch: branch.clone(),
                    error: None,
                    verification: verification.clone(),
                    started_at,
                    finished_at: Some(uuid::Uuid::now_v7().to_string()),
                });
                if success {
                    return Ok(ExecutedStage {
                        status: "completed".to_string(),
                        outcome,
                        branch,
                        result,
                        artifacts: produced,
                        transcript: next_transcript,
                        verification,
                        error: None,
                        attempts,
                        consumed_artifact_ids,
                    });
                }
                last_error = Some("verification failed".to_string());
            }
            Err(error) => {
                let error_message = error.to_string();
                attempts.push(RunStageAttemptRecord {
                    attempt,
                    status: "failed".to_string(),
                    outcome: "error".to_string(),
                    branch: Some("error".to_string()),
                    error: Some(error_message.clone()),
                    verification: None,
                    started_at,
                    finished_at: Some(uuid::Uuid::now_v7().to_string()),
                });
                last_error = Some(error_message);
            }
        }
    }

    Ok(ExecutedStage {
        status: "failed".to_string(),
        outcome: "failed".to_string(),
        branch: Some("failed".to_string()),
        result: serde_json::json!({"status": "failed", "text": ""}),
        artifacts: Vec::new(),
        transcript,
        verification: None,
        error: last_error,
        attempts,
        consumed_artifact_ids,
    })
}

async fn execute_workflow(
    task: String,
    graph: WorkflowGraph,
    mut artifacts: Vec<ArtifactRecord>,
    options: BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let report = validate_workflow(&graph, Some(&builtin_ceiling()));
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "workflow_execute: invalid workflow: {}",
            report.errors.join("; ")
        )));
    }

    let resumed_run = match options.get("resume_path").map(|v| v.display()) {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("resume_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_source = match options.get("replay_path").map(|v| v.display()) {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("replay_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_mode = options
        .get("replay_mode")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());

    let persist_path = options
        .get("persist_path")
        .map(|value| value.display())
        .filter(|path| !path.is_empty())
        .or_else(|| {
            options
                .get("resume_path")
                .map(|value| value.display())
                .filter(|path| !path.is_empty())
        })
        .unwrap_or_else(|| format!(".harn-runs/{}.json", uuid::Uuid::now_v7()));
    let execution = parse_execution_record(options.get("execution"))?;
    let parent_run_id = options
        .get("parent_run_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let root_run_id = options
        .get("root_run_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .or_else(|| parent_run_id.clone());

    let mut run = resumed_run.unwrap_or_else(|| RunRecord {
        type_name: "run_record".to_string(),
        id: uuid::Uuid::now_v7().to_string(),
        workflow_id: graph.id.clone(),
        workflow_name: graph.name.clone(),
        task: task.clone(),
        status: "running".to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        parent_run_id: parent_run_id.clone(),
        root_run_id: root_run_id.clone(),
        stages: Vec::new(),
        transitions: Vec::new(),
        checkpoints: Vec::new(),
        pending_nodes: vec![graph.entry.clone()],
        completed_nodes: Vec::new(),
        child_runs: Vec::new(),
        artifacts: artifacts.clone(),
        policy: builtin_ceiling(),
        execution: execution.clone(),
        transcript: None,
        replay_fixture: None,
        metadata: BTreeMap::new(),
        persisted_path: None,
    });
    run.workflow_id = graph.id.clone();
    run.workflow_name = graph.name.clone();
    run.task = task.clone();
    run.status = "running".to_string();
    run.parent_run_id = parent_run_id.clone().or(run.parent_run_id.clone());
    if run.root_run_id.is_none() {
        run.root_run_id = root_run_id.clone().or(Some(run.id.clone()));
    }
    if run.execution.is_none() {
        run.execution = execution.clone();
    }
    run.metadata.insert(
        "effective_policy".to_string(),
        serde_json::to_value(&run.policy).unwrap_or_default(),
    );
    if let Some(parent_worker_id) = options
        .get("parent_worker_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
    {
        run.metadata.insert(
            "parent_worker_id".to_string(),
            serde_json::json!(parent_worker_id),
        );
    }
    if let Some(parent_stage_id) = options
        .get("parent_stage_id")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
    {
        run.metadata.insert(
            "parent_stage_id".to_string(),
            serde_json::json!(parent_stage_id),
        );
    }
    if matches!(options.get("delegated"), Some(VmValue::Bool(true))) {
        run.metadata
            .insert("delegated".to_string(), serde_json::json!(true));
    }
    if let Some(parent_run_id) = &run.parent_run_id {
        run.metadata.insert(
            "parent_run_id".to_string(),
            serde_json::json!(parent_run_id),
        );
    }
    if let Some(root_run_id) = &run.root_run_id {
        run.metadata
            .insert("root_run_id".to_string(), serde_json::json!(root_run_id));
    }
    if let Some(execution) = &run.execution {
        run.metadata.insert(
            "execution".to_string(),
            serde_json::to_value(execution).unwrap_or_default(),
        );
    }
    if !graph.metadata.is_empty() {
        run.metadata.insert(
            "workflow_metadata".to_string(),
            serde_json::to_value(&graph.metadata).unwrap_or_default(),
        );
    }

    let mut transcript = run
        .transcript
        .clone()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    if !run.artifacts.is_empty() {
        artifacts = run.artifacts.clone();
    }
    let mut ready_nodes: VecDeque<String> = if run.pending_nodes.is_empty() {
        VecDeque::from(vec![graph.entry.clone()])
    } else {
        VecDeque::from(run.pending_nodes.clone())
    };
    let mut completed_nodes: BTreeSet<String> = run.completed_nodes.iter().cloned().collect();
    let mut steps = 0usize;
    let max_steps = options
        .get("max_steps")
        .and_then(|v| v.as_int())
        .unwrap_or((graph.nodes.len() * 4) as i64)
        .max(1) as usize;
    run.metadata.insert(
        "workflow_version".to_string(),
        serde_json::json!(graph.version),
    );
    run.metadata.insert(
        "validation".to_string(),
        serde_json::to_value(&report).unwrap_or_default(),
    );
    run.metadata
        .insert("max_steps".to_string(), serde_json::json!(max_steps));
    run.metadata.insert(
        "resumed".to_string(),
        serde_json::json!(!run.stages.is_empty()),
    );
    if let Some(replay_mode) = &replay_mode {
        run.metadata
            .insert("replay_mode".to_string(), serde_json::json!(replay_mode));
    }
    if let Some(replay_source) = &replay_source {
        run.metadata.insert(
            "replayed_from".to_string(),
            serde_json::json!(replay_source.id.clone()),
        );
    }
    let mut replay_stages = replay_source
        .as_ref()
        .map(|source| VecDeque::from(source.stages.clone()));
    checkpoint_run(
        &mut run,
        &ready_nodes,
        &completed_nodes,
        None,
        "start",
        &persist_path,
    )?;

    while steps < max_steps && !ready_nodes.is_empty() {
        steps += 1;
        let current = ready_nodes.pop_front().unwrap_or_default();
        let node =
            graph.nodes.get(&current).cloned().ok_or_else(|| {
                VmError::Runtime(format!("workflow_execute: missing node {current}"))
            })?;
        if node.kind == "join" {
            let incoming = graph
                .edges
                .iter()
                .filter(|edge| edge.to == current)
                .map(|edge| edge.from.clone())
                .collect::<Vec<_>>();
            let required = node.join_policy.min_completed.unwrap_or(
                if node.join_policy.require_all_inputs || node.join_policy.strategy == "all" {
                    incoming.len()
                } else {
                    1
                },
            );
            let completed_inputs = incoming
                .iter()
                .filter(|input| completed_nodes.contains(*input))
                .count();
            if completed_inputs < required {
                enqueue_unique(&mut ready_nodes, current.clone());
                continue;
            }
        }
        let node = apply_runtime_node_overrides(node, &options);
        let stage_policy = effective_node_policy(&graph, &node)?;

        let stage_id = format!("{}:{}:{}", run.id, current, run.stages.len() + 1);
        let started_at = uuid::Uuid::now_v7().to_string();
        push_execution_policy(stage_policy.clone());
        let executed_result = if replay_mode.as_deref() == Some("deterministic") {
            match replay_stages.as_mut() {
                Some(stages) => replay_stage(&current, stages),
                None => Err(VmError::Runtime(
                    "replay_mode requires replay_run or replay_path".to_string(),
                )),
            }
        } else {
            execute_stage_attempts(&task, &current, &node, &artifacts, transcript.clone()).await
        };
        pop_execution_policy();
        let executed = match executed_result {
            Ok(executed) => executed,
            Err(error) => return Err(error),
        };

        transcript = executed.transcript.clone();
        artifacts.extend(executed.artifacts.clone());
        run.artifacts = artifacts.clone();
        run.transcript = transcript
            .clone()
            .map(|value| crate::llm::vm_value_to_json(&value));

        let mut stage_metadata = BTreeMap::new();
        stage_metadata.insert(
            "model_policy".to_string(),
            serde_json::to_value(&node.model_policy).unwrap_or_default(),
        );
        stage_metadata.insert(
            "transcript_policy".to_string(),
            serde_json::to_value(&node.transcript_policy).unwrap_or_default(),
        );
        stage_metadata.insert(
            "context_policy".to_string(),
            serde_json::to_value(&node.context_policy).unwrap_or_default(),
        );
        stage_metadata.insert(
            "retry_policy".to_string(),
            serde_json::to_value(&node.retry_policy).unwrap_or_default(),
        );
        stage_metadata.insert(
            "effective_capability_policy".to_string(),
            serde_json::to_value(&stage_policy).unwrap_or_default(),
        );
        stage_metadata.insert(
            "input_contract".to_string(),
            serde_json::to_value(&node.input_contract).unwrap_or_default(),
        );
        stage_metadata.insert(
            "output_contract".to_string(),
            serde_json::to_value(&node.output_contract).unwrap_or_default(),
        );
        if let Some(worker) = executed.result.get("worker") {
            stage_metadata.insert("worker".to_string(), worker.clone());
            if let Some(worker_id) = worker.get("id") {
                stage_metadata.insert("worker_id".to_string(), worker_id.clone());
            }
        }
        if let Some(error) = executed.error.clone() {
            stage_metadata.insert("error".to_string(), serde_json::json!(error));
        }

        let produced_artifact_ids = executed
            .artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>();
        run.stages.push(RunStageRecord {
            id: stage_id.clone(),
            node_id: current.clone(),
            kind: node.kind.clone(),
            status: executed.status.clone(),
            outcome: executed.outcome.clone(),
            branch: executed.branch.clone(),
            started_at,
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
            visible_text: executed
                .result
                .get("visible_text")
                .or_else(|| executed.result.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            private_reasoning: executed
                .result
                .get("private_reasoning")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            transcript: executed
                .transcript
                .as_ref()
                .map(crate::llm::vm_value_to_json),
            verification: executed.verification.clone(),
            artifacts: executed.artifacts.clone(),
            consumed_artifact_ids: executed.consumed_artifact_ids.clone(),
            produced_artifact_ids: produced_artifact_ids.clone(),
            attempts: executed.attempts,
            metadata: stage_metadata,
        });
        append_child_run_record(&mut run, &stage_id, &executed.result);
        completed_nodes.insert(current.clone());

        let next_edges = next_nodes_for(&graph, &current, executed.branch.as_deref());
        for edge in next_edges {
            enqueue_unique(&mut ready_nodes, edge.to.clone());
            run.transitions.push(RunTransitionRecord {
                id: uuid::Uuid::now_v7().to_string(),
                from_stage_id: Some(stage_id.clone()),
                from_node_id: Some(current.clone()),
                to_node_id: edge.to,
                branch: edge.branch.clone(),
                timestamp: uuid::Uuid::now_v7().to_string(),
                consumed_artifact_ids: executed.consumed_artifact_ids.clone(),
                produced_artifact_ids: produced_artifact_ids.clone(),
            });
        }
        checkpoint_run(
            &mut run,
            &ready_nodes,
            &completed_nodes,
            Some(stage_id),
            "stage_complete",
            &persist_path,
        )?;
    }

    run.status = if ready_nodes.is_empty() {
        "completed".to_string()
    } else {
        "paused".to_string()
    };
    run.finished_at = Some(uuid::Uuid::now_v7().to_string());
    run.replay_fixture = Some(replay_fixture_from_run(&run));
    checkpoint_run(
        &mut run,
        &ready_nodes,
        &completed_nodes,
        None,
        "finalize",
        &persist_path,
    )?;

    to_vm(&serde_json::json!({
        "status": run.status,
        "run": run,
        "artifacts": artifacts,
        "transcript": transcript.map(|value| crate::llm::vm_value_to_json(&value)),
        "path": persist_path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_run_tree_recurses_into_child_runs() {
        let dir = std::env::temp_dir().join(format!("harn-run-tree-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let child_path = dir.join("child.json");
        let parent_path = dir.join("parent.json");

        let child = RunRecord {
            id: "child".to_string(),
            workflow_id: "wf".to_string(),
            root_run_id: Some("root".to_string()),
            status: "completed".to_string(),
            ..Default::default()
        };
        let parent = RunRecord {
            id: "parent".to_string(),
            workflow_id: "wf".to_string(),
            root_run_id: Some("root".to_string()),
            status: "completed".to_string(),
            child_runs: vec![RunChildRecord {
                worker_id: "worker_1".to_string(),
                worker_name: "worker".to_string(),
                run_id: Some("child".to_string()),
                run_path: Some(child_path.to_string_lossy().to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
        save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

        let tree = load_run_tree(parent_path.to_str().unwrap()).unwrap();
        assert_eq!(tree["run"]["id"], "parent");
        assert_eq!(tree["children"][0]["run"]["id"], "child");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
