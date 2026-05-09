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
        push_error(&mut report, "workflow", error, None);
    }
    for warning in graph_report.warnings {
        push_warning(&mut report, "workflow", warning, None);
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
        triggers: bundle.triggers.clone(),
        connectors: bundle.connectors.clone(),
        environment: bundle.environment.clone(),
        nodes,
        edges: sorted_edges(&canonical),
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
