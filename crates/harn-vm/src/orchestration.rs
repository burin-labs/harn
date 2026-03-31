use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::llm::{extract_llm_options, vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{ts}")
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}

fn default_run_dir() -> PathBuf {
    std::env::var("HARN_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".harn-runs"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapabilityPolicy {
    pub tools: Vec<String>,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub workspace_roots: Vec<String>,
    pub side_effect_level: Option<String>,
    pub recursion_limit: Option<usize>,
}

impl CapabilityPolicy {
    pub fn intersect(&self, requested: &CapabilityPolicy) -> Result<CapabilityPolicy, String> {
        let side_effect_level = match (&self.side_effect_level, &requested.side_effect_level) {
            (Some(a), Some(b)) => Some(min_side_effect(a, b).to_string()),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        };

        if !self.tools.is_empty() {
            let denied: Vec<String> = requested
                .tools
                .iter()
                .filter(|tool| !self.tools.contains(*tool))
                .cloned()
                .collect();
            if !denied.is_empty() {
                return Err(format!(
                    "requested tools exceed host ceiling: {}",
                    denied.join(", ")
                ));
            }
        }

        for (capability, requested_ops) in &requested.capabilities {
            if let Some(allowed_ops) = self.capabilities.get(capability) {
                let denied: Vec<String> = requested_ops
                    .iter()
                    .filter(|op| !allowed_ops.contains(*op))
                    .cloned()
                    .collect();
                if !denied.is_empty() {
                    return Err(format!(
                        "requested capability operations exceed host ceiling: {}.{}",
                        capability,
                        denied.join(",")
                    ));
                }
            } else if !self.capabilities.is_empty() {
                return Err(format!(
                    "requested capability exceeds host ceiling: {capability}"
                ));
            }
        }

        let tools = if self.tools.is_empty() {
            requested.tools.clone()
        } else if requested.tools.is_empty() {
            self.tools.clone()
        } else {
            requested
                .tools
                .iter()
                .filter(|tool| self.tools.contains(*tool))
                .cloned()
                .collect()
        };

        let capabilities = if self.capabilities.is_empty() {
            requested.capabilities.clone()
        } else if requested.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            requested
                .capabilities
                .iter()
                .filter_map(|(capability, requested_ops)| {
                    self.capabilities.get(capability).map(|allowed_ops| {
                        (
                            capability.clone(),
                            requested_ops
                                .iter()
                                .filter(|op| allowed_ops.contains(*op))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    })
                })
                .collect()
        };

        let workspace_roots = if self.workspace_roots.is_empty() {
            requested.workspace_roots.clone()
        } else if requested.workspace_roots.is_empty() {
            self.workspace_roots.clone()
        } else {
            requested
                .workspace_roots
                .iter()
                .filter(|root| self.workspace_roots.contains(*root))
                .cloned()
                .collect()
        };

        let recursion_limit = match (self.recursion_limit, requested.recursion_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            side_effect_level,
            recursion_limit,
        })
    }
}

fn min_side_effect<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptPolicy {
    pub mode: Option<String>,
    pub visibility: Option<String>,
    pub summarize: bool,
    pub compact: bool,
    pub keep_last: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPolicy {
    pub max_artifacts: Option<usize>,
    pub max_tokens: Option<usize>,
    pub include_kinds: Vec<String>,
    pub exclude_kinds: Vec<String>,
    pub render: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtifactRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub data: Option<serde_json::Value>,
    pub source: Option<String>,
    pub created_at: String,
    pub freshness: Option<String>,
    pub lineage: Vec<String>,
    pub relevance: Option<f64>,
    pub estimated_tokens: Option<usize>,
    pub stage: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ArtifactRecord {
    pub fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = "artifact".to_string();
        }
        if self.id.is_empty() {
            self.id = new_id("artifact");
        }
        if self.created_at.is_empty() {
            self.created_at = now_rfc3339();
        }
        if self.kind.is_empty() {
            self.kind = "artifact".to_string();
        }
        if self.estimated_tokens.is_none() {
            self.estimated_tokens = self
                .text
                .as_ref()
                .map(|text| ((text.len() as f64) / 4.0).ceil() as usize);
        }
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowNode {
    pub id: Option<String>,
    pub kind: String,
    pub mode: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub task_label: Option<String>,
    pub tools: Vec<String>,
    pub model_policy: ModelPolicy,
    pub transcript_policy: TranscriptPolicy,
    pub context_policy: ContextPolicy,
    pub retry_policy: RetryPolicy,
    pub capability_policy: CapabilityPolicy,
    pub verify: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    pub branch: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowGraph {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub version: usize,
    pub entry: String,
    pub nodes: BTreeMap<String, WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    pub capability_policy: CapabilityPolicy,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub audit_log: Vec<WorkflowAuditEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowAuditEntry {
    pub id: String,
    pub op: String,
    pub node_id: Option<String>,
    pub timestamp: String,
    pub reason: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageRecord {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub visible_text: Option<String>,
    pub private_reasoning: Option<String>,
    pub transcript: Option<serde_json::Value>,
    pub verification: Option<serde_json::Value>,
    pub artifacts: Vec<ArtifactRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub stages: Vec<RunStageRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub policy: CapabilityPolicy,
    pub transcript: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub persisted_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reachable_nodes: Vec<String>,
}

fn parse_json_value<T: for<'de> Deserialize<'de>>(value: &VmValue) -> Result<T, VmError> {
    serde_json::from_value(vm_value_to_json(value))
        .map_err(|e| VmError::Runtime(format!("orchestration parse error: {e}")))
}

pub fn normalize_workflow_value(value: &VmValue) -> Result<WorkflowGraph, VmError> {
    let mut graph: WorkflowGraph = parse_json_value(value)?;
    let as_dict = value.as_dict().cloned().unwrap_or_default();

    if graph.nodes.is_empty() {
        for key in ["act", "verify", "repair"] {
            if let Some(node_value) = as_dict.get(key) {
                let mut node: WorkflowNode = parse_json_value(node_value)?;
                let raw_node = node_value.as_dict().cloned().unwrap_or_default();
                node.id = Some(key.to_string());
                if node.kind.is_empty() {
                    node.kind = if key == "verify" {
                        "verify".to_string()
                    } else {
                        "stage".to_string()
                    };
                }
                if node.model_policy.provider.is_none() {
                    node.model_policy.provider = as_dict
                        .get("provider")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model.is_none() {
                    node.model_policy.model = as_dict
                        .get("model")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model_tier.is_none() {
                    node.model_policy.model_tier = as_dict
                        .get("model_tier")
                        .or_else(|| as_dict.get("tier"))
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.temperature.is_none() {
                    node.model_policy.temperature = as_dict.get("temperature").and_then(|value| {
                        if let VmValue::Float(number) = value {
                            Some(*number)
                        } else {
                            value.as_int().map(|number| number as f64)
                        }
                    });
                }
                if node.model_policy.max_tokens.is_none() {
                    node.model_policy.max_tokens =
                        as_dict.get("max_tokens").and_then(|value| value.as_int());
                }
                if node.mode.is_none() {
                    node.mode = as_dict
                        .get("mode")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if key == "verify"
                    && node.verify.is_none()
                    && (raw_node.contains_key("assert_text")
                        || raw_node.contains_key("command")
                        || raw_node.contains_key("expect_status")
                        || raw_node.contains_key("expect_text"))
                {
                    node.verify = Some(serde_json::json!({
                        "assert_text": raw_node.get("assert_text").map(vm_value_to_json),
                        "command": raw_node.get("command").map(vm_value_to_json),
                        "expect_status": raw_node.get("expect_status").map(vm_value_to_json),
                        "expect_text": raw_node.get("expect_text").map(vm_value_to_json),
                    }));
                }
                graph.nodes.insert(key.to_string(), node);
            }
        }
        if graph.entry.is_empty() && graph.nodes.contains_key("act") {
            graph.entry = "act".to_string();
        }
        if graph.edges.is_empty() && graph.nodes.contains_key("act") {
            if graph.nodes.contains_key("verify") {
                graph.edges.push(WorkflowEdge {
                    from: "act".to_string(),
                    to: "verify".to_string(),
                    branch: None,
                    label: None,
                });
            }
            if graph.nodes.contains_key("repair") {
                graph.edges.push(WorkflowEdge {
                    from: "verify".to_string(),
                    to: "repair".to_string(),
                    branch: Some("failed".to_string()),
                    label: None,
                });
                graph.edges.push(WorkflowEdge {
                    from: "repair".to_string(),
                    to: "verify".to_string(),
                    branch: Some("retry".to_string()),
                    label: None,
                });
            }
        }
    }

    if graph.type_name.is_empty() {
        graph.type_name = "workflow_graph".to_string();
    }
    if graph.id.is_empty() {
        graph.id = new_id("workflow");
    }
    if graph.version == 0 {
        graph.version = 1;
    }
    if graph.entry.is_empty() {
        graph.entry = graph
            .nodes
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "act".to_string());
    }
    for (node_id, node) in &mut graph.nodes {
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
    Ok(graph)
}

pub fn validate_workflow(
    graph: &WorkflowGraph,
    ceiling: Option<&CapabilityPolicy>,
) -> WorkflowValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !graph.nodes.contains_key(&graph.entry) {
        errors.push(format!("entry node does not exist: {}", graph.entry));
    }

    let node_ids: BTreeSet<String> = graph.nodes.keys().cloned().collect();
    for edge in &graph.edges {
        if !node_ids.contains(&edge.from) {
            errors.push(format!("edge.from references unknown node: {}", edge.from));
        }
        if !node_ids.contains(&edge.to) {
            errors.push(format!("edge.to references unknown node: {}", edge.to));
        }
    }

    let reachable_nodes = reachable_nodes(graph);
    for node_id in &node_ids {
        if !reachable_nodes.contains(node_id) {
            warnings.push(format!("node is unreachable: {node_id}"));
        }
    }

    if let Some(ceiling) = ceiling {
        if let Err(error) = ceiling.intersect(&graph.capability_policy) {
            errors.push(error);
        }
        for (node_id, node) in &graph.nodes {
            if let Err(error) = ceiling.intersect(&node.capability_policy) {
                errors.push(format!("node {node_id}: {error}"));
            }
        }
    }

    WorkflowValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
        reachable_nodes: reachable_nodes.into_iter().collect(),
    }
}

fn reachable_nodes(graph: &WorkflowGraph) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![graph.entry.clone()];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.from == node_id) {
            stack.push(edge.to.clone());
        }
    }
    seen
}

pub fn select_artifacts(
    mut artifacts: Vec<ArtifactRecord>,
    policy: &ContextPolicy,
) -> Vec<ArtifactRecord> {
    artifacts.retain(|artifact| {
        (policy.include_kinds.is_empty() || policy.include_kinds.contains(&artifact.kind))
            && !policy.exclude_kinds.contains(&artifact.kind)
    });
    artifacts.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.estimated_tokens
                    .unwrap_or(usize::MAX)
                    .cmp(&b.estimated_tokens.unwrap_or(usize::MAX))
            })
    });

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;
    for artifact in artifacts {
        if let Some(max_artifacts) = policy.max_artifacts {
            if selected.len() >= max_artifacts {
                break;
            }
        }
        let next_tokens = artifact.estimated_tokens.unwrap_or(0);
        if let Some(max_tokens) = policy.max_tokens {
            if used_tokens + next_tokens > max_tokens {
                continue;
            }
        }
        used_tokens += next_tokens;
        selected.push(artifact);
    }
    selected
}

pub fn render_artifacts_context(artifacts: &[ArtifactRecord], policy: &ContextPolicy) -> String {
    let mut parts = Vec::new();
    for artifact in artifacts {
        let title = artifact
            .title
            .clone()
            .unwrap_or_else(|| format!("{} {}", artifact.kind, artifact.id));
        let body = artifact
            .text
            .clone()
            .or_else(|| artifact.data.as_ref().map(|v| v.to_string()))
            .unwrap_or_default();
        match policy.render.as_deref() {
            Some("json") => {
                parts.push(
                    serde_json::json!({
                        "id": artifact.id,
                        "kind": artifact.kind,
                        "title": title,
                        "source": artifact.source,
                        "text": body,
                    })
                    .to_string(),
                );
            }
            _ => parts.push(format!("[{title}]\n{body}")),
        }
    }
    parts.join("\n\n")
}

pub fn normalize_artifact(value: &VmValue) -> Result<ArtifactRecord, VmError> {
    let artifact: ArtifactRecord = parse_json_value(value)?;
    Ok(artifact.normalize())
}

pub fn normalize_run_record(value: &VmValue) -> Result<RunRecord, VmError> {
    let mut run: RunRecord = parse_json_value(value)?;
    if run.type_name.is_empty() {
        run.type_name = "run_record".to_string();
    }
    if run.id.is_empty() {
        run.id = new_id("run");
    }
    if run.started_at.is_empty() {
        run.started_at = now_rfc3339();
    }
    if run.status.is_empty() {
        run.status = "running".to_string();
    }
    Ok(run)
}

pub fn save_run_record(run: &RunRecord, path: Option<&str>) -> Result<String, VmError> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_run_dir().join(format!("{}.json", run.id)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("failed to create run directory: {e}")))?;
    }
    let json = serde_json::to_string_pretty(run)
        .map_err(|e| VmError::Runtime(format!("failed to encode run record: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| VmError::Runtime(format!("failed to persist run record: {e}")))?;
    Ok(path.to_string_lossy().to_string())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))
}

fn compact_transcript(transcript: &VmValue, keep_last: usize) -> Option<VmValue> {
    let dict = transcript.as_dict()?;
    let messages = match dict.get("messages") {
        Some(VmValue::List(list)) => list.iter().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let retained = messages
        .into_iter()
        .rev()
        .take(keep_last)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let mut compacted = dict.clone();
    compacted.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(retained.clone())),
    );
    compacted.insert(
        "events".to_string(),
        VmValue::List(Rc::new(
            crate::llm::helpers::transcript_events_from_messages(&retained),
        )),
    );
    Some(VmValue::Dict(Rc::new(compacted)))
}

fn redact_transcript_visibility(transcript: &VmValue, visibility: Option<&str>) -> Option<VmValue> {
    let Some(visibility) = visibility else {
        return Some(transcript.clone());
    };
    if visibility != "public" && visibility != "public_only" {
        return Some(transcript.clone());
    }
    let dict = transcript.as_dict()?;
    let public_messages = match dict.get("messages") {
        Some(VmValue::List(list)) => list
            .iter()
            .filter(|message| {
                message
                    .as_dict()
                    .and_then(|d| d.get("role"))
                    .map(|v| v.display())
                    .map(|role| role != "tool_result")
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let public_events = match dict.get("events") {
        Some(VmValue::List(list)) => list
            .iter()
            .filter(|event| {
                event
                    .as_dict()
                    .and_then(|d| d.get("visibility"))
                    .map(|v| v.display())
                    .map(|value| value == "public")
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut redacted = dict.clone();
    redacted.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(public_messages)),
    );
    redacted.insert("events".to_string(), VmValue::List(Rc::new(public_events)));
    Some(VmValue::Dict(Rc::new(redacted)))
}

fn apply_input_transcript_policy(
    transcript: Option<VmValue>,
    policy: &TranscriptPolicy,
) -> Option<VmValue> {
    let mut transcript = transcript;
    match policy.mode.as_deref() {
        Some("reset") => return None,
        Some("fork") => {
            if let Some(VmValue::Dict(dict)) = transcript.as_ref() {
                let mut forked = dict.as_ref().clone();
                forked.insert(
                    "id".to_string(),
                    VmValue::String(Rc::from(new_id("transcript"))),
                );
                transcript = Some(VmValue::Dict(Rc::new(forked)));
            }
        }
        _ => {}
    }
    if policy.compact {
        let keep_last = policy.keep_last.unwrap_or(6);
        transcript = transcript.and_then(|value| compact_transcript(&value, keep_last));
    }
    transcript
}

fn apply_output_transcript_policy(
    transcript: Option<VmValue>,
    policy: &TranscriptPolicy,
) -> Option<VmValue> {
    let mut transcript = transcript;
    if policy.compact {
        let keep_last = policy.keep_last.unwrap_or(6);
        transcript = transcript.and_then(|value| compact_transcript(&value, keep_last));
    }
    transcript.and_then(|value| redact_transcript_visibility(&value, policy.visibility.as_deref()))
}

pub async fn execute_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let selected = select_artifacts(artifacts.to_vec(), &node.context_policy);
    let rendered_context = render_artifacts_context(&selected, &node.context_policy);
    let transcript = apply_input_transcript_policy(transcript, &node.transcript_policy);
    let prompt = if rendered_context.is_empty() {
        task.to_string()
    } else {
        format!(
            "{rendered_context}\n\n{}:\n{task}",
            node.task_label
                .clone()
                .unwrap_or_else(|| "Task".to_string())
        )
    };

    let mut options = BTreeMap::new();
    if let Some(provider) = &node.model_policy.provider {
        options.insert(
            "provider".to_string(),
            VmValue::String(Rc::from(provider.clone())),
        );
    }
    if let Some(model) = &node.model_policy.model {
        options.insert(
            "model".to_string(),
            VmValue::String(Rc::from(model.clone())),
        );
    }
    if let Some(model_tier) = &node.model_policy.model_tier {
        options.insert(
            "model_tier".to_string(),
            VmValue::String(Rc::from(model_tier.clone())),
        );
    }
    if let Some(temperature) = node.model_policy.temperature {
        options.insert("temperature".to_string(), VmValue::Float(temperature));
    }
    if let Some(max_tokens) = node.model_policy.max_tokens {
        options.insert("max_tokens".to_string(), VmValue::Int(max_tokens));
    }
    if !node.tools.is_empty() {
        options.insert(
            "tools".to_string(),
            VmValue::List(Rc::new(
                node.tools
                    .iter()
                    .map(|tool| VmValue::String(Rc::from(tool.clone())))
                    .collect(),
            )),
        );
    }
    if let Some(transcript) = transcript.clone() {
        options.insert("transcript".to_string(), transcript);
    }

    let args = vec![
        VmValue::String(Rc::from(prompt)),
        node.system
            .clone()
            .map(|s| VmValue::String(Rc::from(s)))
            .unwrap_or(VmValue::Nil),
        VmValue::Dict(Rc::new(options)),
    ];
    let mut opts = extract_llm_options(&args)?;

    let llm_result = if node.mode.as_deref() == Some("agent") || !node.tools.is_empty() {
        crate::llm::run_agent_loop_internal(
            &mut opts,
            crate::llm::AgentLoopConfig {
                persistent: true,
                max_iterations: 12,
                max_nudges: 3,
                nudge: None,
                tool_retries: 0,
                tool_backoff_ms: 1000,
                tool_format: "text".to_string(),
            },
        )
        .await?
    } else {
        let result = vm_call_llm_full(&opts).await?;
        crate::llm::agent_loop_result_from_llm(&result, opts)
    };

    let visible_text = llm_result["text"].as_str().unwrap_or_default().to_string();
    let transcript = llm_result
        .get("transcript")
        .cloned()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    let transcript = apply_output_transcript_policy(transcript, &node.transcript_policy);
    let artifact = ArtifactRecord {
        type_name: "artifact".to_string(),
        id: new_id("artifact"),
        kind: if node.kind == "verify" {
            "verification_result".to_string()
        } else {
            "artifact".to_string()
        },
        title: Some(format!("stage {node_id} output")),
        text: Some(visible_text),
        data: Some(llm_result.clone()),
        source: Some(node_id.to_string()),
        created_at: now_rfc3339(),
        freshness: Some("fresh".to_string()),
        lineage: selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata: BTreeMap::new(),
    }
    .normalize();

    Ok((llm_result, vec![artifact], transcript))
}

pub fn next_node_for(graph: &WorkflowGraph, current: &str, status: &str) -> Option<String> {
    graph
        .edges
        .iter()
        .find(|edge| edge.from == current && edge.branch.as_deref() == Some(status))
        .or_else(|| {
            graph
                .edges
                .iter()
                .find(|edge| edge.from == current && edge.branch.is_none())
        })
        .map(|edge| edge.to.clone())
}

pub fn append_audit_entry(
    graph: &mut WorkflowGraph,
    op: &str,
    node_id: Option<String>,
    reason: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    graph.audit_log.push(WorkflowAuditEntry {
        id: new_id("audit"),
        op: op.to_string(),
        node_id,
        timestamp: now_rfc3339(),
        reason,
        metadata,
    });
}

pub fn builtin_ceiling() -> CapabilityPolicy {
    CapabilityPolicy {
        tools: vec![
            "read".to_string(),
            "read_file".to_string(),
            "search".to_string(),
            "edit".to_string(),
            "run".to_string(),
            "exec".to_string(),
            "outline".to_string(),
            "list_directory".to_string(),
            "lsp_hover".to_string(),
            "lsp_definition".to_string(),
            "lsp_references".to_string(),
            "web_search".to_string(),
            "web_fetch".to_string(),
        ],
        capabilities: BTreeMap::from([
            (
                "workspace".to_string(),
                vec![
                    "read_text".to_string(),
                    "write_text".to_string(),
                    "apply_edit".to_string(),
                    "delete".to_string(),
                    "exists".to_string(),
                    "list".to_string(),
                ],
            ),
            ("process".to_string(), vec!["exec".to_string()]),
        ]),
        workspace_roots: Vec::new(),
        side_effect_level: Some("network".to_string()),
        recursion_limit: Some(8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_intersection_rejects_privilege_expansion() {
        let ceiling = CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: BTreeMap::new(),
            workspace_roots: Vec::new(),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(2),
        };
        let requested = CapabilityPolicy {
            tools: vec!["read".to_string(), "edit".to_string()],
            ..Default::default()
        };
        let error = ceiling.intersect(&requested).unwrap_err();
        assert!(error.contains("host ceiling"));
    }

    #[test]
    fn workflow_normalization_upgrades_legacy_act_verify_repair_shape() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "name": "legacy",
            "act": {"mode": "llm"},
            "verify": {"kind": "verify"},
            "repair": {"mode": "agent"},
        }));
        let graph = normalize_workflow_value(&value).unwrap();
        assert_eq!(graph.type_name, "workflow_graph");
        assert!(graph.nodes.contains_key("act"));
        assert!(graph.nodes.contains_key("verify"));
        assert!(graph.nodes.contains_key("repair"));
        assert_eq!(graph.entry, "act");
    }

    #[test]
    fn artifact_selection_honors_budget_and_priority() {
        let policy = ContextPolicy {
            max_artifacts: Some(2),
            max_tokens: Some(30),
            ..Default::default()
        };
        let artifacts = vec![
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "a".to_string(),
                kind: "summary".to_string(),
                text: Some("short".to_string()),
                relevance: Some(0.9),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "b".to_string(),
                kind: "summary".to_string(),
                text: Some("this is a much larger artifact body".to_string()),
                relevance: Some(1.0),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "c".to_string(),
                kind: "summary".to_string(),
                text: Some("tiny".to_string()),
                relevance: Some(0.5),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
        ];
        let selected = select_artifacts(artifacts, &policy);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|artifact| artifact.kind == "summary"));
    }
}
