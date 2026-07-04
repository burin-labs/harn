//! Workflow patch proposals: a bounded, auditable contract that lets an
//! agent author propose changes to a portable [`WorkflowBundle`] without
//! touching the live runtime.
//!
//! The shape is intentionally small. An agent emits a JSON document
//! describing a sequence of [`WorkflowPatchOperation`]s; Harn applies
//! them to a copy of the bundle, runs the existing bundle validator,
//! computes a capability-ceiling delta against the parent execution
//! policy (when one is supplied), and returns a single
//! [`WorkflowPatchValidationReport`] the host can render.
//!
//! The patch surface deliberately mirrors what the issue calls out:
//! insert agent / verifier / approval node, update prompt capsule,
//! update model & tool policy, add edge. Anything not in this list
//! goes through a normal bundle edit instead — the patch contract is
//! the meta-programming layer, not a general bundle DSL.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::workflow::{WorkflowEdge, WorkflowNode};
use super::workflow_bundle::{
    preview_workflow_bundle, validate_workflow_bundle, WorkflowBundle, WorkflowBundleGraphExport,
    WorkflowBundlePolicy, WorkflowBundleValidationReport,
};
use super::CapabilityPolicy;

pub const WORKFLOW_PATCH_SCHEMA_VERSION: u32 = 1;

/// A bounded, auditable proposal to mutate a workflow bundle.
///
/// Patches are flat lists of [`WorkflowPatchOperation`]s applied in
/// order. An empty operations list is rejected by [`apply_workflow_patch`]
/// — silent no-ops would let agents claim work they did not do.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowPatch {
    pub schema_version: u32,
    pub id: String,
    pub summary: Option<String>,
    pub operations: Vec<WorkflowPatchOperation>,
}

/// Operations are tagged by an external `op` discriminator so handlers
/// can dispatch on the literal string from JSON.
///
/// Only the operations called out in #1423 are exposed: insert node,
/// add edge, upsert prompt capsule, update node policy, update bundle
/// policy. New operations require a deliberate contract bump.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WorkflowPatchOperation {
    /// Insert a new workflow node. Fails if `node_id` already exists or
    /// is empty. The `node` body uses the same shape as
    /// `workflow.nodes[k]` in a bundle JSON, with sensible defaults
    /// applied for unspecified fields.
    InsertNode {
        node_id: String,
        #[serde(default)]
        node: WorkflowPatchNodeBody,
    },
    /// Append an edge to the workflow graph. Fails if either endpoint
    /// references an unknown node, or if the edge already exists.
    AddEdge {
        from: String,
        to: String,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        label: Option<String>,
    },
    /// Insert or replace a prompt capsule. The capsule's `node_id` must
    /// reference a node that exists after all prior patch operations
    /// have been applied (so an `InsertNode` followed by
    /// `UpsertPromptCapsule` works).
    UpsertPromptCapsule {
        capsule_id: String,
        capsule: WorkflowPatchPromptCapsuleBody,
    },
    /// Merge per-node policy fields onto an existing node. Only the
    /// fields named on [`WorkflowPatchNodePolicyBody`] can be set —
    /// arbitrary node fields are intentionally not patchable.
    UpdateNodePolicy {
        node_id: String,
        policy: WorkflowPatchNodePolicyBody,
    },
    /// Merge bundle-level policy fields. Mirrors the safe knobs in
    /// [`WorkflowBundlePolicy`].
    UpdateBundlePolicy {
        policy: WorkflowPatchBundlePolicyBody,
    },
}

/// Shape used by [`WorkflowPatchOperation::InsertNode`]. Mirrors the
/// editable fields on [`WorkflowNode`] without exposing every tunable.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowPatchNodeBody {
    pub kind: Option<String>,
    pub task_label: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub tools: Option<serde_json::Value>,
    pub model_policy: Option<serde_json::Value>,
    pub capability_policy: Option<CapabilityPolicy>,
    pub approval_policy: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Shape used by [`WorkflowPatchOperation::UpdateNodePolicy`]. Each
/// field maps to the same-named field on [`WorkflowNode`] and is
/// merged in place (set when `Some`, left alone when `None`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowPatchNodePolicyBody {
    pub task_label: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub tools: Option<serde_json::Value>,
    pub model_policy: Option<serde_json::Value>,
    pub capability_policy: Option<CapabilityPolicy>,
    pub approval_policy: Option<serde_json::Value>,
}

/// Shape used by [`WorkflowPatchOperation::UpsertPromptCapsule`]. The
/// patch always sets `id` to match `capsule_id` so the bundle invariant
/// (`capsule.id == map_key`) holds without callers needing to repeat it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowPatchPromptCapsuleBody {
    pub node_id: String,
    pub trigger_id: Option<String>,
    pub prompt: String,
    pub system: Option<String>,
    pub context: BTreeMap<String, serde_json::Value>,
}

/// Shape used by [`WorkflowPatchOperation::UpdateBundlePolicy`]. Each
/// field replaces the corresponding [`WorkflowBundlePolicy`] field when
/// `Some`. The `tool_policy` and `approval_required` lists replace
/// rather than merge to keep the contract obvious — agents that want a
/// merge should compute and submit the full list.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowPatchBundlePolicyBody {
    pub autonomy_tier: Option<String>,
    pub tool_policy: Option<BTreeMap<String, serde_json::Value>>,
    pub approval_required: Option<Vec<String>>,
    pub retry: Option<serde_json::Value>,
    pub catchup: Option<serde_json::Value>,
}

/// What a host renders when it shows the result of validating a patch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPatchValidationReport {
    pub schema_version: u32,
    pub patch_id: String,
    pub bundle_id: String,
    pub valid: bool,
    pub apply_errors: Vec<WorkflowPatchDiagnostic>,
    pub bundle_validation: WorkflowBundleValidationReport,
    pub graph_diff: WorkflowPatchGraphDiff,
    pub capability_delta: WorkflowPatchCapabilityDelta,
    pub graph_export: WorkflowBundleGraphExport,
}

/// One structured failure from applying a patch. The `op_index` lets
/// the host highlight the offending operation in its review surface.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPatchDiagnostic {
    pub severity: String,
    pub op_index: Option<usize>,
    pub op: Option<String>,
    pub path: String,
    pub message: String,
    pub node_id: Option<String>,
}

/// Structural diff between the original and patched workflow graph.
/// Used by host UIs and the patch-authoring skill so a model that
/// proposes a patch can tell at a glance whether its edit produced the
/// shape it intended.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPatchGraphDiff {
    pub added_nodes: Vec<String>,
    pub added_edges: Vec<WorkflowPatchEdgeRef>,
    pub updated_nodes: Vec<String>,
    pub updated_capsules: Vec<String>,
    pub policy_fields_changed: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPatchEdgeRef {
    pub from: String,
    pub to: String,
    pub branch: Option<String>,
    pub label: Option<String>,
}

/// Capability ceiling delta between the bundle before and after the
/// patch, optionally compared against a parent execution policy.
///
/// `widening` collects every dimension where the patched bundle asks
/// for *more* than the parent ceiling (or the original bundle, when no
/// parent is supplied). The patch is rejected when this list is
/// non-empty — agents must not be able to expand permissions.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPatchCapabilityDelta {
    pub before: CapabilityPolicy,
    pub after: CapabilityPolicy,
    pub parent: Option<CapabilityPolicy>,
    pub added_tools: Vec<String>,
    pub added_capabilities: BTreeMap<String, Vec<String>>,
    pub raised_side_effect_level: Option<RaisedSideEffectLevel>,
    pub added_workspace_roots: Vec<String>,
    #[serde(default)]
    pub added_read_only_roots: Vec<String>,
    pub added_connector_scopes: BTreeMap<String, Vec<String>>,
    pub added_command_gates: Vec<String>,
    pub raised_autonomy_tier: Option<RaisedAutonomyTier>,
    pub widening: Vec<CapabilityCeilingViolation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RaisedSideEffectLevel {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RaisedAutonomyTier {
    pub from: String,
    pub to: String,
}

/// One concrete way the patched bundle exceeds the parent ceiling.
/// `kind` is a stable enum string so hosts can group/explain them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityCeilingViolation {
    pub kind: String,
    pub detail: String,
}

/// Apply a patch to a copy of the bundle and return the new bundle.
/// Fails fast on the first structural error; callers that want richer
/// diagnostics should prefer [`validate_workflow_patch`].
pub fn apply_workflow_patch(
    bundle: &WorkflowBundle,
    patch: &WorkflowPatch,
) -> Result<WorkflowBundle, Vec<WorkflowPatchDiagnostic>> {
    let mut errors = Vec::new();
    if patch.schema_version != WORKFLOW_PATCH_SCHEMA_VERSION {
        errors.push(diagnostic_global(format!(
            "unsupported patch schema_version {}; expected {}",
            patch.schema_version, WORKFLOW_PATCH_SCHEMA_VERSION
        )));
    }
    if patch.id.trim().is_empty() {
        errors.push(diagnostic_global("patch id is required".to_string()));
    }
    if patch.operations.is_empty() {
        errors.push(diagnostic_global(
            "patch contains no operations; refusing to no-op".to_string(),
        ));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut working = bundle.clone();
    for (index, operation) in patch.operations.iter().enumerate() {
        if let Err(diag) = apply_operation(&mut working, operation, index) {
            return Err(vec![diag]);
        }
    }
    Ok(working)
}

/// Apply + validate + diff + ceiling check, in one pass. The bundle
/// validator is the source of truth for "is this still a valid bundle?";
/// this function adds the patch-specific apply errors, the structural
/// diff, and the capability delta on top.
pub fn validate_workflow_patch(
    bundle: &WorkflowBundle,
    patch: &WorkflowPatch,
    parent_ceiling: Option<&CapabilityPolicy>,
) -> WorkflowPatchValidationReport {
    let before_ceiling = bundle_capability_ceiling(bundle);

    let (patched, apply_errors) = match apply_workflow_patch(bundle, patch) {
        Ok(patched) => (patched, Vec::new()),
        Err(errors) => (bundle.clone(), errors),
    };

    let bundle_validation = validate_workflow_bundle(&patched);
    let graph_diff = diff_bundle_graph(bundle, &patched, patch);
    let after_ceiling = bundle_capability_ceiling(&patched);
    let capability_delta = compute_capability_delta(
        bundle,
        &patched,
        before_ceiling,
        after_ceiling,
        parent_ceiling,
    );
    let graph_export = preview_workflow_bundle(&patched).graph;
    let valid =
        apply_errors.is_empty() && bundle_validation.valid && capability_delta.widening.is_empty();

    WorkflowPatchValidationReport {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        patch_id: patch.id.clone(),
        bundle_id: bundle.id.clone(),
        valid,
        apply_errors,
        bundle_validation,
        graph_diff,
        capability_delta,
        graph_export,
    }
}

/// Project a bundle's effective capability ceiling. The bundle does
/// not carry a single capability policy; we compose one from the
/// per-node `capability_policy` declarations, the bundle-level
/// `tool_policy` keys, the connector scopes, the autonomy tier, the
/// `worktree_policy`, and the declared `command_gates`. The result is
/// what a parent runtime needs to compare against to decide whether
/// running this bundle would widen its own ceiling.
pub fn bundle_capability_ceiling(bundle: &WorkflowBundle) -> CapabilityPolicy {
    let mut tools: BTreeSet<String> = bundle.policy.tool_policy.keys().cloned().collect();
    let mut capabilities: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut workspace_roots: BTreeSet<String> = BTreeSet::new();
    let mut read_only_roots: BTreeSet<String> = BTreeSet::new();
    let mut max_side_effect: Option<&'static str> = None;

    for node in bundle.workflow.nodes.values() {
        for tool in &node.capability_policy.tools {
            tools.insert(tool.clone());
        }
        for (capability, ops) in &node.capability_policy.capabilities {
            let entry = capabilities.entry(capability.clone()).or_default();
            for op in ops {
                entry.insert(op.clone());
            }
        }
        for root in &node.capability_policy.workspace_roots {
            workspace_roots.insert(root.clone());
        }
        for root in &node.capability_policy.read_only_roots {
            read_only_roots.insert(root.clone());
        }
        if let Some(level) = node.capability_policy.side_effect_level.as_deref() {
            max_side_effect = match max_side_effect {
                Some(current) if side_effect_rank(current) >= side_effect_rank(level) => {
                    Some(current)
                }
                _ => Some(static_side_effect(level)),
            };
        }
    }

    let autonomy_floor = autonomy_side_effect_floor(&bundle.policy.autonomy_tier);
    if let Some(floor) = autonomy_floor {
        max_side_effect = match max_side_effect {
            Some(current) if side_effect_rank(current) >= side_effect_rank(floor) => Some(current),
            _ => Some(floor),
        };
    }

    if !bundle.connectors.is_empty() {
        capabilities
            .entry("connector".to_string())
            .or_default()
            .insert("call".to_string());
    }
    if !bundle.environment.command_gates.is_empty()
        || bundle.environment.worktree_policy != "host_managed"
    {
        capabilities
            .entry("process".to_string())
            .or_default()
            .insert("exec".to_string());
        max_side_effect = match max_side_effect {
            Some(current) if side_effect_rank(current) >= side_effect_rank("process_exec") => {
                Some(current)
            }
            _ => Some("process_exec"),
        };
    }

    CapabilityPolicy {
        tools: tools.into_iter().collect(),
        capabilities: capabilities
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect(),
        workspace_roots: workspace_roots.into_iter().collect(),
        read_only_roots: read_only_roots.into_iter().collect(),
        side_effect_level: max_side_effect.map(|level| level.to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
        process_sandbox: Default::default(),
    }
}

fn apply_operation(
    bundle: &mut WorkflowBundle,
    operation: &WorkflowPatchOperation,
    index: usize,
) -> Result<(), WorkflowPatchDiagnostic> {
    match operation {
        WorkflowPatchOperation::InsertNode { node_id, node } => {
            if node_id.trim().is_empty() {
                return Err(diagnostic_op(
                    index,
                    "insert_node",
                    "operations".to_string(),
                    "insert_node node_id is required".to_string(),
                    None,
                ));
            }
            if bundle.workflow.nodes.contains_key(node_id) {
                return Err(diagnostic_op(
                    index,
                    "insert_node",
                    format!("workflow.nodes.{node_id}"),
                    format!("workflow already contains node {node_id}"),
                    Some(node_id.clone()),
                ));
            }
            let workflow_node = node_body_into_workflow_node(node_id, node);
            bundle.workflow.nodes.insert(node_id.clone(), workflow_node);
            if bundle.workflow.entry.is_empty() {
                bundle.workflow.entry = node_id.clone();
            }
            Ok(())
        }
        WorkflowPatchOperation::AddEdge {
            from,
            to,
            branch,
            label,
        } => {
            if !bundle.workflow.nodes.contains_key(from) {
                return Err(diagnostic_op(
                    index,
                    "add_edge",
                    "edges.from".to_string(),
                    format!("edge.from references unknown node: {from}"),
                    Some(from.clone()),
                ));
            }
            if !bundle.workflow.nodes.contains_key(to) {
                return Err(diagnostic_op(
                    index,
                    "add_edge",
                    "edges.to".to_string(),
                    format!("edge.to references unknown node: {to}"),
                    Some(to.clone()),
                ));
            }
            let candidate = WorkflowEdge {
                from: from.clone(),
                to: to.clone(),
                branch: branch.clone(),
                label: label.clone(),
            };
            if bundle.workflow.edges.iter().any(|edge| {
                edge.from == candidate.from
                    && edge.to == candidate.to
                    && edge.branch == candidate.branch
                    && edge.label == candidate.label
            }) {
                return Err(diagnostic_op(
                    index,
                    "add_edge",
                    "edges".to_string(),
                    format!("edge {from} -> {to} already exists"),
                    Some(from.clone()),
                ));
            }
            bundle.workflow.edges.push(candidate);
            Ok(())
        }
        WorkflowPatchOperation::UpsertPromptCapsule {
            capsule_id,
            capsule,
        } => {
            if capsule_id.trim().is_empty() {
                return Err(diagnostic_op(
                    index,
                    "upsert_prompt_capsule",
                    "prompt_capsules".to_string(),
                    "capsule_id is required".to_string(),
                    None,
                ));
            }
            if !bundle.workflow.nodes.contains_key(&capsule.node_id) {
                return Err(diagnostic_op(
                    index,
                    "upsert_prompt_capsule",
                    format!("prompt_capsules.{capsule_id}.node_id"),
                    format!(
                        "prompt capsule references unknown node: {}",
                        capsule.node_id
                    ),
                    Some(capsule.node_id.clone()),
                ));
            }
            let existing = bundle
                .prompt_capsules
                .values()
                .find(|other| other.node_id == capsule.node_id && other.id != *capsule_id);
            if let Some(other) = existing {
                return Err(diagnostic_op(
                    index,
                    "upsert_prompt_capsule",
                    format!("prompt_capsules.{capsule_id}.node_id"),
                    format!(
                        "prompt capsule {capsule_id} would target node {} but capsule {} already targets it",
                        capsule.node_id, other.id
                    ),
                    Some(capsule.node_id.clone()),
                ));
            }
            let capsule_value = super::workflow_bundle::PromptCapsule {
                id: capsule_id.clone(),
                node_id: capsule.node_id.clone(),
                trigger_id: capsule.trigger_id.clone(),
                prompt: capsule.prompt.clone(),
                system: capsule.system.clone(),
                context: capsule.context.clone(),
            };
            bundle
                .prompt_capsules
                .insert(capsule_id.clone(), capsule_value);
            Ok(())
        }
        WorkflowPatchOperation::UpdateNodePolicy { node_id, policy } => {
            let Some(node) = bundle.workflow.nodes.get_mut(node_id) else {
                return Err(diagnostic_op(
                    index,
                    "update_node_policy",
                    format!("workflow.nodes.{node_id}"),
                    format!("workflow does not contain node {node_id}"),
                    Some(node_id.clone()),
                ));
            };
            apply_node_policy_body(node, policy).map_err(|message| {
                diagnostic_op(
                    index,
                    "update_node_policy",
                    format!("workflow.nodes.{node_id}"),
                    message,
                    Some(node_id.clone()),
                )
            })?;
            Ok(())
        }
        WorkflowPatchOperation::UpdateBundlePolicy { policy } => {
            apply_bundle_policy_body(&mut bundle.policy, policy).map_err(|message| {
                diagnostic_op(
                    index,
                    "update_bundle_policy",
                    "policy".to_string(),
                    message,
                    None,
                )
            })?;
            Ok(())
        }
    }
}

fn node_body_into_workflow_node(node_id: &str, body: &WorkflowPatchNodeBody) -> WorkflowNode {
    let mut node = WorkflowNode {
        id: Some(node_id.to_string()),
        kind: body
            .kind
            .clone()
            .filter(|kind| !kind.trim().is_empty())
            .unwrap_or_else(|| "stage".to_string()),
        ..WorkflowNode::default()
    };
    node.task_label = body.task_label.clone();
    node.prompt = body.prompt.clone();
    node.system = body.system.clone();
    if let Some(tools) = &body.tools {
        node.tools = tools.clone();
    }
    if let Some(model_policy) = &body.model_policy {
        if let Ok(parsed) = serde_json::from_value(model_policy.clone()) {
            node.model_policy = parsed;
        }
    }
    if let Some(capability_policy) = &body.capability_policy {
        node.capability_policy = capability_policy.clone();
    }
    if let Some(approval_policy) = &body.approval_policy {
        if let Ok(parsed) = serde_json::from_value(approval_policy.clone()) {
            node.approval_policy = parsed;
        }
    }
    node.metadata = body.metadata.clone();
    node
}

fn apply_node_policy_body(
    node: &mut WorkflowNode,
    body: &WorkflowPatchNodePolicyBody,
) -> Result<(), String> {
    if let Some(label) = &body.task_label {
        node.task_label = Some(label.clone());
    }
    if let Some(prompt) = &body.prompt {
        node.prompt = Some(prompt.clone());
    }
    if let Some(system) = &body.system {
        node.system = Some(system.clone());
    }
    if let Some(tools) = &body.tools {
        node.tools = tools.clone();
    }
    if let Some(model_policy) = &body.model_policy {
        node.model_policy = serde_json::from_value(model_policy.clone())
            .map_err(|error| format!("invalid model_policy: {error}"))?;
    }
    if let Some(capability_policy) = &body.capability_policy {
        node.capability_policy = capability_policy.clone();
    }
    if let Some(approval_policy) = &body.approval_policy {
        node.approval_policy = serde_json::from_value(approval_policy.clone())
            .map_err(|error| format!("invalid approval_policy: {error}"))?;
    }
    Ok(())
}

fn apply_bundle_policy_body(
    policy: &mut WorkflowBundlePolicy,
    body: &WorkflowPatchBundlePolicyBody,
) -> Result<(), String> {
    if let Some(autonomy) = &body.autonomy_tier {
        policy.autonomy_tier = autonomy.clone();
    }
    if let Some(tool_policy) = &body.tool_policy {
        policy.tool_policy = tool_policy.clone();
    }
    if let Some(approval_required) = &body.approval_required {
        policy.approval_required = approval_required.clone();
    }
    if let Some(retry) = &body.retry {
        policy.retry = serde_json::from_value(retry.clone())
            .map_err(|error| format!("invalid retry: {error}"))?;
    }
    if let Some(catchup) = &body.catchup {
        policy.catchup = serde_json::from_value(catchup.clone())
            .map_err(|error| format!("invalid catchup: {error}"))?;
    }
    Ok(())
}

fn diff_bundle_graph(
    before: &WorkflowBundle,
    after: &WorkflowBundle,
    patch: &WorkflowPatch,
) -> WorkflowPatchGraphDiff {
    let mut diff = WorkflowPatchGraphDiff::default();
    let before_node_ids: BTreeSet<&String> = before.workflow.nodes.keys().collect();
    for node_id in after.workflow.nodes.keys() {
        if !before_node_ids.contains(node_id) {
            diff.added_nodes.push(node_id.clone());
        }
    }
    let before_edges: BTreeSet<(String, String, Option<String>, Option<String>)> = before
        .workflow
        .edges
        .iter()
        .map(|edge| {
            (
                edge.from.clone(),
                edge.to.clone(),
                edge.branch.clone(),
                edge.label.clone(),
            )
        })
        .collect();
    for edge in &after.workflow.edges {
        let key = (
            edge.from.clone(),
            edge.to.clone(),
            edge.branch.clone(),
            edge.label.clone(),
        );
        if !before_edges.contains(&key) {
            diff.added_edges.push(WorkflowPatchEdgeRef {
                from: edge.from.clone(),
                to: edge.to.clone(),
                branch: edge.branch.clone(),
                label: edge.label.clone(),
            });
        }
    }
    for operation in &patch.operations {
        match operation {
            WorkflowPatchOperation::UpdateNodePolicy { node_id, .. } => {
                diff.updated_nodes.push(node_id.clone());
            }
            WorkflowPatchOperation::UpsertPromptCapsule { capsule_id, .. } => {
                diff.updated_capsules.push(capsule_id.clone());
            }
            WorkflowPatchOperation::UpdateBundlePolicy { policy } => {
                if policy.autonomy_tier.is_some() {
                    diff.policy_fields_changed.push("autonomy_tier".to_string());
                }
                if policy.tool_policy.is_some() {
                    diff.policy_fields_changed.push("tool_policy".to_string());
                }
                if policy.approval_required.is_some() {
                    diff.policy_fields_changed
                        .push("approval_required".to_string());
                }
                if policy.retry.is_some() {
                    diff.policy_fields_changed.push("retry".to_string());
                }
                if policy.catchup.is_some() {
                    diff.policy_fields_changed.push("catchup".to_string());
                }
            }
            _ => {}
        }
    }
    diff.added_nodes.sort();
    diff.updated_nodes.sort();
    diff.updated_nodes.dedup();
    diff.updated_capsules.sort();
    diff.updated_capsules.dedup();
    diff.policy_fields_changed.sort();
    diff.policy_fields_changed.dedup();
    diff.added_edges
        .sort_by(|left, right| (&left.from, &left.to).cmp(&(&right.from, &right.to)));
    diff
}

fn compute_capability_delta(
    before_bundle: &WorkflowBundle,
    after_bundle: &WorkflowBundle,
    before: CapabilityPolicy,
    after: CapabilityPolicy,
    parent: Option<&CapabilityPolicy>,
) -> WorkflowPatchCapabilityDelta {
    let added_tools: Vec<String> = after
        .tools
        .iter()
        .filter(|tool| !before.tools.contains(tool))
        .cloned()
        .collect();

    let mut added_capabilities: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (capability, ops) in &after.capabilities {
        let before_ops = before
            .capabilities
            .get(capability)
            .cloned()
            .unwrap_or_default();
        let added: Vec<String> = ops
            .iter()
            .filter(|op| !before_ops.contains(op))
            .cloned()
            .collect();
        if !added.is_empty() {
            added_capabilities.insert(capability.clone(), added);
        }
    }

    let raised_side_effect_level = match (
        before.side_effect_level.as_deref(),
        after.side_effect_level.as_deref(),
    ) {
        (Some(before_level), Some(after_level))
            if side_effect_rank(after_level) > side_effect_rank(before_level) =>
        {
            Some(RaisedSideEffectLevel {
                from: before_level.to_string(),
                to: after_level.to_string(),
            })
        }
        (None, Some(after_level)) => Some(RaisedSideEffectLevel {
            from: "none".to_string(),
            to: after_level.to_string(),
        }),
        _ => None,
    };

    let added_workspace_roots: Vec<String> = after
        .workspace_roots
        .iter()
        .filter(|root| !before.workspace_roots.contains(root))
        .cloned()
        .collect();

    let added_read_only_roots: Vec<String> = after
        .read_only_roots
        .iter()
        .filter(|root| !before.read_only_roots.contains(root))
        .cloned()
        .collect();

    let mut added_connector_scopes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let before_scopes_by_id: BTreeMap<&str, BTreeSet<&str>> = before_bundle
        .connectors
        .iter()
        .map(|connector| {
            (
                connector.id.as_str(),
                connector.scopes.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    for connector in &after_bundle.connectors {
        let before_scopes = before_scopes_by_id
            .get(connector.id.as_str())
            .cloned()
            .unwrap_or_default();
        let added: Vec<String> = connector
            .scopes
            .iter()
            .filter(|scope| !before_scopes.contains(scope.as_str()))
            .cloned()
            .collect();
        if !added.is_empty() {
            added_connector_scopes.insert(connector.id.clone(), added);
        }
    }

    let added_command_gates: Vec<String> = after_bundle
        .environment
        .command_gates
        .iter()
        .filter(|gate| !before_bundle.environment.command_gates.contains(gate))
        .cloned()
        .collect();

    let raised_autonomy_tier = match (
        before_bundle.policy.autonomy_tier.as_str(),
        after_bundle.policy.autonomy_tier.as_str(),
    ) {
        (before_tier, after_tier) if autonomy_rank(after_tier) > autonomy_rank(before_tier) => {
            Some(RaisedAutonomyTier {
                from: before_tier.to_string(),
                to: after_tier.to_string(),
            })
        }
        _ => None,
    };

    let widening = match parent {
        Some(parent) => collect_ceiling_violations(
            parent,
            &after,
            &added_connector_scopes,
            &added_command_gates,
            raised_autonomy_tier.as_ref(),
        ),
        None => Vec::new(),
    };

    WorkflowPatchCapabilityDelta {
        before,
        after,
        parent: parent.cloned(),
        added_tools,
        added_capabilities,
        raised_side_effect_level,
        added_workspace_roots,
        added_read_only_roots,
        added_connector_scopes,
        added_command_gates,
        raised_autonomy_tier,
        widening,
    }
}

fn collect_ceiling_violations(
    parent: &CapabilityPolicy,
    requested: &CapabilityPolicy,
    added_connector_scopes: &BTreeMap<String, Vec<String>>,
    added_command_gates: &[String],
    raised_autonomy_tier: Option<&RaisedAutonomyTier>,
) -> Vec<CapabilityCeilingViolation> {
    let mut violations = Vec::new();
    if !parent.tools.is_empty() {
        for tool in &requested.tools {
            if !parent.tools.contains(tool) {
                violations.push(CapabilityCeilingViolation {
                    kind: "tool".to_string(),
                    detail: format!("tool '{tool}' is not in parent tool ceiling"),
                });
            }
        }
    }
    for (capability, ops) in &requested.capabilities {
        match parent.capabilities.get(capability) {
            Some(parent_ops) => {
                for op in ops {
                    if !parent_ops.contains(op) {
                        violations.push(CapabilityCeilingViolation {
                            kind: "capability".to_string(),
                            detail: format!(
                                "capability '{capability}.{op}' exceeds parent ceiling"
                            ),
                        });
                    }
                }
            }
            None if !parent.capabilities.is_empty() => {
                violations.push(CapabilityCeilingViolation {
                    kind: "capability".to_string(),
                    detail: format!("capability '{capability}' is not in parent ceiling"),
                });
            }
            _ => {}
        }
    }
    if let (Some(parent_level), Some(requested_level)) = (
        parent.side_effect_level.as_deref(),
        requested.side_effect_level.as_deref(),
    ) {
        if side_effect_rank(requested_level) > side_effect_rank(parent_level) {
            violations.push(CapabilityCeilingViolation {
                kind: "side_effect_level".to_string(),
                detail: format!(
                    "side_effect_level '{requested_level}' exceeds parent ceiling '{parent_level}'"
                ),
            });
        }
    }
    if !parent.workspace_roots.is_empty() {
        for root in &requested.workspace_roots {
            if !parent.workspace_roots.contains(root) {
                violations.push(CapabilityCeilingViolation {
                    kind: "workspace_root".to_string(),
                    detail: format!("workspace_root '{root}' exceeds parent allowlist"),
                });
            }
        }
    }
    // A read-only root is within ceiling if the parent could read it —
    // any of its writable or read-only roots.
    if !parent.workspace_roots.is_empty() || !parent.read_only_roots.is_empty() {
        for root in &requested.read_only_roots {
            if !parent.workspace_roots.contains(root) && !parent.read_only_roots.contains(root) {
                violations.push(CapabilityCeilingViolation {
                    kind: "read_only_root".to_string(),
                    detail: format!("read_only_root '{root}' exceeds parent allowlist"),
                });
            }
        }
    }
    if !added_connector_scopes.is_empty() {
        let parent_allows_connector_calls = parent
            .capabilities
            .get("connector")
            .is_some_and(|ops| ops.iter().any(|op| op == "call"));
        if !parent_allows_connector_calls && !parent.capabilities.is_empty() {
            for (connector_id, scopes) in added_connector_scopes {
                violations.push(CapabilityCeilingViolation {
                    kind: "connector_scope".to_string(),
                    detail: format!(
                        "connector '{connector_id}' adds scopes {scopes:?} but parent ceiling does not include connector.call"
                    ),
                });
            }
        }
    }
    if !added_command_gates.is_empty() {
        let parent_allows_exec = parent
            .capabilities
            .get("process")
            .is_some_and(|ops| ops.iter().any(|op| op == "exec"));
        if !parent_allows_exec && !parent.capabilities.is_empty() {
            violations.push(CapabilityCeilingViolation {
                kind: "command_gate".to_string(),
                detail: format!(
                    "patch adds command gates {added_command_gates:?} but parent ceiling does not include process.exec"
                ),
            });
        }
    }
    if let Some(raised) = raised_autonomy_tier {
        violations.push(CapabilityCeilingViolation {
            kind: "autonomy_tier".to_string(),
            detail: format!(
                "autonomy_tier raised from '{}' to '{}' — patches must not widen autonomy",
                raised.from, raised.to
            ),
        });
    }
    violations
}

fn side_effect_rank(level: &str) -> usize {
    crate::tool_annotations::SideEffectLevel::rank_str(level)
}

fn static_side_effect(level: &str) -> &'static str {
    // Canonical normalization (single source of truth). A previous local table
    // silently downgraded any level it didn't list (e.g. `desktop_control`) to
    // `none`; parsing through the ladder keeps every known level intact and maps
    // only a genuinely-unknown value to `none`.
    crate::tool_annotations::SideEffectLevel::parse(level).as_str()
}

fn autonomy_rank(tier: &str) -> usize {
    match tier {
        "shadow" => 0,
        "suggest" => 1,
        "act_with_approval" => 2,
        "act_auto" => 3,
        _ => 0,
    }
}

fn autonomy_side_effect_floor(tier: &str) -> Option<&'static str> {
    match tier {
        "act_auto" => Some("network"),
        "act_with_approval" => Some("process_exec"),
        "suggest" => Some("read_only"),
        _ => None,
    }
}

fn diagnostic_op(
    index: usize,
    op: &str,
    path: String,
    message: String,
    node_id: Option<String>,
) -> WorkflowPatchDiagnostic {
    WorkflowPatchDiagnostic {
        severity: "error".to_string(),
        op_index: Some(index),
        op: Some(op.to_string()),
        path,
        message,
        node_id,
    }
}

fn diagnostic_global(message: String) -> WorkflowPatchDiagnostic {
    WorkflowPatchDiagnostic {
        severity: "error".to_string(),
        op_index: None,
        op: None,
        path: "patch".to_string(),
        message,
        node_id: None,
    }
}

#[cfg(test)]
#[path = "workflow_patch_tests.rs"]
mod workflow_patch_tests;
