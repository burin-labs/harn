//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::orchestration::{
    append_audit_entry, builtin_ceiling, load_run_record, next_node_for, normalize_artifact,
    normalize_run_record, normalize_workflow_value, render_artifacts_context, save_run_record,
    select_artifacts, validate_workflow, ArtifactRecord, CapabilityPolicy, ContextPolicy,
    RunRecord, RunStageRecord, WorkflowEdge, WorkflowGraph,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

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
        let report = validate_workflow(&graph, Some(&builtin_ceiling()));
        to_vm(&serde_json::json!({
            "graph": graph,
            "validation": report,
            "node_count": graph.nodes.len(),
            "edge_count": graph.edges.len(),
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

    vm.register_builtin("run_record", |args, _out| {
        let run = normalize_run_record(
            args.first()
                .ok_or_else(|| VmError::Runtime("run_record: missing payload".to_string()))?,
        )?;
        to_vm(&run)
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
    node
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

    let mut run = resumed_run.unwrap_or_else(|| RunRecord {
        type_name: "run_record".to_string(),
        id: uuid::Uuid::now_v7().to_string(),
        workflow_id: graph.id.clone(),
        workflow_name: graph.name.clone(),
        task: task.clone(),
        status: "running".to_string(),
        started_at: uuid::Uuid::now_v7().to_string(),
        finished_at: None,
        stages: Vec::new(),
        artifacts: artifacts.clone(),
        policy: builtin_ceiling(),
        transcript: None,
        metadata: BTreeMap::new(),
        persisted_path: None,
    });
    run.workflow_id = graph.id.clone();
    run.workflow_name = graph.name.clone();
    run.task = task.clone();
    run.status = "running".to_string();

    let mut current = run
        .stages
        .last()
        .and_then(|stage| next_node_for(&graph, &stage.node_id, &stage.status))
        .unwrap_or_else(|| graph.entry.clone());
    let mut transcript = run
        .transcript
        .clone()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    if !run.artifacts.is_empty() {
        artifacts = run.artifacts.clone();
    }
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

    while steps < max_steps {
        steps += 1;
        let node =
            graph.nodes.get(&current).cloned().ok_or_else(|| {
                VmError::Runtime(format!("workflow_execute: missing node {current}"))
            })?;
        let node = apply_runtime_node_overrides(node, &options);

        let stage_id = format!("{}:{}", run.id, current);
        let started_at = uuid::Uuid::now_v7().to_string();
        let mut attempt = 0usize;
        let max_attempts = node.retry_policy.max_attempts.max(1);
        let mut result = serde_json::json!({"status": "failed", "text": ""});
        let mut stage_artifacts = Vec::new();
        let mut next_transcript = transcript.clone();
        let mut verification_outcome = None;
        let mut stage_error = None;

        while attempt < max_attempts {
            attempt += 1;
            let attempt_task = if attempt == 1 {
                task.clone()
            } else {
                format!(
                    "{task}\n\nRetry attempt {attempt} of {max_attempts}. Repair the previous failure and produce a corrected result."
                )
            };
            let stage_attempt = match node.kind.as_str() {
                "join" => Ok((
                    serde_json::json!({"status": "joined", "text": ""}),
                    Vec::new(),
                    transcript.clone(),
                )),
                "condition" => Ok((
                    serde_json::json!({"status": "branch", "text": ""}),
                    Vec::new(),
                    transcript.clone(),
                )),
                "map" => {
                    let items = node
                        .metadata
                        .get("items")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let mut mapped = Vec::new();
                    let mut mapped_transcript = transcript.clone();
                    let item_count = items.len();
                    for item in items {
                        let subtask = format!("{attempt_task}\n\nMap item:\n{}", item);
                        let (_, produced, latest_transcript) =
                            crate::orchestration::execute_stage_node(
                                &current,
                                &node,
                                &subtask,
                                &artifacts,
                                mapped_transcript.clone(),
                            )
                            .await?;
                        mapped.extend(produced);
                        mapped_transcript = latest_transcript;
                    }
                    Ok((
                        serde_json::json!({"status": "mapped", "text": format!("mapped {} items", item_count)}),
                        mapped,
                        mapped_transcript,
                    ))
                }
                _ => {
                    crate::orchestration::execute_stage_node(
                        &current,
                        &node,
                        &attempt_task,
                        &artifacts,
                        transcript.clone(),
                    )
                    .await
                }
            };

            match stage_attempt {
                Ok((candidate_result, candidate_artifacts, candidate_transcript)) => {
                    let visible_text = candidate_result
                        .get("visible_text")
                        .or_else(|| candidate_result.get("text"))
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let verified = node
                        .verify
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
                        .unwrap_or_else(|| serde_json::json!({"kind": "none", "ok": true}));
                    let verified_ok = verified
                        .get("ok")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true);
                    result = candidate_result;
                    stage_artifacts = candidate_artifacts;
                    next_transcript = candidate_transcript;
                    verification_outcome = Some(verified.clone());
                    if verified_ok {
                        stage_error = None;
                        break;
                    }
                    stage_error = Some(format!("verification failed: {}", verified));
                    if attempt >= max_attempts {
                        result["status"] = serde_json::json!("failed");
                    }
                }
                Err(error) => {
                    stage_error = Some(error.to_string());
                    if attempt >= max_attempts {
                        return Err(error);
                    }
                }
            }
        }

        transcript = next_transcript.clone();
        artifacts.extend(stage_artifacts.clone());
        run.artifacts = artifacts.clone();
        run.transcript = transcript
            .clone()
            .map(|value| crate::llm::vm_value_to_json(&value));
        let mut stage_metadata = BTreeMap::new();
        stage_metadata.insert("attempts".to_string(), serde_json::json!(attempt));
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
            "selected_lineage".to_string(),
            serde_json::json!(stage_artifacts
                .iter()
                .flat_map(|artifact| artifact.lineage.iter().cloned())
                .collect::<Vec<_>>()),
        );
        if let Some(error) = stage_error {
            stage_metadata.insert("error".to_string(), serde_json::json!(error));
        }
        run.stages.push(RunStageRecord {
            id: stage_id,
            node_id: current.clone(),
            kind: node.kind.clone(),
            status: result["status"].as_str().unwrap_or("done").to_string(),
            started_at,
            finished_at: Some(uuid::Uuid::now_v7().to_string()),
            visible_text: result
                .get("visible_text")
                .or_else(|| result.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            private_reasoning: result
                .get("private_reasoning")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            transcript: next_transcript.as_ref().map(crate::llm::vm_value_to_json),
            verification: verification_outcome,
            artifacts: stage_artifacts,
            metadata: stage_metadata,
        });

        let status = result["status"].as_str().unwrap_or("done");
        let next = next_node_for(&graph, &current, status);

        match next {
            Some(next) => current = next,
            None => break,
        }
    }

    run.status = "completed".to_string();
    run.finished_at = Some(uuid::Uuid::now_v7().to_string());
    let persisted_path = save_run_record(&run, run.persisted_path.as_deref())?;
    run.persisted_path = Some(persisted_path.clone());
    save_run_record(&run, Some(&persisted_path))?;

    to_vm(&serde_json::json!({
        "status": run.status,
        "run": run,
        "artifacts": artifacts,
        "transcript": transcript.map(|value| crate::llm::vm_value_to_json(&value)),
        "path": persisted_path,
    }))
}
