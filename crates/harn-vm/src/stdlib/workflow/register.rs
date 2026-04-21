//! Top-level workflow executor and builtin registration.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, install_current_mutation_session,
    install_workflow_skill_context, load_run_record, next_nodes_for, normalize_run_record,
    normalize_workflow_value, pop_execution_policy, push_execution_policy, validate_workflow,
    workflow_verification_contracts, ArtifactRecord, MutationSessionRecord, RunRecord,
    RunStageRecord, RunTransitionRecord, WorkflowEdge, WorkflowGraph, WorkflowSkillContext,
    WorkflowSkillContextGuard,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::super::{parse_artifact_list, parse_context_policy};

use super::artifact::{
    append_child_run_record, checkpoint_run, optional_string_option, parse_execution_record,
    snapshot_trace_spans,
};
use super::convert::{to_vm, workflow_graph_to_vm};
use super::guards::{MutationSessionResetGuard, WorkflowApprovalPolicyGuard};
use super::policy::{
    apply_runtime_node_overrides, effective_node_approval_policy, effective_node_policy,
    normalize_policy, set_node_policy,
};
use super::stage::{execute_stage_attempts, replay_stage};
use super::usage::{llm_usage_delta, llm_usage_snapshot};

fn parse_trigger_event_option(
    value: Option<&VmValue>,
) -> Result<Option<crate::TriggerEvent>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map(Some)
        .map_err(|error| {
            VmError::Runtime(format!(
                "workflow_execute: trigger_event parse error: {error}"
            ))
        })
}

/// Accept a skill registry dict on `workflow_execute(task, graph,
/// artifacts, {skills: ...})`. Mirrors the agent-loop normalizer: a
/// raw registry dict passes through, a list of skill entries is
/// wrapped into a synthetic registry. Other shapes are dropped —
/// failing silently rather than erroring keeps backwards compatibility
/// for callers that already pass unvalidated option dicts through.
fn validate_workflow_skill_registry(value: VmValue) -> Option<VmValue> {
    match &value {
        VmValue::Dict(d)
            if d.get("_type")
                .map(|v| v.display() == "skill_registry")
                .unwrap_or(false) =>
        {
            Some(value)
        }
        VmValue::List(list) => {
            let mut dict = BTreeMap::new();
            dict.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("skill_registry")),
            );
            dict.insert("skills".to_string(), VmValue::List(list.clone()));
            Some(VmValue::Dict(Rc::new(dict)))
        }
        _ => None,
    }
}

pub(in crate::stdlib) async fn execute_workflow(
    task: String,
    graph: WorkflowGraph,
    mut artifacts: Vec<ArtifactRecord>,
    options: BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    crate::llm::enable_tracing();
    crate::tracing::set_tracing_enabled(true);
    let workflow_span_id = crate::tracing::span_start(
        crate::tracing::SpanKind::Pipeline,
        graph
            .name
            .clone()
            .unwrap_or_else(|| graph.id.clone())
            .to_string(),
    );
    let run_usage_before = llm_usage_snapshot();
    let report = validate_workflow(&graph, Some(&builtin_ceiling()));
    if !report.valid {
        return Err(VmError::Runtime(format!(
            "workflow_execute: invalid workflow: {}",
            report.errors.join("; ")
        )));
    }
    let workflow_verification_contracts = workflow_verification_contracts(&graph)?;

    let resumed_run = match optional_string_option(&options, "resume_path") {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("resume_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_source = match optional_string_option(&options, "replay_path") {
        Some(path) if !path.is_empty() => Some(load_run_record(std::path::Path::new(&path))?),
        _ => match options.get("replay_run") {
            Some(value) => Some(normalize_run_record(value)?),
            None => None,
        },
    };
    let replay_mode = options.get("replay_mode").and_then(|value| match value {
        VmValue::Nil => None,
        _ => {
            let rendered = value.display();
            if rendered.is_empty() || rendered == "nil" {
                None
            } else {
                Some(rendered)
            }
        }
    });

    let persist_path = optional_string_option(&options, "persist_path")
        .or_else(|| optional_string_option(&options, "resume_path"))
        .unwrap_or_else(|| {
            match std::env::var(crate::runtime_paths::HARN_RUN_DIR_ENV) {
                Ok(value) if !value.trim().is_empty() => crate::orchestration::default_run_dir(),
                _ => std::path::PathBuf::from(".harn-runs"),
            }
            .join(format!("{}.json", uuid::Uuid::now_v7()))
            .display()
            .to_string()
        });
    let execution = parse_execution_record(options.get("execution"))?;
    let parent_run_id = optional_string_option(&options, "parent_run_id");
    let root_run_id =
        optional_string_option(&options, "root_run_id").or_else(|| parent_run_id.clone());

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
        usage: None,
        replay_fixture: None,
        observability: None,
        trace_spans: Vec::new(),
        tool_recordings: Vec::new(),
        metadata: BTreeMap::new(),
        persisted_path: None,
    });
    let requested_mutation_scope = optional_string_option(&options, "mutation_scope")
        .unwrap_or_else(|| {
            execution
                .as_ref()
                .and_then(|record| record.adapter.clone())
                .map(|adapter| {
                    if adapter == "worktree" {
                        "apply_worktree".to_string()
                    } else {
                        "read_only".to_string()
                    }
                })
                .unwrap_or_else(|| "read_only".to_string())
        });
    let mutation_approval_policy = options.get("approval_policy").and_then(|value| {
        serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(
            crate::llm::vm_value_to_json(value),
        )
        .ok()
    });
    let audit_input = options
        .get("audit")
        .cloned()
        .unwrap_or_else(|| VmValue::Dict(Rc::new(BTreeMap::new())));
    let mut mutation_session: MutationSessionRecord =
        serde_json::from_value(crate::llm::vm_value_to_json(&audit_input))
            .map_err(|e| VmError::Runtime(format!("workflow_execute: audit parse error: {e}")))?;
    mutation_session.run_id = Some(run.id.clone());
    mutation_session.execution_kind = Some("workflow".to_string());
    if mutation_session.mutation_scope.is_empty() {
        mutation_session.mutation_scope = requested_mutation_scope;
    }
    if mutation_session.approval_policy.is_none() {
        mutation_session.approval_policy = mutation_approval_policy;
    }
    mutation_session = mutation_session.normalize();
    if run.transcript.is_none() {
        if let Some(seed_transcript) = options.get("transcript") {
            run.transcript = Some(crate::llm::vm_value_to_json(seed_transcript));
        }
    }
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
    run.metadata.insert(
        "mutation_session".to_string(),
        serde_json::to_value(&mutation_session).unwrap_or_default(),
    );
    if !graph.metadata.is_empty() {
        run.metadata.insert(
            "workflow_metadata".to_string(),
            serde_json::to_value(&graph.metadata).unwrap_or_default(),
        );
    }
    let dispatch_context = crate::triggers::dispatcher::current_dispatch_context();
    let trigger_event = parse_trigger_event_option(options.get("trigger_event"))?.or_else(|| {
        dispatch_context
            .as_ref()
            .map(|context| context.trigger_event.clone())
    });
    if let Some(trigger_event) = trigger_event {
        run.metadata.insert(
            "trigger_event".to_string(),
            serde_json::to_value(&trigger_event).unwrap_or_default(),
        );
        run.metadata.insert(
            "trace_id".to_string(),
            serde_json::json!(trigger_event.trace_id.0),
        );
    }
    if let Some(replay_of_event_id) = dispatch_context
        .as_ref()
        .and_then(|context| context.replay_of_event_id.as_ref())
    {
        run.metadata.insert(
            "replay_of_event_id".to_string(),
            serde_json::json!(replay_of_event_id),
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
    install_current_mutation_session(Some(mutation_session.clone()));
    let _mutation_session_guard = MutationSessionResetGuard;

    // Install workflow-level skill wiring so each per-stage agent loop
    // (execute / verify / plan / subagent — every kind that constructs
    // an `AgentLoopConfig` through `execute_stage_node`) picks up the
    // same registry without a new parameter on every signature. Before
    // this, only direct `agent_loop(...)` callers received `skills:` —
    // the workflow path silently dropped it.
    let workflow_skill_registry = options
        .get("skills")
        .cloned()
        .and_then(validate_workflow_skill_registry);
    let workflow_skill_match = options.get("skill_match").cloned();
    if workflow_skill_registry.is_some() || workflow_skill_match.is_some() {
        install_workflow_skill_context(Some(WorkflowSkillContext {
            registry: workflow_skill_registry,
            match_config: workflow_skill_match,
        }));
    }
    let _workflow_skill_guard = WorkflowSkillContextGuard;

    let workflow_approval_guard = match mutation_session.approval_policy.clone() {
        Some(policy) => {
            crate::orchestration::push_approval_policy(policy);
            WorkflowApprovalPolicyGuard(true)
        }
        None => WorkflowApprovalPolicyGuard(false),
    };
    let _workflow_approval_guard = workflow_approval_guard;
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
                super::artifact::enqueue_unique(&mut ready_nodes, current.clone());
                continue;
            }
        }
        let mut node = apply_runtime_node_overrides(node, &options);
        crate::orchestration::inject_workflow_verification_contracts(
            &mut node,
            &workflow_verification_contracts,
        );
        let stage_policy = effective_node_policy(&graph, &node)?;
        let stage_approval = effective_node_approval_policy(&graph, &node);

        let stage_id = format!("{}:{}:{}", run.id, current, run.stages.len() + 1);
        let started_at = uuid::Uuid::now_v7().to_string();
        push_execution_policy(stage_policy.clone());
        crate::orchestration::push_approval_policy(stage_approval.clone());
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
        crate::orchestration::pop_approval_policy();
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
            "auto_compact".to_string(),
            serde_json::to_value(&node.auto_compact).unwrap_or_default(),
        );
        if let Some(ref visibility) = node.output_visibility {
            stage_metadata.insert(
                "output_visibility".to_string(),
                serde_json::Value::String(visibility.clone()),
            );
        }
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
        if let Some(prompt) = executed.result.get("prompt") {
            stage_metadata.insert("prompt".to_string(), prompt.clone());
        }
        if let Some(system_prompt) = executed.result.get("system_prompt") {
            stage_metadata.insert("system_prompt".to_string(), system_prompt.clone());
        }
        if let Some(rendered_context) = executed.result.get("rendered_context") {
            stage_metadata.insert("rendered_context".to_string(), rendered_context.clone());
        }
        if let Some(verification_contracts) = executed.result.get("verification_contracts") {
            stage_metadata.insert(
                "verification_contracts".to_string(),
                verification_contracts.clone(),
            );
        }
        if let Some(rendered_verification_context) =
            executed.result.get("rendered_verification_context")
        {
            stage_metadata.insert(
                "rendered_verification_context".to_string(),
                rendered_verification_context.clone(),
            );
        }
        if let Some(selected_artifact_ids) = executed.result.get("selected_artifact_ids") {
            stage_metadata.insert(
                "selected_artifact_ids".to_string(),
                selected_artifact_ids.clone(),
            );
        }
        if let Some(selected_artifact_titles) = executed.result.get("selected_artifact_titles") {
            stage_metadata.insert(
                "selected_artifact_titles".to_string(),
                selected_artifact_titles.clone(),
            );
        }
        if let Some(tool_calling_mode) = executed.result.get("tool_calling_mode") {
            stage_metadata.insert("tool_calling_mode".to_string(), tool_calling_mode.clone());
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
            usage: Some(executed.usage.clone()),
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
            super::artifact::enqueue_unique(&mut ready_nodes, edge.to.clone());
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
    run.usage = Some(llm_usage_delta(&run_usage_before, &llm_usage_snapshot()));
    run.replay_fixture = Some(crate::orchestration::replay_fixture_from_run(&run));
    crate::tracing::span_end(workflow_span_id);
    run.trace_spans = snapshot_trace_spans();
    run.tool_recordings = crate::llm::mock::drain_tool_recordings();
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

pub(crate) fn register_workflow_builtins(vm: &mut Vm) {
    vm.register_builtin("workflow_graph", |args, _out| {
        let input = args
            .first()
            .cloned()
            .unwrap_or(VmValue::Dict(Rc::new(BTreeMap::new())));
        let graph = normalize_workflow_value(&input)?;
        workflow_graph_to_vm(&graph)
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
        workflow_graph_to_vm(&graph)
    });

    vm.register_builtin("workflow_insert_node", |args, _out| {
        let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_insert_node: missing workflow".to_string())
        })?)?;
        let node_value = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("workflow_insert_node: missing node".to_string()))?;
        let mut node =
            crate::orchestration::parse_workflow_node_value(node_value, "workflow_insert_node")?;
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
            let edge = crate::orchestration::parse_workflow_edge_json(
                edge_json,
                "workflow_insert_node edge",
            )?;
            graph.edges.push(edge);
        }
        append_audit_entry(
            &mut graph,
            "insert_node",
            Some(node_id),
            None,
            BTreeMap::new(),
        );
        workflow_graph_to_vm(&graph)
    });

    vm.register_builtin("workflow_replace_node", |args, _out| {
        let mut graph = normalize_workflow_value(args.first().ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing workflow".to_string())
        })?)?;
        let node_id = args.get(1).map(|v| v.display()).ok_or_else(|| {
            VmError::Runtime("workflow_replace_node: missing node id".to_string())
        })?;
        let mut node = crate::orchestration::parse_workflow_node_value(
            args.get(2).ok_or_else(|| {
                VmError::Runtime("workflow_replace_node: missing node".to_string())
            })?,
            "workflow_replace_node",
        )?;
        node.id = Some(node_id.clone());
        graph.nodes.insert(node_id.clone(), node);
        append_audit_entry(
            &mut graph,
            "replace_node",
            Some(node_id),
            None,
            BTreeMap::new(),
        );
        workflow_graph_to_vm(&graph)
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
        workflow_graph_to_vm(&graph)
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

    vm.register_builtin("workflow_set_auto_compact", |args, _out| {
        set_node_policy(args, |node, policy| {
            node.auto_compact = serde_json::from_value(policy)
                .map_err(|e| VmError::Runtime(format!("workflow_set_auto_compact: {e}")))?;
            Ok(())
        })
    });

    vm.register_builtin("workflow_set_output_visibility", |args, _out| {
        set_node_policy(args, |node, policy| {
            node.output_visibility = match policy {
                serde_json::Value::Null => None,
                serde_json::Value::String(s) => Some(s),
                _ => {
                    return Err(VmError::Runtime(
                        "workflow_set_output_visibility: value must be a string or nil".into(),
                    ))
                }
            };
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
        workflow_graph_to_vm(&graph)
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

    vm.register_builtin("select_artifacts_adaptive", |args, _out| {
        let artifacts_val = args.first().cloned().unwrap_or(VmValue::Nil);
        let policy_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let artifacts: Vec<ArtifactRecord> = parse_artifact_list(Some(&artifacts_val))?;
        let policy: crate::orchestration::ContextPolicy = parse_context_policy(Some(&policy_val))?;
        let selected = crate::orchestration::select_artifacts_adaptive(artifacts, &policy);
        to_vm(&selected)
    });

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
        let _ =
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
