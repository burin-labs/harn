//! Workflow graph types, verification contracts, and stage execution.

mod graph;
pub use graph::*;

#[cfg(test)]
mod flatten_tests;

use crate::value::VmDictExt;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    new_id, now_unix_seconds_text, redact_transcript_visibility, ArtifactRecord, AutoCompactPolicy,
    BranchSemantics, CapabilityPolicy, ContextPolicy, EqIgnored, EscalationPolicy, JoinPolicy,
    MapPolicy, ModelPolicy, ReducePolicy, RetryPolicy, StageContract,
};
use crate::llm::{extract_llm_options, vm_call_llm_full, vm_value_to_json};
use crate::tool_surface::{tool_capability_policy_from_spec, tool_names_from_spec};
use crate::value::{VmError, VmValue};

pub const WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY: &str = "workflow_verification_contracts";
pub const WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY: &str = "workflow_verification_scope";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowNode {
    pub id: Option<String>,
    pub kind: String,
    pub mode: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub task_label: Option<String>,
    pub done_sentinel: Option<String>,
    pub tools: serde_json::Value,
    pub model_policy: ModelPolicy,
    /// Per-stage auto-compaction settings for the agent loop's context
    /// window. Lifecycle operations (reset, fork, trim, compact) are NOT
    /// expressible here — call the `agent_session_*` builtins before the
    /// stage or in a prior stage.
    pub auto_compact: AutoCompactPolicy,
    /// Output visibility filter applied to the transcript after the
    /// stage's agent loop exits. `"public"` / `"public_only"` drops
    /// `tool_result` messages and non-public events. `None` or any
    /// unknown string is a no-op.
    #[serde(default)]
    pub output_visibility: Option<String>,
    pub context_policy: ContextPolicy,
    pub retry_policy: RetryPolicy,
    pub capability_policy: CapabilityPolicy,
    pub approval_policy: super::ToolApprovalPolicy,
    pub input_contract: StageContract,
    pub output_contract: StageContract,
    pub branch_semantics: BranchSemantics,
    pub map_policy: MapPolicy,
    pub join_policy: JoinPolicy,
    pub reduce_policy: ReducePolicy,
    pub escalation_policy: EscalationPolicy,
    pub verify: Option<serde_json::Value>,
    /// When true, the stage's agent loop gates the done sentinel on the most
    /// recent `run()` tool call exiting cleanly (`exit_code == 0`). Use for
    /// persistent execute stages that fold verification into the loop via a
    /// shell-exec tool the model invokes explicitly.
    #[serde(default)]
    pub exit_when_verified: bool,
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub raw_tools: Option<VmValue>,
    /// Raw auto_compact VmValue dict — preserved for extracting closure
    /// fields (compress_callback, mask_callback, custom_compactor) that
    /// can't go through serde.
    #[serde(skip)]
    pub raw_auto_compact: Option<VmValue>,
    /// Raw model_policy VmValue dict — preserved for extracting closure
    /// fields (post_turn_callback) that can't go through serde.
    #[serde(skip)]
    pub raw_model_policy: Option<VmValue>,
    /// Raw context_assembler VmValue dict — when set, the stage's
    /// artifact context is packed through `assemble_context` before
    /// rendering the system prompt. Closure fields (`ranker_callback`)
    /// are preserved here because they can't round-trip through serde.
    #[serde(skip)]
    pub raw_context_assembler: Option<VmValue>,
    /// Raw `verify` VmValue — preserved so a *callable* verifier (fn-verify
    /// mode) survives the builtin seam. The typed `verify` field above is a
    /// `serde_json::Value`, which drops closures; when `verify` is a Harn
    /// function the live closure is lifted here and re-attached by
    /// `node_to_vm_with_raw` (stage.rs) so the embedded stage loop can invoke
    /// it against each attempt's result (`workflow_evaluate_verification`).
    #[serde(skip)]
    pub raw_verify: Option<VmValue>,
    /// Raw `executor` VmValue — a caller-supplied closure that runs as the
    /// stage's leaf *instead of* spawning a delegated worker. When set, the
    /// embedded stage loop (`workflow_execute_stage_attempts`) invokes it with
    /// the retry context and shapes its return into the same settled payload
    /// `__host_stage_execute_once` produces, so the SAME retry-with-feedback
    /// threading, fn-verify gate, and (Rust sole-writer) attempt recording run
    /// around it. `WorkflowNode` has no typed `executor` field — the closure
    /// can't round-trip through serde — so it is lifted here and re-attached by
    /// `node_to_vm_with_raw` (stage.rs), exactly like `raw_verify`.
    #[serde(skip)]
    pub raw_executor: Option<VmValue>,
}

impl PartialEq for WorkflowNode {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationRequirement {
    pub kind: String,
    pub value: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationContract {
    pub source_node: Option<String>,
    pub summary: Option<String>,
    pub command: Option<String>,
    pub expect_status: Option<i64>,
    pub assert_text: Option<String>,
    pub expect_text: Option<String>,
    pub required_identifiers: Vec<String>,
    pub required_paths: Vec<String>,
    pub required_text: Vec<String>,
    pub notes: Vec<String>,
    pub checks: Vec<VerificationRequirement>,
}

impl VerificationContract {
    fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.command.is_none()
            && self.expect_status.is_none()
            && self.assert_text.is_none()
            && self.expect_text.is_none()
            && self.required_identifiers.is_empty()
            && self.required_paths.is_empty()
            && self.required_text.is_empty()
            && self.notes.is_empty()
            && self.checks.is_empty()
    }
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if !values.iter().any(|existing| existing == trimmed) {
        values.push(trimmed.to_string());
    }
}

fn push_unique_requirement(
    values: &mut Vec<VerificationRequirement>,
    kind: &str,
    value: &str,
    note: Option<&str>,
) {
    let trimmed_kind = kind.trim();
    let trimmed_value = value.trim();
    let trimmed_note = note
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string());
    if trimmed_kind.is_empty() || trimmed_value.is_empty() {
        return;
    }
    let candidate = VerificationRequirement {
        kind: trimmed_kind.to_string(),
        value: trimmed_value.to_string(),
        note: trimmed_note,
    };
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

fn json_string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(text)) => {
            let mut values = Vec::new();
            push_unique_string(&mut values, text);
            values
        }
        Some(serde_json::Value::Array(items)) => {
            let mut values = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    push_unique_string(&mut values, text);
                }
            }
            values
        }
        _ => Vec::new(),
    }
}

fn merge_verification_requirement_list(
    target: &mut Vec<VerificationRequirement>,
    value: Option<&serde_json::Value>,
) {
    let Some(items) = value.and_then(|raw| raw.as_array()) else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let kind = object
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let value = object
            .get("value")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let note = object
            .get("note")
            .or_else(|| object.get("description"))
            .or_else(|| object.get("reason"))
            .and_then(|value| value.as_str());
        push_unique_requirement(target, kind, value, note);
    }
}

fn merge_verification_contract_fields(
    target: &mut VerificationContract,
    object: &serde_json::Map<String, serde_json::Value>,
) {
    if target.summary.is_none() {
        target.summary = object
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.command.is_none() {
        target.command = object
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_status.is_none() {
        target.expect_status = object.get("expect_status").and_then(|value| value.as_i64());
    }
    if target.assert_text.is_none() {
        target.assert_text = object
            .get("assert_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_text.is_none() {
        target.expect_text = object
            .get("expect_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }

    for value in json_string_list(
        object
            .get("required_identifiers")
            .or_else(|| object.get("identifiers")),
    ) {
        push_unique_string(&mut target.required_identifiers, &value);
    }
    for value in json_string_list(object.get("required_paths").or_else(|| object.get("paths"))) {
        push_unique_string(&mut target.required_paths, &value);
    }
    for value in json_string_list(
        object
            .get("required_text")
            .or_else(|| object.get("exact_text"))
            .or_else(|| object.get("required_strings")),
    ) {
        push_unique_string(&mut target.required_text, &value);
    }
    for value in json_string_list(object.get("notes")) {
        push_unique_string(&mut target.notes, &value);
    }
    merge_verification_requirement_list(&mut target.checks, object.get("checks"));
}

fn load_verification_contract_file(path: &str) -> Result<serde_json::Value, VmError> {
    let resolved = crate::stdlib::process::resolve_source_asset_path(path);
    let contents = std::fs::read_to_string(&resolved).map_err(|error| {
        VmError::Runtime(format!(
            "workflow verification contract read failed for {}: {error}",
            resolved.display()
        ))
    })?;
    serde_json::from_str(&contents).map_err(|error| {
        VmError::Runtime(format!(
            "workflow verification contract parse failed for {}: {error}",
            resolved.display()
        ))
    })
}

fn resolve_verification_contract_path(
    verify: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<serde_json::Value>, VmError> {
    let Some(path) = verify
        .get("contract_path")
        .or_else(|| verify.get("verification_contract_path"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(load_verification_contract_file(path)?))
}

pub fn verification_contract_from_verify(
    node_id: &str,
    verify: Option<&serde_json::Value>,
) -> Result<Option<VerificationContract>, VmError> {
    let Some(verify_object) = verify.and_then(|value| value.as_object()) else {
        return Ok(None);
    };

    let mut contract = VerificationContract {
        source_node: Some(node_id.to_string()),
        ..Default::default()
    };

    if let Some(file_contract) = resolve_verification_contract_path(verify_object)? {
        let Some(object) = file_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verification contract file must parse to a JSON object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    if let Some(inline_contract) = verify_object.get("contract") {
        let Some(object) = inline_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verify.contract must be an object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    merge_verification_contract_fields(&mut contract, verify_object);

    if let Some(assert_text) = contract.assert_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "visible_text_contains",
            &assert_text,
            Some("verify stage requires visible output to contain this text"),
        );
    }
    if let Some(expect_text) = contract.expect_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "combined_output_contains",
            &expect_text,
            Some("verify command requires combined stdout/stderr to contain this text"),
        );
    }
    if let Some(expect_status) = contract.expect_status {
        push_unique_requirement(
            &mut contract.checks,
            "expect_status",
            &expect_status.to_string(),
            Some("verify command exit status must match exactly"),
        );
    }
    for identifier in contract.required_identifiers.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "identifier",
            &identifier,
            Some("use this exact identifier spelling"),
        );
    }
    for path in contract.required_paths.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "path",
            &path,
            Some("preserve this exact path"),
        );
    }
    for text in contract.required_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "text",
            &text,
            Some("required exact text or wiring snippet"),
        );
    }

    if contract.is_empty() {
        return Ok(None);
    }
    Ok(Some(contract))
}

fn push_unique_contract(values: &mut Vec<VerificationContract>, candidate: VerificationContract) {
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

pub fn workflow_verification_contracts(
    graph: &WorkflowGraph,
) -> Result<Vec<VerificationContract>, VmError> {
    let mut contracts = Vec::new();
    for (node_id, node) in &graph.nodes {
        if let Some(contract) = verification_contract_from_verify(node_id, node.verify.as_ref())? {
            push_unique_contract(&mut contracts, contract);
        }
    }
    Ok(contracts)
}

pub fn inject_workflow_verification_contracts(
    node: &mut WorkflowNode,
    contracts: &[VerificationContract],
) {
    if contracts.is_empty() {
        return;
    }
    node.metadata.insert(
        WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY.to_string(),
        serde_json::to_value(contracts).unwrap_or_default(),
    );
}

pub fn stage_verification_contracts(
    node_id: &str,
    node: &WorkflowNode,
) -> Result<Vec<VerificationContract>, VmError> {
    let local_contract = verification_contract_from_verify(node_id, node.verify.as_ref())?;
    let local_only = matches!(
        node.metadata
            .get(WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY)
            .and_then(|value| value.as_str()),
        Some("local_only")
    );
    if local_only {
        return Ok(local_contract.into_iter().collect());
    }

    let mut contracts = node
        .metadata
        .get(WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY)
        .cloned()
        .map(|value| {
            serde_json::from_value::<Vec<VerificationContract>>(value).map_err(|error| {
                VmError::Runtime(format!(
                    "workflow stage {node_id} verification contract metadata parse failed: {error}"
                ))
            })
        })
        .transpose()?
        .unwrap_or_default();

    if let Some(local_contract) = local_contract {
        push_unique_contract(&mut contracts, local_contract);
    }
    Ok(contracts)
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
    pub approval_policy: super::ToolApprovalPolicy,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub audit_log: Vec<WorkflowAuditEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowAuditEntry {
    pub id: String,
    pub op: String,
    pub node_id: Option<String>,
    pub timestamp: String,
    pub reason: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reachable_nodes: Vec<String>,
}

/// Pick the session id a stage should run under. Prefers an explicit
/// `session_id` on the node's `model_policy` dict (so pipelines with
/// `agent_session_open` / `agent_session_fork` flowing through a graph
/// line up); falls back to a stable, node-derived id so multi-stage
/// graphs with no explicit session share a conversation across stages.
fn resolve_node_session_id(node: &WorkflowNode) -> String {
    if let Some(explicit) = node
        .raw_model_policy
        .as_ref()
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("session_id"))
        .and_then(|v| match v {
            VmValue::String(s) if !s.trim().is_empty() => Some(s.to_string()),
            _ => None,
        })
    {
        return explicit;
    }
    if let Some(persisted) = node
        .metadata
        .get("worker_session_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return persisted.to_string();
    }
    format!("workflow_stage_{}", uuid::Uuid::now_v7())
}

fn raw_model_policy_dict(node: &WorkflowNode) -> Option<&crate::value::DictMap> {
    node.raw_model_policy
        .as_ref()
        .and_then(|value| value.as_dict())
}

fn insert_json_vm_option<T: Serialize>(
    options: &mut crate::value::DictMap,
    key: &str,
    value: &T,
) -> Result<(), VmError> {
    let json = serde_json::to_value(value).map_err(|error| {
        VmError::Runtime(format!("workflow stage option encode error: {error}"))
    })?;
    options.insert(
        crate::value::intern_key(key),
        crate::stdlib::json_to_vm_value(&json),
    );
    Ok(())
}

fn stage_tools_value(node: &WorkflowNode) -> Option<VmValue> {
    node.raw_tools.clone().or_else(|| {
        if matches!(node.tools, serde_json::Value::Null) {
            None
        } else {
            Some(crate::stdlib::json_to_vm_value(&node.tools))
        }
    })
}

fn add_stage_tools_option(
    options: &mut crate::value::DictMap,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
) {
    if !tool_names.is_empty() {
        if let Some(value) = tools_value.clone() {
            options.insert(crate::value::intern_key("tools"), value);
        }
    }
}

fn workflow_stage_llm_options(
    node: &WorkflowNode,
    stage_session_id: &str,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
    stage_agent_options: &super::WorkflowStageAgentOptions,
) -> Result<crate::value::DictMap, VmError> {
    let mut options = stage_agent_options.llm_options_vm_dict();
    if let Some(raw) = raw_model_policy_dict(node) {
        for (key, value) in crate::llm::helpers::project_llm_options(raw)? {
            if !matches!(value, VmValue::Nil) {
                options.insert(key, value);
            }
        }
    }
    options.put_str("session_id", stage_session_id);
    options.put_str("tool_format", stage_agent_options.tool_format.clone());
    add_stage_tools_option(&mut options, tools_value, tool_names);
    Ok(options)
}

/// Assemble the agent_loop options for one stage.
///
/// The policy *flattening* — collapsing the ~15 per-stage policy structs into
/// the options dict the loop consumes — lives in Harn
/// (`workflow_flatten_agent_loop_options` in `std/workflow/stage.harn`, design
/// D5). Rust keeps only the enforcement leaves: it re-derives the capability
/// ceiling (`tool spec ∩ stage capability_policy`) and, when the flattened
/// dict re-enters the host, rejects any result whose `policy` *widens* that
/// ceiling ([`enforce_flattened_ceiling`]). Raw model-policy / tool / compaction
/// values cross as `VmValue`s so their closures survive the round trip.
async fn workflow_stage_agent_loop_options(
    ctx: &crate::vm::AsyncBuiltinCtx,
    node: &WorkflowNode,
    stage_session_id: &str,
    tools_value: &Option<VmValue>,
    tool_names: &[String],
    stage_agent_options: &super::WorkflowStageAgentOptions,
) -> Result<crate::value::DictMap, VmError> {
    // Ceiling derivation stays in Rust (enforcement, not flattening): the
    // Harn flattener may narrow it but never widen it.
    let tool_policy = tool_capability_policy_from_spec(&node.tools);
    let effective_policy = tool_policy
        .intersect(&node.capability_policy)
        .map_err(VmError::Runtime)?;

    let stage_label = node
        .id
        .clone()
        .unwrap_or_else(|| stage_session_id.to_string());

    let mut config = crate::value::DictMap::new();
    config.insert(
        crate::value::intern_key("base"),
        VmValue::dict(stage_agent_options.agent_loop_options_vm_dict()),
    );
    config.insert(
        crate::value::intern_key("raw_model_policy"),
        node.raw_model_policy.clone().unwrap_or(VmValue::Nil),
    );
    insert_json_vm_option(&mut config, "auto_compact", &node.auto_compact)?;
    config.insert(
        crate::value::intern_key("raw_auto_compact"),
        node.raw_auto_compact.clone().unwrap_or(VmValue::Nil),
    );
    // The host only forwards a tool spec when the stage actually exposes tools;
    // matching the former `add_stage_tools_option` gate keeps the dict identical.
    config.insert(
        crate::value::intern_key("tools"),
        if tool_names.is_empty() {
            VmValue::Nil
        } else {
            tools_value.clone().unwrap_or(VmValue::Nil)
        },
    );
    crate::orchestration::WorkflowStageContext::apply_current_to_stage_config(&mut config);
    insert_json_vm_option(&mut config, "policy", &effective_policy)?;
    insert_json_vm_option(&mut config, "approval_policy", &node.approval_policy)?;
    config.put_str("session_id", stage_session_id);
    config.put_str("tool_format", stage_agent_options.tool_format.clone());
    config.put_str(
        "nested_kind",
        crate::orchestration::NestedExecutionKind::WorkflowStage.as_str(),
    );
    config.put_str("nested_label", stage_label);

    let flattened = crate::stdlib::harn_entry::call_harn_export_by_name(
        ctx,
        "std/workflow/stage",
        "workflow_flatten_agent_loop_options",
        "workflow_flatten_agent_loop_options",
        &[VmValue::dict(config)],
    )
    .await?;
    let VmValue::Dict(options) = flattened else {
        return Err(VmError::Runtime(
            "workflow_flatten_agent_loop_options must return a dict".to_string(),
        ));
    };
    let options = (*options).clone();
    enforce_flattened_ceiling(&options, &effective_policy)?;
    Ok(options)
}

/// Enforce the ceiling invariant on a Harn-flattened stage options dict: its
/// `policy` (the capability policy the loop will run under) must never widen
/// `ceiling`, the workflow-level grant Rust derived. This is the trust
/// boundary — the Harn flattener is untrusted for *authority*, only for
/// *shape*, so the host re-checks the returned policy rather than assuming the
/// flattener narrowed correctly.
fn enforce_flattened_ceiling(
    options: &crate::value::DictMap,
    ceiling: &CapabilityPolicy,
) -> Result<(), VmError> {
    let Some(policy_value) = options.get("policy") else {
        return Err(VmError::Runtime(
            "flattened stage options are missing the capability policy".to_string(),
        ));
    };
    let requested: CapabilityPolicy = serde_json::from_value(vm_value_to_json(policy_value))
        .map_err(|error| {
            VmError::Runtime(format!(
                "flattened stage capability policy is malformed: {error}"
            ))
        })?;
    ceiling
        .assert_within_ceiling(&requested)
        .map_err(|message| VmError::CategorizedError {
            message,
            category: crate::value::ErrorCategory::ToolRejected,
        })
}

#[derive(Clone, Debug)]
pub struct PreparedWorkflowStageNode {
    pub prompt: String,
    pub system: Option<String>,
    pub run_agent_loop: bool,
    pub llm_options: crate::value::DictMap,
    pub agent_loop_options: crate::value::DictMap,
    pub result: Option<serde_json::Value>,
    pub selected: Vec<ArtifactRecord>,
    pub rendered_context: String,
    pub rendered_verification: String,
    pub verification_contracts: Vec<VerificationContract>,
    pub tool_format: String,
    pub stage_session_id: String,
}

pub async fn prepare_stage_node(
    ctx: &crate::vm::AsyncBuiltinCtx,
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
) -> Result<PreparedWorkflowStageNode, VmError> {
    let selected_stage = super::select_workflow_stage_artifacts(
        ctx,
        artifacts,
        &node.context_policy,
        &node.input_contract,
    )
    .await?;
    let selected = selected_stage.artifacts;
    let context_policy = selected_stage.context_policy;
    let rendered_context_override = if let Some(assembler) = node.raw_context_assembler.as_ref() {
        let assembled =
            crate::stdlib::assemble::assemble_from_options(ctx, &selected, assembler).await?;
        Some(super::render_assembled_chunks(&assembled))
    } else {
        None
    };
    let verification_contracts = super::stage_verification_contracts(node_id, node)?;
    let stage_session_id = resolve_node_session_id(node);
    if node.input_contract.require_transcript && !crate::agent_sessions::exists(&stage_session_id) {
        return Err(VmError::Runtime(format!(
            "workflow stage {node_id} requires an existing session \
             (call agent_session_open and feed session_id through model_policy \
             before entering this stage)"
        )));
    }
    if let Some(min_inputs) = node.input_contract.min_inputs {
        if selected.len() < min_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} requires at least {min_inputs} input artifacts"
            )));
        }
    }
    if let Some(max_inputs) = node.input_contract.max_inputs {
        if selected.len() > max_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} accepts at most {max_inputs} input artifacts"
            )));
        }
    }
    let prepared_prompt = super::prepare_workflow_stage_prompt(
        ctx,
        task,
        node.task_label.as_deref(),
        &selected,
        &context_policy,
        rendered_context_override.as_deref(),
        &verification_contracts,
    )
    .await?;
    let prompt = prepared_prompt.prompt;
    let rendered_context = prepared_prompt.rendered_context;
    let rendered_verification = prepared_prompt.rendered_verification;

    let tool_names = tool_names_from_spec(&node.tools);
    let stage_agent_options = super::prepare_workflow_stage_agent_options(
        ctx,
        node,
        &stage_session_id,
        !tool_names.is_empty(),
    )
    .await?;
    let tool_format = stage_agent_options.tool_format.clone();
    let result = if node.kind == "verify" {
        if let Some(command) = node
            .verify
            .as_ref()
            .and_then(|verify| verify.as_object())
            .and_then(|verify| verify.get("command"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let (program, args) = if cfg!(target_os = "windows") {
                ("cmd", vec!["/C".to_string(), command.to_string()])
            } else {
                // Do not use a login shell here. On macOS, `/bin/sh -l`
                // reads user dotfiles such as `~/.profile`, which makes
                // sandboxed verification depend on out-of-worktree state.
                ("/bin/sh", vec!["-c".to_string(), command.to_string()])
            };
            let mut process_config = crate::stdlib::sandbox::ProcessCommandConfig::default();
            if let Some(context) = crate::stdlib::process::current_execution_context() {
                if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
                    crate::stdlib::sandbox::enforce_process_cwd(std::path::Path::new(&cwd))?;
                    process_config.cwd = Some(std::path::PathBuf::from(cwd));
                }
                if !context.env.is_empty() {
                    process_config.env.extend(context.env);
                }
            }
            let output = crate::stdlib::sandbox::command_output(program, &args, &process_config)?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = if stderr.is_empty() {
                stdout.clone()
            } else if stdout.is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            serde_json::json!({
                "status": "completed",
                "text": combined,
                "visible_text": combined,
                "command": command,
                "stdout": stdout,
                "stderr": stderr,
                "exit_status": output.status.code().unwrap_or(-1),
                "success": output.status.success(),
            })
        } else {
            serde_json::json!({
                "status": "completed",
                "text": "",
                "visible_text": "",
            })
        }
    } else {
        let tools_value = stage_tools_value(node);
        let llm_options = workflow_stage_llm_options(
            node,
            &stage_session_id,
            &tools_value,
            &tool_names,
            &stage_agent_options,
        )?;
        let agent_loop_options = if stage_agent_options.run_agent_loop {
            workflow_stage_agent_loop_options(
                ctx,
                node,
                &stage_session_id,
                &tools_value,
                &tool_names,
                &stage_agent_options,
            )
            .await?
        } else {
            crate::value::DictMap::new()
        };
        return Ok(PreparedWorkflowStageNode {
            prompt,
            system: node.system.clone(),
            run_agent_loop: stage_agent_options.run_agent_loop,
            llm_options,
            agent_loop_options,
            result: None,
            selected,
            rendered_context,
            rendered_verification,
            verification_contracts,
            tool_format,
            stage_session_id,
        });
    };

    Ok(PreparedWorkflowStageNode {
        prompt,
        system: node.system.clone(),
        run_agent_loop: false,
        llm_options: crate::value::DictMap::new(),
        agent_loop_options: crate::value::DictMap::new(),
        result: Some(result),
        selected,
        rendered_context,
        rendered_verification,
        verification_contracts,
        tool_format,
        stage_session_id,
    })
}

pub fn complete_prepared_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    prepared: &PreparedWorkflowStageNode,
    mut llm_result: serde_json::Value,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    if let Some(payload) = llm_result.as_object_mut() {
        payload.insert(
            "prompt".to_string(),
            serde_json::json!(prepared.prompt.clone()),
        );
        payload.insert(
            "system_prompt".to_string(),
            serde_json::json!(node.system.clone().unwrap_or_default()),
        );
        payload.insert(
            "rendered_context".to_string(),
            serde_json::json!(prepared.rendered_context.clone()),
        );
        if !prepared.verification_contracts.is_empty() {
            payload.insert(
                "verification_contracts".to_string(),
                serde_json::to_value(&prepared.verification_contracts).unwrap_or_default(),
            );
            payload.insert(
                "rendered_verification_context".to_string(),
                serde_json::json!(prepared.rendered_verification.clone()),
            );
        }
        payload.insert(
            "selected_artifact_ids".to_string(),
            serde_json::json!(prepared
                .selected
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "selected_artifact_titles".to_string(),
            serde_json::json!(prepared
                .selected
                .iter()
                .map(|artifact| artifact.title.clone())
                .collect::<Vec<_>>()),
        );
        match payload
            .entry("tools".to_string())
            .or_insert_with(|| serde_json::json!({}))
        {
            serde_json::Value::Object(tools) => {
                tools.insert(
                    "mode".to_string(),
                    serde_json::json!(prepared.tool_format.clone()),
                );
            }
            slot => {
                *slot = serde_json::json!({ "mode": prepared.tool_format.clone() });
            }
        }
    }

    let visible_text = llm_result["text"].as_str().unwrap_or_default().to_string();
    // Non-LLM stages (verify command, condition, fork, join, ...) don't produce
    // a "transcript" field; fall back to the input so cross-stage conversation
    // state survives transitions.
    let result_transcript = llm_result
        .get("transcript")
        .cloned()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    let session_transcript = crate::agent_sessions::snapshot(&prepared.stage_session_id);
    let transcript = result_transcript
        .or(session_transcript)
        .and_then(|value| redact_transcript_visibility(&value, node.output_visibility.as_deref()));
    let output_kind = node
        .output_contract
        .output_kinds
        .first()
        .cloned()
        .unwrap_or_else(|| {
            if node.kind == "verify" {
                "verification_result".to_string()
            } else {
                "artifact".to_string()
            }
        });
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "input_artifact_ids".to_string(),
        serde_json::json!(prepared
            .selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>()),
    );
    metadata.insert("node_kind".to_string(), serde_json::json!(node.kind));
    if !node.approval_policy.write_path_allowlist.is_empty() {
        metadata.insert(
            "changed_paths".to_string(),
            serde_json::json!(node.approval_policy.write_path_allowlist),
        );
    }
    let artifact = ArtifactRecord {
        type_name: "artifact".to_string(),
        id: new_id("artifact"),
        kind: output_kind,
        title: Some(format!("stage {node_id} output")),
        text: Some(visible_text),
        data: Some(llm_result.clone()),
        source: Some(node_id.to_string()),
        created_at: now_unix_seconds_text(),
        freshness: Some("fresh".to_string()),
        priority: None,
        lineage: prepared
            .selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata,
    }
    .normalize();

    Ok((llm_result, vec![artifact], transcript))
}

pub async fn execute_stage_node(
    ctx: &crate::vm::AsyncBuiltinCtx,
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let prepared = prepare_stage_node(ctx, node_id, node, task, artifacts).await?;
    let llm_result = if let Some(result) = prepared.result.clone() {
        result
    } else if prepared.run_agent_loop {
        let result = crate::stdlib::harn_entry::call_agent_loop(
            ctx,
            prepared.prompt.clone(),
            prepared.system.clone(),
            prepared.agent_loop_options.clone(),
        )
        .await?;
        crate::llm::vm_value_to_json(&result)
    } else {
        let args = vec![
            VmValue::String(arcstr::ArcStr::from(prepared.prompt.clone())),
            prepared
                .system
                .clone()
                .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
                .unwrap_or(VmValue::Nil),
            VmValue::dict(prepared.llm_options.clone()),
        ];
        let opts = extract_llm_options(&args)?;
        let result = vm_call_llm_full(&opts).await?;
        crate::llm::agent_loop_result_from_llm(&result, opts)
    };
    complete_prepared_stage_node(node_id, node, &prepared, llm_result)
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
        timestamp: now_unix_seconds_text(),
        reason,
        metadata,
    });
}
