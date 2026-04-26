//! Artifact construction, checkpointing, and run-tree traversal helpers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::orchestration::{
    load_run_record, run_child_record_from_worker_metadata, save_run_record, ArtifactRecord,
    RunCheckpointRecord, RunExecutionRecord, RunRecord, RunTraceSpanRecord,
};
use crate::value::{VmError, VmValue};

pub(super) fn artifact_from_value(
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

pub(super) fn checkpoint_run(
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

pub(super) fn snapshot_trace_spans() -> Vec<RunTraceSpanRecord> {
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

pub(super) fn parse_execution_record(
    value: Option<&VmValue>,
) -> Result<Option<RunExecutionRecord>, VmError> {
    match value {
        Some(value) => serde_json::from_value(crate::llm::vm_value_to_json(value))
            .map(Some)
            .map_err(|e| VmError::Runtime(format!("workflow execution parse error: {e}"))),
        None => Ok(None),
    }
}

pub(super) fn optional_string_option(
    options: &BTreeMap<String, VmValue>,
    key: &str,
) -> Option<String> {
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

pub(super) fn append_child_run_record(
    run: &mut RunRecord,
    stage_id: &str,
    stage: &serde_json::Value,
) {
    let Some(worker) = stage.get("worker") else {
        return;
    };
    let Some(child) = run_child_record_from_worker_metadata(Some(stage_id.to_string()), worker)
    else {
        return;
    };
    run.child_runs
        .retain(|existing| existing.worker_id != child.worker_id);
    run.child_runs.push(child);
}

pub(super) fn enqueue_unique(queue: &mut VecDeque<String>, node_id: String) {
    if !queue.iter().any(|queued| queued == &node_id) {
        queue.push_back(node_id);
    }
}
