//! Workflow graph manipulation and execution builtins.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, install_current_mutation_session, load_run_record,
    next_nodes_for, normalize_run_record, normalize_workflow_value, pop_execution_policy,
    push_execution_policy, save_run_record, select_artifacts, validate_workflow,
    workflow_tool_policy_from_tools, ArtifactRecord, CapabilityPolicy, LlmUsageRecord,
    MutationSessionRecord, RunCheckpointRecord, RunChildRecord, RunExecutionRecord, RunRecord,
    RunStageAttemptRecord, RunStageRecord, RunTraceSpanRecord, RunTransitionRecord, WorkflowEdge,
    WorkflowGraph,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::{parse_artifact_list, parse_context_policy};

fn to_vm<T: serde::Serialize>(value: &T) -> Result<VmValue, VmError> {
    let json = serde_json::to_value(value)
        .map_err(|e| VmError::Runtime(format!("workflow encode error: {e}")))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

fn workflow_graph_to_vm(graph: &WorkflowGraph) -> Result<VmValue, VmError> {
    let base = to_vm(graph)?;
    let VmValue::Dict(base_dict) = base else {
        return Err(VmError::Runtime(
            "workflow graph encoding did not produce a dict".to_string(),
        ));
    };
    let mut graph_dict = (*base_dict).clone();
    let nodes_value = graph_dict
        .get("nodes")
        .cloned()
        .ok_or_else(|| VmError::Runtime("workflow graph is missing nodes".to_string()))?;
    let VmValue::Dict(nodes_dict) = nodes_value else {
        return Err(VmError::Runtime(
            "workflow graph nodes encoding did not produce a dict".to_string(),
        ));
    };
    let mut nodes = (*nodes_dict).clone();
    for (node_id, node) in &graph.nodes {
        let Some(raw_tools) = node.raw_tools.clone() else {
            continue;
        };
        let Some(node_value) = nodes.get(node_id).cloned() else {
            continue;
        };
        let VmValue::Dict(node_dict) = node_value else {
            continue;
        };
        let mut node_map = (*node_dict).clone();
        node_map.insert("tools".to_string(), raw_tools);
        nodes.insert(node_id.clone(), VmValue::Dict(Rc::new(node_map)));
    }
    graph_dict.insert("nodes".to_string(), VmValue::Dict(Rc::new(nodes)));
    Ok(VmValue::Dict(Rc::new(graph_dict)))
}

fn normalize_policy(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    serde_json::from_value(crate::llm::vm_value_to_json(value))
        .map_err(|e| VmError::Runtime(format!("policy parse error: {e}")))
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
    workflow_graph_to_vm(&graph)
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
        node.tools = filter_workflow_tools(&node.tools, &node.capability_policy.tools);
        node.raw_tools = node
            .raw_tools
            .as_ref()
            .map(|tools| filter_workflow_tools_vm(tools, &node.capability_policy.tools));
    }
    node
}

fn filter_workflow_tools(tools: &serde_json::Value, allowed: &[String]) -> serde_json::Value {
    match tools {
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .filter(|item| match item {
                    serde_json::Value::Object(map) => map
                        .get("name")
                        .and_then(|value| value.as_str())
                        .map(|name| allowed.iter().any(|allowed_name| allowed_name == name))
                        .unwrap_or(false),
                    _ => false,
                })
                .cloned()
                .collect(),
        ),
        serde_json::Value::Object(map)
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") =>
        {
            let mut filtered = map.clone();
            let tool_items = map
                .get("tools")
                .map(|value| filter_workflow_tools(value, allowed))
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
            filtered.insert("tools".to_string(), tool_items);
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Object(map) => {
            let keep = map
                .get("name")
                .and_then(|value| value.as_str())
                .map(|name| allowed.iter().any(|allowed_name| allowed_name == name))
                .unwrap_or(false);
            if keep {
                tools.clone()
            } else {
                serde_json::Value::Null
            }
        }
        _ => serde_json::Value::Null,
    }
}

fn filter_workflow_tools_vm(tools: &VmValue, allowed: &[String]) -> VmValue {
    match tools {
        VmValue::Nil => VmValue::Nil,
        VmValue::List(items) => VmValue::List(Rc::new(
            items
                .iter()
                .filter(|item| {
                    item.as_dict()
                        .and_then(|map| map.get("name"))
                        .map(|name| name.display())
                        .map(|name| allowed.iter().any(|allowed_name| allowed_name == &name))
                        .unwrap_or(false)
                })
                .cloned()
                .collect(),
        )),
        VmValue::Dict(map)
            if map.get("_type").map(|value| value.display()).as_deref()
                == Some("tool_registry") =>
        {
            let mut filtered = (**map).clone();
            let tool_items = map
                .get("tools")
                .map(|value| filter_workflow_tools_vm(value, allowed))
                .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new())));
            filtered.insert("tools".to_string(), tool_items);
            VmValue::Dict(Rc::new(filtered))
        }
        VmValue::Dict(map) => {
            let keep = map
                .get("name")
                .map(|value| value.display())
                .map(|name| allowed.iter().any(|allowed_name| allowed_name == &name))
                .unwrap_or(false);
            if keep {
                tools.clone()
            } else {
                VmValue::Nil
            }
        }
        _ => VmValue::Nil,
    }
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
    usage: LlmUsageRecord,
    error: Option<String>,
    attempts: Vec<RunStageAttemptRecord>,
    consumed_artifact_ids: Vec<String>,
}

#[derive(Clone, Debug)]
struct UsageSnapshot {
    input_tokens: i64,
    output_tokens: i64,
    total_duration_ms: i64,
    call_count: i64,
    total_cost: f64,
    trace_len: usize,
}

fn llm_usage_snapshot() -> UsageSnapshot {
    let (input_tokens, output_tokens, total_duration_ms, call_count) =
        crate::llm::peek_trace_summary();
    let total_cost = crate::llm::cost::peek_total_cost();
    let trace_len = crate::llm::peek_trace().len();
    UsageSnapshot {
        input_tokens,
        output_tokens,
        total_duration_ms,
        call_count,
        total_cost,
        trace_len,
    }
}

fn llm_usage_delta(before: &UsageSnapshot, after: &UsageSnapshot) -> LlmUsageRecord {
    let trace = crate::llm::peek_trace();
    let start = before.trace_len.min(trace.len());
    let models = trace[start..]
        .iter()
        .map(|entry| entry.model.clone())
        .filter(|model| !model.is_empty())
        .fold(Vec::<String>::new(), |mut acc, model| {
            if !acc.iter().any(|existing| existing == &model) {
                acc.push(model);
            }
            acc
        });

    LlmUsageRecord {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        total_duration_ms: after
            .total_duration_ms
            .saturating_sub(before.total_duration_ms),
        call_count: after.call_count.saturating_sub(before.call_count),
        total_cost: (after.total_cost - before.total_cost).max(0.0),
        models,
    }
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
        usage: stage.usage.clone().unwrap_or_default(),
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
    let exit_status = result
        .get("exit_status")
        .or_else(|| result.get("status_code"))
        .or_else(|| result.get("status"))
        .and_then(|value| value.as_i64());
    let Some(verify) = node.verify.as_ref().and_then(|verify| verify.as_object()) else {
        return serde_json::json!({"kind": "none", "ok": true});
    };

    let mut checks = Vec::new();
    if let Some(needle) = verify.get("assert_text").and_then(|value| value.as_str()) {
        checks.push(serde_json::json!({
            "kind": "assert_text",
            "ok": visible_text.contains(needle),
            "expected": needle,
        }));
    }
    if let Some(expected_status) = verify.get("expect_status").and_then(|value| value.as_i64()) {
        checks.push(serde_json::json!({
            "kind": "expect_status",
            "ok": exit_status == Some(expected_status),
            "expected": expected_status,
            "actual": exit_status,
        }));
    }
    if checks.is_empty() {
        return serde_json::json!({"kind": "none", "ok": true});
    }

    let ok = checks.iter().all(|check| {
        check
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    });
    serde_json::json!({
        "kind": "composite",
        "ok": ok,
        "checks": checks,
    })
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
    run.trace_spans = snapshot_trace_spans();
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

fn snapshot_trace_spans() -> Vec<RunTraceSpanRecord> {
    crate::tracing::peek_spans()
        .into_iter()
        .map(|span| RunTraceSpanRecord {
            span_id: span.span_id,
            parent_id: span.parent_id,
            kind: span.kind.as_str().to_string(),
            name: span.name,
            start_ms: span.start_ms,
            duration_ms: span.duration_ms,
            metadata: span.metadata,
        })
        .collect()
}

fn parse_execution_record(value: Option<&VmValue>) -> Result<Option<RunExecutionRecord>, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map(Some)
            .map_err(|e| VmError::Runtime(format!("workflow execution parse error: {e}"))),
        None => Ok(None),
    }
}

fn optional_string_option(options: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    options.get(key).and_then(|value| match value {
        VmValue::Nil => None,
        _ => {
            let rendered = value.display();
            if rendered.is_empty() || rendered == "nil" {
                None
            } else {
                Some(rendered)
            }
        }
    })
}

pub(in crate::stdlib) fn load_run_tree(path: &str) -> Result<serde_json::Value, VmError> {
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
        session_id: worker
            .get("audit")
            .and_then(|value| value.get("session_id"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        parent_session_id: worker
            .get("audit")
            .and_then(|value| value.get("parent_session_id"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        mutation_scope: worker
            .get("audit")
            .and_then(|value| value.get("mutation_scope"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        approval_mode: worker
            .get("audit")
            .and_then(|value| value.get("approval_mode"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
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

fn classify_stage_outcome(
    node_kind: &str,
    result: &serde_json::Value,
    verification: &serde_json::Value,
) -> (String, Option<String>) {
    let verified_ok = verification
        .get("ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let result_status = result
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("completed");
    let stage_succeeded = if node_kind == "verify" {
        verified_ok
    } else {
        result_status == "done" || result_status == "completed"
    };

    let outcome = if node_kind == "verify" {
        if verified_ok {
            "verified".to_string()
        } else {
            "verification_failed".to_string()
        }
    } else if !stage_succeeded {
        result_status.to_string()
    } else if node_kind == "subagent" {
        "subagent_completed".to_string()
    } else {
        "success".to_string()
    };

    let branch = if node_kind == "verify" {
        Some(if verified_ok {
            "passed".to_string()
        } else {
            "failed".to_string()
        })
    } else if stage_succeeded {
        Some("success".to_string())
    } else {
        Some("failed".to_string())
    };

    (outcome, branch)
}

fn effective_node_policy(
    graph: &WorkflowGraph,
    node: &crate::orchestration::WorkflowNode,
) -> Result<CapabilityPolicy, VmError> {
    let builtin = builtin_ceiling();
    let graph_policy = builtin
        .intersect(&graph.capability_policy)
        .map_err(VmError::Runtime)?;
    let node_policy = graph_policy
        .intersect(&node.capability_policy)
        .map_err(VmError::Runtime)?;
    node_policy
        .intersect(&workflow_tool_policy_from_tools(&node.tools))
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
        let usage_before = llm_usage_snapshot();
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
                let (result, produced, next_transcript) =
                    super::agents_workers::execute_delegated_stage(
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
                let (outcome, branch) = classify_stage_outcome(&node.kind, &result, &verification);
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
                let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
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
                        usage,
                        error: None,
                        attempts,
                        consumed_artifact_ids,
                    });
                }
                last_error = Some("verification failed".to_string());
            }
            Err(error) => {
                let usage = llm_usage_delta(&usage_before, &llm_usage_snapshot());
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
                last_error = Some(error_message.clone());
                if attempt == max_attempts {
                    let last_verification = attempts.last().and_then(|a| a.verification.clone());
                    return Ok(ExecutedStage {
                        status: "failed".to_string(),
                        outcome: "failed".to_string(),
                        branch: Some("failed".to_string()),
                        result: serde_json::json!({"status": "failed", "text": ""}),
                        artifacts: Vec::new(),
                        transcript: transcript.clone(),
                        verification: last_verification,
                        usage,
                        error: Some(error_message),
                        attempts,
                        consumed_artifact_ids,
                    });
                }
            }
        }
    }

    // Carry the last attempt's verification into the stage result so
    // classify_stage_outcome sees the actual verification data instead
    // of defaulting to ok=true when verification is None.
    let last_verification = attempts.last().and_then(|a| a.verification.clone());
    Ok(ExecutedStage {
        status: "failed".to_string(),
        outcome: "failed".to_string(),
        branch: Some("failed".to_string()),
        result: serde_json::json!({"status": "failed", "text": ""}),
        artifacts: Vec::new(),
        transcript,
        verification: last_verification,
        usage: LlmUsageRecord::default(),
        error: last_error,
        attempts,
        consumed_artifact_ids,
    })
}

struct MutationSessionResetGuard;

impl Drop for MutationSessionResetGuard {
    fn drop(&mut self) {
        install_current_mutation_session(None);
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
        .unwrap_or_else(|| format!(".harn-runs/{}.json", uuid::Uuid::now_v7()));
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
    let mutation_approval_mode = optional_string_option(&options, "approval_mode")
        .unwrap_or_else(|| "host_enforced".to_string());
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
    if mutation_session.approval_mode.is_empty() {
        mutation_session.approval_mode = mutation_approval_mode;
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
        if let Some(prompt) = executed.result.get("prompt") {
            stage_metadata.insert("prompt".to_string(), prompt.clone());
        }
        if let Some(system_prompt) = executed.result.get("system_prompt") {
            stage_metadata.insert("system_prompt".to_string(), system_prompt.clone());
        }
        if let Some(rendered_context) = executed.result.get("rendered_context") {
            stage_metadata.insert("rendered_context".to_string(), rendered_context.clone());
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
        let policy: crate::orchestration::ContextPolicy = parse_context_policy(Some(&policy_val))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{save_run_record, RunChildRecord, RunRecord};
    use crate::tracing::{set_tracing_enabled, span_end, span_start, SpanKind};

    #[test]
    fn classify_stage_outcome_fails_when_agent_loop_is_stuck() {
        let (outcome, branch) = classify_stage_outcome(
            "stage",
            &serde_json::json!({"status": "stuck"}),
            &serde_json::json!({"ok": true}),
        );
        assert_eq!(outcome, "stuck");
        assert_eq!(branch.as_deref(), Some("failed"));
    }

    #[test]
    fn classify_stage_outcome_accepts_done_status_for_mutating_stage() {
        let (outcome, branch) = classify_stage_outcome(
            "stage",
            &serde_json::json!({"status": "done"}),
            &serde_json::json!({"ok": true}),
        );
        assert_eq!(outcome, "success");
        assert_eq!(branch.as_deref(), Some("success"));
    }

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

    #[test]
    fn snapshot_trace_spans_returns_completed_trace_tree() {
        set_tracing_enabled(true);
        let parent = span_start(SpanKind::Pipeline, "workflow".to_string());
        let child = span_start(SpanKind::ToolCall, "read".to_string());
        span_end(child);
        span_end(parent);

        let spans = snapshot_trace_spans();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].kind, "tool_call");
        assert_eq!(spans[0].parent_id, Some(parent));
        assert_eq!(spans[1].kind, "pipeline");

        set_tracing_enabled(false);
    }
}
