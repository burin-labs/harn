//! Map / parallel branch scheduling and per-branch work items.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use crate::orchestration::{select_artifacts, ArtifactRecord, LlmUsageRecord};

use super::artifact::artifact_from_value;

pub(super) type LocalTask<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

#[derive(Debug)]
pub(super) struct MapBranchResult {
    pub(super) index: usize,
    pub(super) status: String,
    pub(super) result: serde_json::Value,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) usage: LlmUsageRecord,
    pub(super) error: Option<String>,
}

#[derive(Clone)]
pub(super) enum MapWorkItem {
    Artifact {
        index: usize,
        artifact: Box<ArtifactRecord>,
    },
    Value {
        index: usize,
        value: serde_json::Value,
        artifact_kind: String,
    },
}

pub(super) fn map_completion_target(
    strategy: &str,
    total: usize,
    min_completed: Option<usize>,
) -> usize {
    match strategy {
        "first" => total.min(1),
        "quorum" => min_completed.unwrap_or(1).max(1).min(total),
        _ => total,
    }
}

pub(super) async fn execute_join_policy<T: 'static>(
    tasks: Vec<LocalTask<T>>,
    strategy: &str,
    min_completed: Option<usize>,
    max_concurrent: Option<usize>,
) -> Vec<Result<T, String>> {
    if tasks.is_empty() {
        return Vec::new();
    }

    let total = tasks.len();
    let target = map_completion_target(strategy, total, min_completed);
    let concurrency = max_concurrent.unwrap_or(total).max(1).min(total);
    let mut pending = VecDeque::from(tasks);
    let mut join_set = tokio::task::JoinSet::new();
    let mut results = Vec::new();

    while join_set.len() < concurrency {
        let Some(task) = pending.pop_front() else {
            break;
        };
        join_set.spawn_local(task);
    }

    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(result) => results.push(Ok(result)),
            Err(error) => results.push(Err(format!("workflow map branch failed: {error}"))),
        }
        if results.len() >= target {
            join_set.abort_all();
            while join_set.join_next().await.is_some() {}
            break;
        }
        while join_set.len() < concurrency {
            let Some(task) = pending.pop_front() else {
                break;
            };
            join_set.spawn_local(task);
        }
    }

    results
}

pub(super) fn map_branch_artifact(
    node_id: &str,
    item: &MapWorkItem,
    lineage: &[String],
) -> ArtifactRecord {
    match item {
        MapWorkItem::Artifact { artifact, .. } => *artifact.clone(),
        MapWorkItem::Value {
            index,
            value,
            artifact_kind,
        } => artifact_from_value(
            node_id,
            artifact_kind,
            *index,
            value.clone(),
            lineage,
            format!("map {node_id} item {}", index + 1),
        ),
    }
}

pub(super) fn map_executes_stage(node: &crate::orchestration::WorkflowNode) -> bool {
    node.mode.is_some()
        || node.prompt.is_some()
        || node.system.is_some()
        || !crate::orchestration::workflow_tool_names(&node.tools).is_empty()
        || node.model_policy != crate::orchestration::ModelPolicy::default()
}

pub(super) fn map_stage_node(
    node: &crate::orchestration::WorkflowNode,
) -> crate::orchestration::WorkflowNode {
    let mut stage_node = node.clone();
    stage_node.kind = "stage".to_string();
    stage_node.map_policy = Default::default();
    stage_node.join_policy = Default::default();
    if let Some(output_kind) = &node.map_policy.output_kind {
        stage_node.output_contract.output_kinds = vec![output_kind.clone()];
    }
    stage_node
}

pub(super) fn map_work_items(
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> Vec<MapWorkItem> {
    let mut inputs = select_artifacts(artifacts.to_vec(), &node.context_policy);
    if let Some(kind) = &node.map_policy.item_artifact_kind {
        inputs.retain(|artifact| &artifact.kind == kind);
    }
    let mut explicit_items = node.map_policy.items.clone();
    if let Some(max_items) = node.map_policy.max_items {
        explicit_items.truncate(max_items);
        inputs.truncate(max_items);
    }
    if !explicit_items.is_empty() {
        let item_kind = node
            .map_policy
            .item_artifact_kind
            .clone()
            .unwrap_or_else(|| "artifact".to_string());
        return explicit_items
            .into_iter()
            .enumerate()
            .map(|(index, value)| MapWorkItem::Value {
                index,
                value,
                artifact_kind: item_kind.clone(),
            })
            .collect();
    }
    inputs
        .into_iter()
        .enumerate()
        .map(|(index, artifact)| MapWorkItem::Artifact {
            index,
            artifact: Box::new(artifact),
        })
        .collect()
}
