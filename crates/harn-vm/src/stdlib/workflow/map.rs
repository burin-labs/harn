//! Map stage planning and per-branch work items.

use serde::{Deserialize, Serialize};

use crate::orchestration::{ArtifactRecord, LlmUsageRecord};
use crate::value::VmError;

use super::artifact::artifact_from_value;

#[derive(Debug, Serialize)]
pub(super) struct MapBranchResult {
    pub(super) index: usize,
    pub(super) status: String,
    pub(super) result: serde_json::Value,
    pub(super) artifacts: Vec<ArtifactRecord>,
    pub(super) usage: LlmUsageRecord,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct MapExecutionPlan {
    pub(super) items: Vec<MapWorkItem>,
    pub(super) total_items: usize,
    pub(super) strategy: String,
    pub(super) join_target: usize,
    pub(super) max_concurrent: Option<usize>,
    pub(super) stage_node: Option<crate::orchestration::WorkflowNode>,
    pub(super) output_kind: String,
    pub(super) lineage: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WorkflowMapFinalization {
    result: serde_json::Value,
    outcome: String,
    branch: Option<String>,
}

pub(super) async fn map_finalize(
    strategy: &str,
    total_items: usize,
    produced_count: usize,
    completed: Vec<serde_json::Value>,
    failures: Vec<serde_json::Value>,
) -> Result<(serde_json::Value, String, Option<String>), VmError> {
    let payload = serde_json::json!({
        "strategy": strategy,
        "total_items": total_items,
        "produced_count": produced_count,
        "completed": completed,
        "failures": failures,
    });
    let finalized: WorkflowMapFinalization = crate::stdlib::harn_entry::call_harn_export_typed(
        "std/workflow/map",
        "workflow_map_finalize",
        "workflow_map_finalize",
        payload,
    )
    .await?;
    Ok((finalized.result, finalized.outcome, finalized.branch))
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

pub(super) async fn map_execution_plan(
    node: &crate::orchestration::WorkflowNode,
    artifacts: &[ArtifactRecord],
) -> Result<MapExecutionPlan, VmError> {
    let payload = serde_json::json!({
        "node": node,
        "artifacts": artifacts,
    });
    let mut planned: MapExecutionPlan = crate::stdlib::harn_entry::call_harn_export_typed(
        "std/workflow/map",
        "workflow_map_execution_plan",
        "workflow_map_execution_plan",
        payload,
    )
    .await?;
    planned.items = planned
        .items
        .into_iter()
        .map(|item| match item {
            MapWorkItem::Artifact { index, artifact } => MapWorkItem::Artifact {
                index,
                artifact: Box::new(artifact.normalize()),
            },
            MapWorkItem::Value {
                index,
                value,
                artifact_kind,
            } => MapWorkItem::Value {
                index,
                value,
                artifact_kind,
            },
        })
        .collect();
    Ok(planned)
}
