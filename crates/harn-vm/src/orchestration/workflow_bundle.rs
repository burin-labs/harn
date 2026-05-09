//! Portable workflow bundle contract and deterministic local receipts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{validate_workflow, WorkflowEdge, WorkflowGraph};

pub const WORKFLOW_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const WORKFLOW_BUNDLE_RECEIPT_TYPE: &str = "harn.workflow_bundle.run";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowBundle {
    pub schema_version: u32,
    pub id: String,
    pub name: Option<String>,
    pub version: String,
    pub triggers: Vec<WorkflowBundleTrigger>,
    pub workflow: WorkflowGraph,
    pub prompt_capsules: BTreeMap<String, PromptCapsule>,
    pub policy: WorkflowBundlePolicy,
    pub connectors: Vec<ConnectorRequirement>,
    pub environment: EnvironmentRequirements,
    pub receipts: WorkflowBundleReplayMetadata,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowBundleTrigger {
    pub id: String,
    pub kind: String,
    pub provider: Option<String>,
    pub events: Vec<String>,
    pub schedule: Option<String>,
    pub delay: Option<String>,
    pub webhook_path: Option<String>,
    pub mcp_tool: Option<String>,
    pub resume_key: Option<String>,
    pub node_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromptCapsule {
    pub id: String,
    pub node_id: String,
    pub trigger_id: Option<String>,
    pub prompt: String,
    pub system: Option<String>,
    pub context: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowBundlePolicy {
    pub autonomy_tier: String,
    pub tool_policy: BTreeMap<String, serde_json::Value>,
    pub approval_required: Vec<String>,
    pub retry: RetryPolicySpec,
    pub catchup: CatchupPolicySpec,
}

impl Default for WorkflowBundlePolicy {
    fn default() -> Self {
        Self {
            autonomy_tier: "act_with_approval".to_string(),
            tool_policy: BTreeMap::new(),
            approval_required: Vec::new(),
            retry: RetryPolicySpec::default(),
            catchup: CatchupPolicySpec::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetryPolicySpec {
    pub max_attempts: u32,
    pub backoff: String,
}

impl Default for RetryPolicySpec {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff: "none".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CatchupPolicySpec {
    pub mode: String,
    pub max_events: Option<u32>,
}

impl Default for CatchupPolicySpec {
    fn default() -> Self {
        Self {
            mode: "latest".to_string(),
            max_events: Some(1),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ConnectorRequirement {
    pub id: String,
    pub provider_id: String,
    pub scopes: Vec<String>,
    pub setup_required: bool,
    pub status_required: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EnvironmentRequirements {
    pub repo_setup_profile: Option<String>,
    pub worktree_policy: String,
    pub command_gates: Vec<String>,
}

impl Default for EnvironmentRequirements {
    fn default() -> Self {
        Self {
            repo_setup_profile: None,
            worktree_policy: "host_managed".to_string(),
            command_gates: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowBundleReplayMetadata {
    pub run_id: Option<String>,
    pub event_ids: Vec<String>,
    pub workflow_version: Option<usize>,
    pub graph_digest: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowBundleDiagnostic {
    pub severity: String,
    pub path: String,
    pub message: String,
    pub node_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowBundleValidationReport {
    pub valid: bool,
    pub bundle_id: String,
    pub workflow_id: String,
    pub graph_digest: String,
    pub errors: Vec<WorkflowBundleDiagnostic>,
    pub warnings: Vec<WorkflowBundleDiagnostic>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkflowBundlePreview {
    pub schema_version: u32,
    pub bundle_id: String,
    pub bundle_version: String,
    pub workflow_id: String,
    pub workflow_version: usize,
    pub graph_digest: String,
    pub validation: WorkflowBundleValidationReport,
    pub graph: WorkflowBundleGraphExport,
    pub mermaid: String,
    pub triggers: Vec<WorkflowBundleTrigger>,
    pub connectors: Vec<ConnectorRequirement>,
    pub environment: EnvironmentRequirements,
    pub nodes: Vec<WorkflowBundlePreviewNode>,
    pub edges: Vec<WorkflowEdge>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowBundlePreviewNode {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
    pub prompt_capsule: Option<String>,
    pub trigger_ids: Vec<String>,
    pub outgoing: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkflowBundleGraphExport {
    pub schema_version: u32,
    pub graph_id: String,
    pub graph_digest: String,
    pub nodes: Vec<WorkflowBundleGraphNode>,
    pub edges: Vec<WorkflowBundleGraphEdge>,
    pub diagnostics: Vec<WorkflowBundleGraphDiagnostic>,
    pub editable_fields: Vec<WorkflowBundleEditableField>,
    pub mermaid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkflowBundleGraphNode {
    pub id: String,
    pub node_type: String,
    pub label: String,
    pub workflow_node_id: Option<String>,
    pub trigger_id: Option<String>,
    pub connector_id: Option<String>,
    pub editable_fields: Vec<WorkflowBundleEditableField>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowBundleGraphEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowBundleGraphDiagnostic {
    pub severity: String,
    pub path: String,
    pub message: String,
    pub node_id: Option<String>,
    pub graph_node_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowBundleEditableField {
    pub id: String,
    pub label: String,
    pub json_pointer: String,
    pub value_type: String,
    pub required: bool,
    pub enum_values: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowBundleRunRequest {
    pub trigger_id: Option<String>,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkflowBundleRunReceipt {
    pub schema_version: u32,
    pub receipt_type: String,
    pub bundle_id: String,
    pub bundle_version: String,
    pub workflow_id: String,
    pub workflow_version: usize,
    pub graph_digest: String,
    pub run_id: String,
    pub trigger_id: Option<String>,
    pub event_ids: Vec<String>,
    pub status: String,
    pub executed_nodes: Vec<WorkflowBundleRunNodeReceipt>,
    pub policy: WorkflowBundlePolicy,
    pub connectors: Vec<ConnectorRequirement>,
    pub environment: EnvironmentRequirements,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowBundleRunNodeReceipt {
    pub node_id: String,
    pub kind: String,
    pub prompt_capsule: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowBundleError {
    pub message: String,
}

impl std::fmt::Display for WorkflowBundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for WorkflowBundleError {}

impl From<std::io::Error> for WorkflowBundleError {
    fn from(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for WorkflowBundleError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

pub fn load_workflow_bundle(path: &Path) -> Result<WorkflowBundle, WorkflowBundleError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

pub fn workflow_graph_digest(graph: &WorkflowGraph) -> String {
    let mut canonical = canonical_workflow_graph(graph);
    canonical.audit_log.clear();
    let bytes = serde_json::to_vec(&canonical).expect("workflow graph serializes");
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

pub fn validate_workflow_bundle(bundle: &WorkflowBundle) -> WorkflowBundleValidationReport {
    let canonical = canonical_workflow_graph(&bundle.workflow);
    let mut report = WorkflowBundleValidationReport {
        valid: true,
        bundle_id: bundle.id.clone(),
        workflow_id: canonical.id.clone(),
        graph_digest: workflow_graph_digest(&canonical),
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    validate_bundle_identity(bundle, &canonical, &mut report);
    validate_triggers(bundle, &canonical, &mut report);
    validate_prompt_capsules(bundle, &canonical, &mut report);
    validate_policy(bundle, &mut report);
    validate_connectors(bundle, &mut report);
    validate_environment(bundle, &mut report);

    let graph_report = validate_workflow(&canonical, None);
    for error in graph_report.errors {
        let node_id = workflow_diagnostic_node_id(&error, &canonical);
        push_error(&mut report, "workflow", error, node_id);
    }
    for warning in graph_report.warnings {
        let node_id = workflow_diagnostic_node_id(&warning, &canonical);
        push_warning(&mut report, "workflow", warning, node_id);
    }

    if let Some(expected) = bundle.receipts.graph_digest.as_deref() {
        if expected != report.graph_digest {
            let actual = report.graph_digest.clone();
            push_error(
                &mut report,
                "receipts.graph_digest",
                format!("graph digest mismatch: expected {expected}, computed {actual}"),
                None,
            );
        }
    }
    if let Some(expected_version) = bundle.receipts.workflow_version {
        if expected_version != canonical.version {
            push_error(
                &mut report,
                "receipts.workflow_version",
                format!(
                    "workflow version mismatch: expected {expected_version}, computed {}",
                    canonical.version
                ),
                None,
            );
        }
    }

    report.valid = report.errors.is_empty();
    report
}

pub fn preview_workflow_bundle(bundle: &WorkflowBundle) -> WorkflowBundlePreview {
    let canonical = canonical_workflow_graph(&bundle.workflow);
    let validation = validate_workflow_bundle(bundle);
    let graph = export_workflow_bundle_graph(bundle, &validation);
    let mermaid = graph.mermaid.clone();
    let triggers_by_node = triggers_by_node(bundle);
    let capsules_by_node = capsules_by_node(bundle);
    let mut nodes = Vec::new();

    for (node_id, node) in &canonical.nodes {
        let mut outgoing = canonical
            .edges
            .iter()
            .filter(|edge| edge.from == *node_id)
            .map(|edge| edge.to.clone())
            .collect::<Vec<_>>();
        outgoing.sort();
        outgoing.dedup();
        nodes.push(WorkflowBundlePreviewNode {
            id: node_id.clone(),
            kind: node.kind.clone(),
            label: node.task_label.clone(),
            prompt_capsule: capsules_by_node.get(node_id).cloned(),
            trigger_ids: triggers_by_node.get(node_id).cloned().unwrap_or_default(),
            outgoing,
        });
    }

    WorkflowBundlePreview {
        schema_version: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        bundle_id: bundle.id.clone(),
        bundle_version: bundle.version.clone(),
        workflow_id: canonical.id.clone(),
        workflow_version: canonical.version,
        graph_digest: validation.graph_digest.clone(),
        validation,
        graph,
        mermaid,
        triggers: bundle.triggers.clone(),
        connectors: bundle.connectors.clone(),
        environment: bundle.environment.clone(),
        nodes,
        edges: sorted_edges(&canonical),
    }
}

pub fn export_workflow_bundle_graph(
    bundle: &WorkflowBundle,
    validation: &WorkflowBundleValidationReport,
) -> WorkflowBundleGraphExport {
    let canonical = canonical_workflow_graph(&bundle.workflow);
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut editable_fields = Vec::new();
    let capsules_by_node = capsules_by_node(bundle);
    let catchup_enabled = bundle.policy.catchup.mode != "none";
    let retry_can_dlq = bundle.policy.retry.max_attempts > 1;

    for (index, connector) in bundle.connectors.iter().enumerate() {
        let node_fields = connector_editable_fields(index, connector);
        editable_fields.extend(node_fields.clone());
        nodes.push(WorkflowBundleGraphNode {
            id: connector_graph_id(&connector.id),
            node_type: "connector_call".to_string(),
            label: connector_label(connector),
            workflow_node_id: None,
            trigger_id: None,
            connector_id: Some(connector.id.clone()),
            editable_fields: node_fields,
            metadata: BTreeMap::from([
                (
                    "provider_id".to_string(),
                    serde_json::json!(connector.provider_id),
                ),
                ("scopes".to_string(), serde_json::json!(connector.scopes)),
            ]),
        });
    }

    let catchup_fields = catchup_editable_fields();
    let retry_fields = retry_editable_fields();
    if catchup_enabled {
        let node_fields = catchup_fields.clone();
        editable_fields.extend(node_fields.clone());
        nodes.push(WorkflowBundleGraphNode {
            id: catchup_graph_id(),
            node_type: "catchup".to_string(),
            label: "Catch up".to_string(),
            workflow_node_id: None,
            trigger_id: None,
            connector_id: None,
            editable_fields: node_fields,
            metadata: BTreeMap::from([(
                "mode".to_string(),
                serde_json::json!(bundle.policy.catchup.mode),
            )]),
        });
    } else {
        editable_fields.extend(catchup_fields);
    }
    if retry_can_dlq {
        let node_fields = retry_fields.clone();
        editable_fields.extend(node_fields.clone());
        nodes.push(WorkflowBundleGraphNode {
            id: dlq_graph_id(),
            node_type: "dlq".to_string(),
            label: "Dead letter queue".to_string(),
            workflow_node_id: None,
            trigger_id: None,
            connector_id: None,
            editable_fields: node_fields,
            metadata: BTreeMap::from([(
                "max_attempts".to_string(),
                serde_json::json!(bundle.policy.retry.max_attempts),
            )]),
        });
    } else {
        editable_fields.extend(retry_fields);
    }

    for (index, trigger) in bundle.triggers.iter().enumerate() {
        let node_fields = trigger_editable_fields(index, trigger);
        editable_fields.extend(node_fields.clone());
        nodes.push(WorkflowBundleGraphNode {
            id: trigger_graph_id(&trigger.id),
            node_type: "trigger".to_string(),
            label: trigger_label(trigger),
            workflow_node_id: trigger.node_id.clone(),
            trigger_id: Some(trigger.id.clone()),
            connector_id: None,
            editable_fields: node_fields,
            metadata: BTreeMap::from([
                ("kind".to_string(), serde_json::json!(trigger.kind)),
                ("provider".to_string(), serde_json::json!(trigger.provider)),
                ("events".to_string(), serde_json::json!(trigger.events)),
            ]),
        });
        if let Some(provider) = trigger.provider.as_deref() {
            if let Some(connector) = bundle
                .connectors
                .iter()
                .find(|connector| connector.provider_id == provider || connector.id == provider)
            {
                edges.push(WorkflowBundleGraphEdge {
                    from: connector_graph_id(&connector.id),
                    to: trigger_graph_id(&trigger.id),
                    label: Some("binds".to_string()),
                    branch: None,
                });
            }
        }
        let target = trigger
            .node_id
            .clone()
            .unwrap_or_else(|| canonical.entry.clone());
        if catchup_enabled {
            edges.push(WorkflowBundleGraphEdge {
                from: trigger_graph_id(&trigger.id),
                to: catchup_graph_id(),
                label: Some(bundle.policy.catchup.mode.clone()),
                branch: Some("catchup".to_string()),
            });
            edges.push(WorkflowBundleGraphEdge {
                from: catchup_graph_id(),
                to: workflow_graph_id(&target),
                label: Some("dispatch".to_string()),
                branch: None,
            });
        } else {
            edges.push(WorkflowBundleGraphEdge {
                from: trigger_graph_id(&trigger.id),
                to: workflow_graph_id(&target),
                label: Some("dispatch".to_string()),
                branch: None,
            });
        }
    }

    for (node_id, node) in &canonical.nodes {
        let capsule_id = capsules_by_node.get(node_id);
        let node_fields = workflow_node_editable_fields(node_id, capsule_id);
        editable_fields.extend(node_fields.clone());
        nodes.push(WorkflowBundleGraphNode {
            id: workflow_graph_id(node_id),
            node_type: workflow_node_type(&node.kind),
            label: workflow_node_label(node_id, node),
            workflow_node_id: Some(node_id.clone()),
            trigger_id: None,
            connector_id: None,
            editable_fields: node_fields,
            metadata: BTreeMap::from([
                ("kind".to_string(), serde_json::json!(node.kind)),
                ("task_label".to_string(), serde_json::json!(node.task_label)),
                (
                    "prompt_capsule".to_string(),
                    serde_json::json!(capsule_id.cloned()),
                ),
            ]),
        });
    }

    for edge in sorted_edges(&canonical) {
        edges.push(WorkflowBundleGraphEdge {
            from: workflow_graph_id(&edge.from),
            to: workflow_graph_id(&edge.to),
            label: edge.label.clone(),
            branch: edge.branch.clone(),
        });
    }

    let outgoing: BTreeSet<&str> = canonical
        .edges
        .iter()
        .map(|edge| edge.from.as_str())
        .collect();
    for node_id in canonical.nodes.keys() {
        if !outgoing.contains(node_id.as_str()) {
            edges.push(WorkflowBundleGraphEdge {
                from: workflow_graph_id(node_id),
                to: terminal_completed_graph_id(),
                label: Some("completed".to_string()),
                branch: Some("completed".to_string()),
            });
        }
        if retry_can_dlq {
            edges.push(WorkflowBundleGraphEdge {
                from: workflow_graph_id(node_id),
                to: dlq_graph_id(),
                label: Some("retry exhausted".to_string()),
                branch: Some("failed".to_string()),
            });
        }
    }

    nodes.push(WorkflowBundleGraphNode {
        id: terminal_completed_graph_id(),
        node_type: "terminal".to_string(),
        label: "Completed".to_string(),
        workflow_node_id: None,
        trigger_id: None,
        connector_id: None,
        editable_fields: Vec::new(),
        metadata: BTreeMap::from([("status".to_string(), serde_json::json!("completed"))]),
    });
    nodes.push(WorkflowBundleGraphNode {
        id: terminal_failed_graph_id(),
        node_type: "terminal".to_string(),
        label: "Failed".to_string(),
        workflow_node_id: None,
        trigger_id: None,
        connector_id: None,
        editable_fields: Vec::new(),
        metadata: BTreeMap::from([("status".to_string(), serde_json::json!("failed"))]),
    });
    if retry_can_dlq {
        edges.push(WorkflowBundleGraphEdge {
            from: dlq_graph_id(),
            to: terminal_failed_graph_id(),
            label: Some("failed".to_string()),
            branch: Some("failed".to_string()),
        });
    }

    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    edges.sort_by(|left, right| {
        (&left.from, &left.to, &left.branch, &left.label).cmp(&(
            &right.from,
            &right.to,
            &right.branch,
            &right.label,
        ))
    });
    editable_fields.sort_by(|left, right| left.id.cmp(&right.id));

    let diagnostics = validation
        .errors
        .iter()
        .chain(validation.warnings.iter())
        .map(|diagnostic| WorkflowBundleGraphDiagnostic {
            severity: diagnostic.severity.clone(),
            path: diagnostic.path.clone(),
            message: diagnostic.message.clone(),
            node_id: diagnostic.node_id.clone(),
            graph_node_id: diagnostic.node_id.as_deref().map(workflow_graph_id),
        })
        .collect::<Vec<_>>();
    let mermaid = render_workflow_bundle_mermaid(&nodes, &edges);

    WorkflowBundleGraphExport {
        schema_version: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        graph_id: canonical.id,
        graph_digest: validation.graph_digest.clone(),
        nodes,
        edges,
        diagnostics,
        editable_fields,
        mermaid,
    }
}

pub fn run_workflow_bundle(
    bundle: &WorkflowBundle,
    request: WorkflowBundleRunRequest,
) -> Result<WorkflowBundleRunReceipt, WorkflowBundleValidationReport> {
    let validation = validate_workflow_bundle(bundle);
    if !validation.valid {
        return Err(validation);
    }

    let canonical = canonical_workflow_graph(&bundle.workflow);
    let trigger_id = match request.trigger_id {
        Some(trigger_id)
            if !bundle
                .triggers
                .iter()
                .any(|trigger| trigger.id == trigger_id) =>
        {
            let mut report = validation;
            push_error(
                &mut report,
                "trigger_id",
                format!("unknown trigger id: {trigger_id}"),
                None,
            );
            report.valid = false;
            return Err(report);
        }
        Some(trigger_id) => Some(trigger_id),
        None => bundle.triggers.first().map(|trigger| trigger.id.clone()),
    };
    let mut event_ids = bundle.receipts.event_ids.clone();
    if let Some(event_id) = request.event_id {
        if !event_ids.contains(&event_id) {
            event_ids.push(event_id);
        }
    }
    let run_id = bundle
        .receipts
        .run_id
        .clone()
        .unwrap_or_else(|| default_run_id(bundle, &validation.graph_digest));
    let capsules_by_node = capsules_by_node(bundle);
    let executed_nodes = execution_order(&canonical)
        .into_iter()
        .map(|node_id| {
            let node = canonical
                .nodes
                .get(&node_id)
                .expect("execution order only contains known nodes");
            WorkflowBundleRunNodeReceipt {
                node_id: node_id.clone(),
                kind: node.kind.clone(),
                prompt_capsule: capsules_by_node.get(&node_id).cloned(),
                status: "completed".to_string(),
            }
        })
        .collect();

    Ok(WorkflowBundleRunReceipt {
        schema_version: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        receipt_type: WORKFLOW_BUNDLE_RECEIPT_TYPE.to_string(),
        bundle_id: bundle.id.clone(),
        bundle_version: bundle.version.clone(),
        workflow_id: canonical.id,
        workflow_version: canonical.version,
        graph_digest: validation.graph_digest,
        run_id,
        trigger_id,
        event_ids,
        status: "completed".to_string(),
        executed_nodes,
        policy: bundle.policy.clone(),
        connectors: bundle.connectors.clone(),
        environment: bundle.environment.clone(),
    })
}

fn canonical_workflow_graph(graph: &WorkflowGraph) -> WorkflowGraph {
    let mut canonical = graph.clone();
    if canonical.type_name.is_empty() {
        canonical.type_name = "workflow_graph".to_string();
    }
    if canonical.version == 0 {
        canonical.version = 1;
    }
    if canonical.entry.is_empty() {
        canonical.entry = canonical.nodes.keys().next().cloned().unwrap_or_default();
    }
    for (node_id, node) in &mut canonical.nodes {
        if node.id.is_none() {
            node.id = Some(node_id.clone());
        }
        if node.kind.is_empty() {
            node.kind = "stage".to_string();
        }
        if node.retry_policy.max_attempts == 0 {
            node.retry_policy.max_attempts = 1;
        }
    }
    canonical.edges = sorted_edges(&canonical);
    canonical
}

fn sorted_edges(graph: &WorkflowGraph) -> Vec<WorkflowEdge> {
    let mut edges = graph.edges.clone();
    edges.sort_by(|left, right| {
        (
            &left.from,
            &left.to,
            left.branch.as_deref(),
            left.label.as_deref(),
        )
            .cmp(&(
                &right.from,
                &right.to,
                right.branch.as_deref(),
                right.label.as_deref(),
            ))
    });
    edges
}

fn validate_bundle_identity(
    bundle: &WorkflowBundle,
    graph: &WorkflowGraph,
    report: &mut WorkflowBundleValidationReport,
) {
    if bundle.schema_version != WORKFLOW_BUNDLE_SCHEMA_VERSION {
        push_error(
            report,
            "schema_version",
            format!(
                "unsupported schema_version {}; expected {}",
                bundle.schema_version, WORKFLOW_BUNDLE_SCHEMA_VERSION
            ),
            None,
        );
    }
    if bundle.id.trim().is_empty() {
        push_error(report, "id", "bundle id is required", None);
    }
    if bundle.version.trim().is_empty() {
        push_error(report, "version", "bundle version is required", None);
    }
    if graph.id.trim().is_empty() {
        push_error(
            report,
            "workflow.id",
            "workflow id is required for portable bundles",
            None,
        );
    }
    if graph.nodes.is_empty() {
        push_error(
            report,
            "workflow.nodes",
            "workflow must contain nodes",
            None,
        );
    }
    for (node_id, node) in &graph.nodes {
        if node_id.trim().is_empty() {
            push_error(report, "workflow.nodes", "node id is required", None);
        }
        if node.id.as_deref().is_some_and(|id| id != node_id) {
            push_error(
                report,
                format!("workflow.nodes.{node_id}.id"),
                "node id field must match its map key",
                Some(node_id.clone()),
            );
        }
    }
}

fn validate_triggers(
    bundle: &WorkflowBundle,
    graph: &WorkflowGraph,
    report: &mut WorkflowBundleValidationReport,
) {
    if bundle.triggers.is_empty() {
        push_warning(report, "triggers", "bundle declares no triggers", None);
    }
    let mut ids = BTreeSet::new();
    for (index, trigger) in bundle.triggers.iter().enumerate() {
        let path = format!("triggers[{index}]");
        if trigger.id.trim().is_empty() {
            push_error(report, format!("{path}.id"), "trigger id is required", None);
        } else if !ids.insert(trigger.id.clone()) {
            push_error(
                report,
                format!("{path}.id"),
                format!("duplicate trigger id: {}", trigger.id),
                None,
            );
        }
        match trigger.kind.as_str() {
            "github" => {
                if trigger.provider.as_deref() != Some("github") {
                    push_error(
                        report,
                        format!("{path}.provider"),
                        "github triggers require provider=\"github\"",
                        None,
                    );
                }
                if trigger.events.is_empty() {
                    push_error(
                        report,
                        format!("{path}.events"),
                        "github triggers require at least one event",
                        None,
                    );
                }
            }
            "cron" if trigger.schedule.is_none() => push_error(
                report,
                format!("{path}.schedule"),
                "cron triggers require schedule",
                None,
            ),
            "delay" if trigger.delay.is_none() => push_error(
                report,
                format!("{path}.delay"),
                "delay triggers require delay",
                None,
            ),
            "webhook" if trigger.webhook_path.is_none() => push_error(
                report,
                format!("{path}.webhook_path"),
                "webhook triggers require webhook_path",
                None,
            ),
            "mcp" if trigger.mcp_tool.is_none() => push_error(
                report,
                format!("{path}.mcp_tool"),
                "mcp triggers require mcp_tool",
                None,
            ),
            "manual" => {}
            "" => push_error(
                report,
                format!("{path}.kind"),
                "trigger kind is required",
                None,
            ),
            other
                if !matches!(
                    other,
                    "github" | "cron" | "delay" | "webhook" | "mcp" | "manual"
                ) =>
            {
                push_error(
                    report,
                    format!("{path}.kind"),
                    format!("unsupported trigger kind: {other}"),
                    None,
                );
            }
            _ => {}
        }
        if let Some(node_id) = trigger.node_id.as_deref() {
            if !graph.nodes.contains_key(node_id) {
                push_error(
                    report,
                    format!("{path}.node_id"),
                    format!("trigger references unknown node: {node_id}"),
                    Some(node_id.to_string()),
                );
            }
        }
    }
}

fn validate_prompt_capsules(
    bundle: &WorkflowBundle,
    graph: &WorkflowGraph,
    report: &mut WorkflowBundleValidationReport,
) {
    let trigger_ids: BTreeSet<&str> = bundle
        .triggers
        .iter()
        .map(|trigger| trigger.id.as_str())
        .collect();
    let mut node_refs = BTreeSet::new();
    for (key, capsule) in &bundle.prompt_capsules {
        let path = format!("prompt_capsules.{key}");
        if capsule.id.trim().is_empty() {
            push_error(
                report,
                format!("{path}.id"),
                "prompt capsule id is required",
                None,
            );
        } else if capsule.id != *key {
            push_error(
                report,
                format!("{path}.id"),
                "prompt capsule id must match its map key",
                None,
            );
        }
        if capsule.prompt.trim().is_empty() {
            push_error(
                report,
                format!("{path}.prompt"),
                "prompt capsule prompt is required",
                Some(capsule.node_id.clone()),
            );
        }
        if !graph.nodes.contains_key(&capsule.node_id) {
            push_error(
                report,
                format!("{path}.node_id"),
                format!(
                    "prompt capsule references unknown node: {}",
                    capsule.node_id
                ),
                Some(capsule.node_id.clone()),
            );
        }
        if !capsule.node_id.is_empty() && !node_refs.insert(capsule.node_id.clone()) {
            push_error(
                report,
                format!("{path}.node_id"),
                format!("multiple prompt capsules target node {}", capsule.node_id),
                Some(capsule.node_id.clone()),
            );
        }
        if let Some(trigger_id) = capsule.trigger_id.as_deref() {
            if !trigger_ids.contains(trigger_id) {
                push_error(
                    report,
                    format!("{path}.trigger_id"),
                    format!("prompt capsule references unknown trigger: {trigger_id}"),
                    Some(capsule.node_id.clone()),
                );
            }
        }
    }
}

fn validate_policy(bundle: &WorkflowBundle, report: &mut WorkflowBundleValidationReport) {
    if !matches!(
        bundle.policy.autonomy_tier.as_str(),
        "shadow" | "suggest" | "act_with_approval" | "act_auto"
    ) {
        push_error(
            report,
            "policy.autonomy_tier",
            "autonomy_tier must be shadow, suggest, act_with_approval, or act_auto",
            None,
        );
    }
    if bundle.policy.retry.max_attempts == 0 {
        push_error(
            report,
            "policy.retry.max_attempts",
            "retry.max_attempts must be at least 1",
            None,
        );
    }
    if !matches!(
        bundle.policy.catchup.mode.as_str(),
        "none" | "latest" | "all"
    ) {
        push_error(
            report,
            "policy.catchup.mode",
            "catchup.mode must be none, latest, or all",
            None,
        );
    }
}

fn validate_connectors(bundle: &WorkflowBundle, report: &mut WorkflowBundleValidationReport) {
    let mut ids = BTreeSet::new();
    let provider_ids: BTreeSet<&str> = bundle
        .connectors
        .iter()
        .map(|connector| connector.provider_id.as_str())
        .collect();
    for (index, connector) in bundle.connectors.iter().enumerate() {
        let path = format!("connectors[{index}]");
        if connector.id.trim().is_empty() {
            push_error(
                report,
                format!("{path}.id"),
                "connector id is required",
                None,
            );
        } else if !ids.insert(connector.id.clone()) {
            push_error(
                report,
                format!("{path}.id"),
                format!("duplicate connector id: {}", connector.id),
                None,
            );
        }
        if connector.provider_id.trim().is_empty() {
            push_error(
                report,
                format!("{path}.provider_id"),
                "connector provider_id is required",
                None,
            );
        }
    }
    for trigger in &bundle.triggers {
        if let Some(provider) = trigger.provider.as_deref() {
            if !provider_ids.contains(provider) {
                push_warning(
                    report,
                    "connectors",
                    format!(
                        "trigger {} references provider {provider} with no connector requirement",
                        trigger.id
                    ),
                    trigger.node_id.clone(),
                );
            }
        }
    }
}

fn validate_environment(bundle: &WorkflowBundle, report: &mut WorkflowBundleValidationReport) {
    if !matches!(
        bundle.environment.worktree_policy.as_str(),
        "reuse_current" | "new_worktree" | "host_managed"
    ) {
        push_error(
            report,
            "environment.worktree_policy",
            "worktree_policy must be reuse_current, new_worktree, or host_managed",
            None,
        );
    }
}

fn workflow_diagnostic_node_id(message: &str, graph: &WorkflowGraph) -> Option<String> {
    for prefix in [
        "node is unreachable: ",
        "edge.from references unknown node: ",
        "edge.to references unknown node: ",
        "entry node does not exist: ",
    ] {
        if let Some(node_id) = message.strip_prefix(prefix) {
            return Some(node_id.to_string());
        }
    }
    if let Some(rest) = message.strip_prefix("node ") {
        if let Some((node_id, _)) = rest.split_once(':') {
            return Some(node_id.to_string());
        }
    }
    graph
        .nodes
        .keys()
        .find(|node_id| message.contains(&format!("node {node_id}:")))
        .cloned()
}

fn workflow_graph_id(node_id: &str) -> String {
    format!("node/{node_id}")
}

fn trigger_graph_id(trigger_id: &str) -> String {
    format!("trigger/{trigger_id}")
}

fn connector_graph_id(connector_id: &str) -> String {
    format!("connector/{connector_id}")
}

fn catchup_graph_id() -> String {
    "policy/catchup".to_string()
}

fn dlq_graph_id() -> String {
    "policy/dlq".to_string()
}

fn terminal_completed_graph_id() -> String {
    "terminal/completed".to_string()
}

fn terminal_failed_graph_id() -> String {
    "terminal/failed".to_string()
}

fn workflow_node_type(kind: &str) -> String {
    match kind {
        "action" => "action",
        "stage" | "agent" => "agent",
        "subagent" | "worker" => "subagent",
        "wait" | "waitpoint" | "delay" => "wait",
        "approval" | "hitl" => "approval",
        "connector" | "connector_call" => "connector_call",
        "notification" | "notify" => "notification",
        "terminal" | "success" | "failure" => "terminal",
        other if other.trim().is_empty() => "agent",
        other => other,
    }
    .to_string()
}

fn workflow_node_label(node_id: &str, node: &super::WorkflowNode) -> String {
    node.task_label
        .clone()
        .or_else(|| node.prompt.clone())
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| node_id.to_string())
}

fn trigger_label(trigger: &WorkflowBundleTrigger) -> String {
    if !trigger.events.is_empty() {
        format!("{}: {}", trigger.kind, trigger.events.join(", "))
    } else if let Some(schedule) = trigger.schedule.as_deref() {
        format!("cron: {schedule}")
    } else if let Some(delay) = trigger.delay.as_deref() {
        format!("delay: {delay}")
    } else {
        trigger.id.clone()
    }
}

fn connector_label(connector: &ConnectorRequirement) -> String {
    if connector.provider_id.is_empty() || connector.provider_id == connector.id {
        connector.id.clone()
    } else {
        format!("{} ({})", connector.id, connector.provider_id)
    }
}

fn editable_field(
    id: impl Into<String>,
    label: impl Into<String>,
    json_pointer: impl Into<String>,
    value_type: impl Into<String>,
    required: bool,
    enum_values: &[&str],
) -> WorkflowBundleEditableField {
    WorkflowBundleEditableField {
        id: id.into(),
        label: label.into(),
        json_pointer: json_pointer.into(),
        value_type: value_type.into(),
        required,
        enum_values: enum_values
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    }
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn trigger_editable_fields(
    index: usize,
    trigger: &WorkflowBundleTrigger,
) -> Vec<WorkflowBundleEditableField> {
    let base = format!("/triggers/{index}");
    let mut fields = vec![
        editable_field(
            format!("trigger.{}.kind", trigger.id),
            "Trigger kind",
            format!("{base}/kind"),
            "enum",
            true,
            &["github", "cron", "delay", "manual", "webhook", "mcp"],
        ),
        editable_field(
            format!("trigger.{}.node_id", trigger.id),
            "Target node",
            format!("{base}/node_id"),
            "string",
            false,
            &[],
        ),
    ];
    if trigger.provider.is_some() || trigger.kind == "github" {
        fields.push(editable_field(
            format!("trigger.{}.provider", trigger.id),
            "Provider",
            format!("{base}/provider"),
            "string",
            trigger.kind == "github",
            &[],
        ));
    }
    for (field, label, value_type) in [
        ("events", "Events", "list"),
        ("schedule", "Schedule", "string"),
        ("delay", "Delay", "string"),
        ("webhook_path", "Webhook path", "string"),
        ("mcp_tool", "MCP tool", "string"),
        ("resume_key", "Resume key", "string"),
        ("metadata", "Metadata", "object"),
    ] {
        fields.push(editable_field(
            format!("trigger.{}.{}", trigger.id, field),
            label,
            format!("{base}/{field}"),
            value_type,
            false,
            &[],
        ));
    }
    fields
}

fn workflow_node_editable_fields(
    node_id: &str,
    capsule_id: Option<&String>,
) -> Vec<WorkflowBundleEditableField> {
    let escaped_node = json_pointer_segment(node_id);
    let mut fields = vec![
        editable_field(
            format!("workflow.{node_id}.task_label"),
            "Task label",
            format!("/workflow/nodes/{escaped_node}/task_label"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.prompt"),
            "Prompt",
            format!("/workflow/nodes/{escaped_node}/prompt"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.system"),
            "System prompt",
            format!("/workflow/nodes/{escaped_node}/system"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.model_policy"),
            "Model policy",
            format!("/workflow/nodes/{escaped_node}/model_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.tools"),
            "Tool policy",
            format!("/workflow/nodes/{escaped_node}/tools"),
            "any",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.capability_policy"),
            "Capability policy",
            format!("/workflow/nodes/{escaped_node}/capability_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.approval_policy"),
            "Approval policy",
            format!("/workflow/nodes/{escaped_node}/approval_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.retry_policy"),
            "Retry policy",
            format!("/workflow/nodes/{escaped_node}/retry_policy"),
            "object",
            false,
            &[],
        ),
    ];
    if let Some(capsule_id) = capsule_id {
        let escaped_capsule = json_pointer_segment(capsule_id);
        fields.extend([
            editable_field(
                format!("prompt_capsule.{capsule_id}.prompt"),
                "Prompt capsule",
                format!("/prompt_capsules/{escaped_capsule}/prompt"),
                "string",
                true,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.system"),
                "Prompt capsule system",
                format!("/prompt_capsules/{escaped_capsule}/system"),
                "string",
                false,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.context"),
                "Prompt capsule context",
                format!("/prompt_capsules/{escaped_capsule}/context"),
                "object",
                false,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.trigger_id"),
                "Prompt capsule trigger",
                format!("/prompt_capsules/{escaped_capsule}/trigger_id"),
                "string",
                false,
                &[],
            ),
        ]);
    }
    fields
}

fn connector_editable_fields(
    index: usize,
    connector: &ConnectorRequirement,
) -> Vec<WorkflowBundleEditableField> {
    let base = format!("/connectors/{index}");
    [
        ("id", "Connector id", "string", true),
        ("provider_id", "Provider id", "string", true),
        ("scopes", "Scopes", "list", false),
        ("setup_required", "Setup required", "bool", false),
        ("status_required", "Status required", "bool", false),
    ]
    .into_iter()
    .map(|(field, label, value_type, required)| {
        editable_field(
            format!("connector.{}.{}", connector.id, field),
            label,
            format!("{base}/{field}"),
            value_type,
            required,
            &[],
        )
    })
    .collect()
}

fn retry_editable_fields() -> Vec<WorkflowBundleEditableField> {
    vec![
        editable_field(
            "policy.retry.max_attempts",
            "Retry attempts",
            "/policy/retry/max_attempts",
            "integer",
            true,
            &[],
        ),
        editable_field(
            "policy.retry.backoff",
            "Retry backoff",
            "/policy/retry/backoff",
            "string",
            true,
            &[],
        ),
    ]
}

fn catchup_editable_fields() -> Vec<WorkflowBundleEditableField> {
    vec![
        editable_field(
            "policy.catchup.mode",
            "Catchup mode",
            "/policy/catchup/mode",
            "enum",
            true,
            &["none", "latest", "all"],
        ),
        editable_field(
            "policy.catchup.max_events",
            "Catchup max events",
            "/policy/catchup/max_events",
            "integer",
            false,
            &[],
        ),
    ]
}

fn render_workflow_bundle_mermaid(
    nodes: &[WorkflowBundleGraphNode],
    edges: &[WorkflowBundleGraphEdge],
) -> String {
    let mut lines = vec!["flowchart TD".to_string()];
    for node in nodes {
        lines.push(format!(
            "  {}[\"{}\"]",
            mermaid_id(&node.id),
            mermaid_label(&format!("{}: {}", node.node_type, node.label))
        ));
    }
    for edge in edges {
        let label = edge
            .label
            .as_deref()
            .or(edge.branch.as_deref())
            .map(mermaid_label);
        match label {
            Some(label) if !label.is_empty() => lines.push(format!(
                "  {} -->|{}| {}",
                mermaid_id(&edge.from),
                label,
                mermaid_id(&edge.to)
            )),
            _ => lines.push(format!(
                "  {} --> {}",
                mermaid_id(&edge.from),
                mermaid_id(&edge.to)
            )),
        }
    }
    lines.join("\n")
}

fn mermaid_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let suffix = digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut out = format!("n_{suffix}_");
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn mermaid_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

fn triggers_by_node(bundle: &WorkflowBundle) -> BTreeMap<String, Vec<String>> {
    let mut by_node: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for trigger in &bundle.triggers {
        if let Some(node_id) = trigger.node_id.as_ref() {
            by_node
                .entry(node_id.clone())
                .or_default()
                .push(trigger.id.clone());
        }
    }
    by_node
}

fn capsules_by_node(bundle: &WorkflowBundle) -> BTreeMap<String, String> {
    bundle
        .prompt_capsules
        .iter()
        .map(|(id, capsule)| (capsule.node_id.clone(), id.clone()))
        .collect()
}

fn execution_order(graph: &WorkflowGraph) -> Vec<String> {
    let outgoing =
        graph
            .edges
            .iter()
            .fold(BTreeMap::<String, Vec<String>>::new(), |mut acc, edge| {
                acc.entry(edge.from.clone())
                    .or_default()
                    .push(edge.to.clone());
                acc
            });
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([graph.entry.clone()]);
    let mut order = Vec::new();
    while let Some(node_id) = queue.pop_front() {
        if !graph.nodes.contains_key(&node_id) || !seen.insert(node_id.clone()) {
            continue;
        }
        order.push(node_id.clone());
        if let Some(next) = outgoing.get(&node_id) {
            let mut next = next.clone();
            next.sort();
            for child in next {
                queue.push_back(child);
            }
        }
    }
    order
}

fn default_run_id(bundle: &WorkflowBundle, graph_digest: &str) -> String {
    let suffix = graph_digest
        .strip_prefix("sha256:")
        .unwrap_or(graph_digest)
        .chars()
        .take(12)
        .collect::<String>();
    format!("bundle_run_{}_{}", sanitize_id(&bundle.id), suffix)
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn push_error(
    report: &mut WorkflowBundleValidationReport,
    path: impl Into<String>,
    message: impl Into<String>,
    node_id: Option<String>,
) {
    report.errors.push(WorkflowBundleDiagnostic {
        severity: "error".to_string(),
        path: path.into(),
        message: message.into(),
        node_id,
    });
}

fn push_warning(
    report: &mut WorkflowBundleValidationReport,
    path: impl Into<String>,
    message: impl Into<String>,
    node_id: Option<String>,
) {
    report.warnings.push(WorkflowBundleDiagnostic {
        severity: "warning".to_string(),
        path: path.into(),
        message: message.into(),
        node_id,
    });
}

#[cfg(test)]
#[path = "workflow_bundle_tests.rs"]
mod workflow_bundle_tests;
