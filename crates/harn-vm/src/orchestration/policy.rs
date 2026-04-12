//! Policy types and capability-ceiling enforcement.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::thread_local;

use serde::{Deserialize, Serialize};

use super::{glob_match, new_id};
use crate::value::{VmError, VmValue};

thread_local! {
    static EXECUTION_POLICY_STACK: RefCell<Vec<CapabilityPolicy>> = const { RefCell::new(Vec::new()) };
}

// ── Per-agent policy with argument patterns ───────────────────────────

/// Extended policy that supports argument-level constraints.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolArgConstraint {
    /// Tool name to constrain.
    pub tool: String,
    /// Glob patterns that the first string argument must match.
    /// If empty, no argument constraint is applied.
    pub arg_patterns: Vec<String>,
}

/// Check if a tool call satisfies argument constraints in the policy.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), VmError> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) {
            continue;
        }
        if constraint.arg_patterns.is_empty() {
            continue;
        }
        let first_arg = args
            .as_object()
            .and_then(|o| {
                policy
                    .tool_metadata
                    .get(tool_name)
                    .into_iter()
                    .flat_map(|metadata| metadata.path_params.iter())
                    .find_map(|param| o.get(param).and_then(|v| v.as_str()))
                    .or_else(|| o.values().find_map(|v| v.as_str()))
            })
            .or_else(|| args.as_str())
            .unwrap_or("");
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, first_arg));
        if !matches {
            return reject_policy(format!(
                "tool '{tool_name}' argument '{first_arg}' does not match allowed patterns: {:?}",
                constraint.arg_patterns
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolRuntimePolicyMetadata {
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub side_effect_level: Option<String>,
    pub path_params: Vec<String>,
    pub mutation_classification: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapabilityPolicy {
    pub tools: Vec<String>,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub workspace_roots: Vec<String>,
    pub side_effect_level: Option<String>,
    pub recursion_limit: Option<usize>,
    /// Argument-level constraints for specific tools.
    #[serde(default)]
    pub tool_arg_constraints: Vec<ToolArgConstraint>,
    #[serde(default)]
    pub tool_metadata: BTreeMap<String, ToolRuntimePolicyMetadata>,
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

        // Merge arg constraints from both sides
        let mut tool_arg_constraints = self.tool_arg_constraints.clone();
        tool_arg_constraints.extend(requested.tool_arg_constraints.clone());

        let tool_metadata = tools
            .iter()
            .filter_map(|tool| {
                requested
                    .tool_metadata
                    .get(tool)
                    .or_else(|| self.tool_metadata.get(tool))
                    .cloned()
                    .map(|metadata| (tool.clone(), metadata))
            })
            .collect();

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            side_effect_level,
            recursion_limit,
            tool_arg_constraints,
            tool_metadata,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TurnPolicy {
    /// When true, text-only responses in a tool-capable stage are treated as
    /// invalid unless they switch phase / finish the stage. This keeps action
    /// stages moving instead of drifting into narration.
    pub require_action_or_yield: bool,
    /// When false, workflow-owned action stages should hand control back via
    /// successful tool calls instead of advertising an additional done
    /// sentinel pathway in corrective nudges.
    #[serde(default = "default_true")]
    pub allow_done_sentinel: bool,
    /// Optional visible prose budget for a single assistant turn. When the
    /// assistant exceeds it, the recorded transcript keeps only a shortened
    /// version and the next corrective nudge reminds the model to stay brief.
    pub max_prose_chars: Option<usize>,
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self {
            require_action_or_yield: false,
            allow_done_sentinel: true,
            max_prose_chars: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// Maximum agent_loop iterations for this stage. Overrides the default 16.
    pub max_iterations: Option<usize>,
    /// Maximum consecutive text-only (no tool call) responses before declaring stuck.
    pub max_nudges: Option<usize>,
    /// Custom nudge message injected when the model produces text without tool calls.
    /// If omitted, the VM uses a generic "Continue — use a tool call" message.
    pub nudge: Option<String>,
    /// Few-shot tool-call examples injected into the tool contract prompt,
    /// shown before the tool schema listing. Pipelines provide these —
    /// the VM has no hardcoded tool names.
    pub tool_examples: Option<String>,
    /// Optional Harn closure called after each tool-calling turn.
    /// Receives turn metadata; returns either a string user message to inject,
    /// a bool stop flag, or a dict like {message, stop}.
    /// Wrapped in EqIgnored so it doesn't affect PartialEq derivation.
    #[serde(skip)]
    pub post_turn_callback: Option<EqIgnored<VmValue>>,
    /// When set, the stage stops after any tool-calling turn whose successful
    /// results include one of these tool names. This is useful for
    /// workflow-owned verify loops where a productive write turn should hand
    /// control back to verification immediately.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// When set, the stage is reported as failed unless at least one of these
    /// tool names succeeds during the interaction. Pipelines use this to
    /// assert a stage cannot quietly finish without running a specific tool.
    pub require_successful_tools: Option<Vec<String>>,
    /// Turn-shape constraints for action stages.
    pub turn_policy: Option<TurnPolicy>,
}

/// Wrapper that always compares equal, allowing non-Eq types in derived PartialEq structs.
#[derive(Clone, Debug, Default)]
pub struct EqIgnored<T>(pub T);

impl<T> PartialEq for EqIgnored<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<T> std::ops::Deref for EqIgnored<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptPolicy {
    pub mode: Option<String>,
    pub visibility: Option<String>,
    pub summarize: bool,
    pub compact: bool,
    pub keep_last: Option<usize>,
    /// Enable per-turn auto-compaction within agent loops.
    pub auto_compact: bool,
    /// Token threshold for tier-1 compaction.
    pub compact_threshold: Option<usize>,
    /// Max chars per tool result before compression.
    pub tool_output_max_chars: Option<usize>,
    /// Tier-1 compaction strategy name (e.g., "observation_mask", "llm").
    pub compact_strategy: Option<String>,
    /// Token threshold for tier-2 aggressive compaction.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 compaction strategy name.
    pub hard_limit_strategy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPolicy {
    pub max_artifacts: Option<usize>,
    pub max_tokens: Option<usize>,
    pub reserve_tokens: Option<usize>,
    pub include_kinds: Vec<String>,
    pub exclude_kinds: Vec<String>,
    pub prioritize_kinds: Vec<String>,
    pub pinned_ids: Vec<String>,
    pub include_stages: Vec<String>,
    pub prefer_recent: bool,
    pub prefer_fresh: bool,
    pub render: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
    /// Initial backoff duration in milliseconds between retry attempts.
    /// When `None`, retries proceed without delay.
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    /// Multiplier applied to `backoff_ms` after each retry attempt.
    /// Defaults to 2.0 when `backoff_ms` is set and this field is `None`.
    #[serde(default)]
    pub backoff_multiplier: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StageContract {
    pub input_kinds: Vec<String>,
    pub output_kinds: Vec<String>,
    pub min_inputs: Option<usize>,
    pub max_inputs: Option<usize>,
    pub require_transcript: bool,
    pub schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchSemantics {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub verify_pass: Option<String>,
    pub verify_fail: Option<String>,
    pub condition_true: Option<String>,
    pub condition_false: Option<String>,
    pub loop_continue: Option<String>,
    pub loop_exit: Option<String>,
    pub escalation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MapPolicy {
    pub items: Vec<serde_json::Value>,
    pub item_artifact_kind: Option<String>,
    pub output_kind: Option<String>,
    pub max_items: Option<usize>,
    pub max_concurrent: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JoinPolicy {
    pub strategy: String,
    pub require_all_inputs: bool,
    pub min_completed: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReducePolicy {
    pub strategy: String,
    pub separator: Option<String>,
    pub output_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EscalationPolicy {
    pub level: Option<String>,
    pub queue: Option<String>,
    pub reason: Option<String>,
}

// ── Execution policy stack ──────────────────────────────────────────

pub fn push_execution_policy(policy: CapabilityPolicy) {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub fn pop_execution_policy() {
    EXECUTION_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn current_execution_policy() -> Option<CapabilityPolicy> {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

pub fn current_tool_metadata(tool: &str) -> Option<ToolRuntimePolicyMetadata> {
    current_execution_policy().and_then(|policy| policy.tool_metadata.get(tool).cloned())
}

fn policy_allows_tool(policy: &CapabilityPolicy, tool: &str) -> bool {
    policy.tools.is_empty() || policy.tools.iter().any(|allowed| allowed == tool)
}

fn policy_allows_capability(policy: &CapabilityPolicy, capability: &str, op: &str) -> bool {
    policy.capabilities.is_empty()
        || policy
            .capabilities
            .get(capability)
            .is_some_and(|ops| ops.is_empty() || ops.iter().any(|allowed| allowed == op))
}

fn policy_allows_side_effect(policy: &CapabilityPolicy, requested: &str) -> bool {
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
    policy
        .side_effect_level
        .as_ref()
        .map(|allowed| rank(allowed) >= rank(requested))
        .unwrap_or(true)
}

fn reject_policy(reason: String) -> Result<(), VmError> {
    Err(VmError::CategorizedError {
        message: reason,
        category: crate::value::ErrorCategory::ToolRejected,
    })
}

fn fallback_mutation_classification(tool_name: &str) -> String {
    let lower = tool_name.to_ascii_lowercase();
    if lower.starts_with("mcp_") {
        return "host_defined".to_string();
    }
    if lower == "exec"
        || lower == "shell"
        || lower == "exec_at"
        || lower == "shell_at"
        || lower == "run"
        || lower.starts_with("run_")
    {
        return "ambient_side_effect".to_string();
    }
    if lower.starts_with("delete")
        || lower.starts_with("remove")
        || lower.starts_with("move")
        || lower.starts_with("rename")
    {
        return "destructive".to_string();
    }
    if lower.contains("write")
        || lower.contains("edit")
        || lower.contains("patch")
        || lower.contains("create")
        || lower.contains("scaffold")
        || lower.starts_with("insert")
        || lower.starts_with("replace")
        || lower == "add_import"
    {
        return "apply_workspace".to_string();
    }
    "read_only".to_string()
}

pub fn current_tool_mutation_classification(tool_name: &str) -> String {
    current_tool_metadata(tool_name)
        .and_then(|metadata| metadata.mutation_classification)
        .unwrap_or_else(|| fallback_mutation_classification(tool_name))
}

pub fn current_tool_declared_paths(tool_name: &str, args: &serde_json::Value) -> Vec<String> {
    let Some(map) = args.as_object() else {
        return Vec::new();
    };
    let path_keys = current_tool_metadata(tool_name)
        .map(|metadata| metadata.path_params)
        .filter(|keys| !keys.is_empty())
        .unwrap_or_else(|| {
            vec![
                "path".to_string(),
                "file".to_string(),
                "cwd".to_string(),
                "repo".to_string(),
                "target".to_string(),
                "destination".to_string(),
            ]
        });
    let mut paths = Vec::new();
    for key in path_keys {
        if let Some(value) = map.get(&key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                paths.push(value.to_string());
            }
        }
    }
    if let Some(items) = map.get("paths").and_then(|value| value.as_array()) {
        for item in items {
            if let Some(value) = item.as_str() {
                if !value.is_empty() {
                    paths.push(value.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

pub fn enforce_current_policy_for_builtin(name: &str, args: &[VmValue]) -> Result<(), VmError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    match name {
        "read" | "read_file" => {
            if !policy_allows_tool(&policy, name)
                || !policy_allows_capability(&policy, "workspace", "read_text")
            {
                return reject_policy(format!(
                    "builtin '{name}' exceeds workspace.read_text ceiling"
                ));
            }
        }
        "search" | "list_dir" => {
            if !policy_allows_tool(&policy, name)
                || !policy_allows_capability(&policy, "workspace", "list")
            {
                return reject_policy(format!("builtin '{name}' exceeds workspace.list ceiling"));
            }
        }
        "file_exists" | "stat" => {
            if !policy_allows_capability(&policy, "workspace", "exists") {
                return reject_policy(format!("builtin '{name}' exceeds workspace.exists ceiling"));
            }
        }
        "edit" | "write_file" | "append_file" | "mkdir" | "copy_file" => {
            if !policy_allows_tool(&policy, "edit")
                || !policy_allows_capability(&policy, "workspace", "write_text")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(format!("builtin '{name}' exceeds workspace write ceiling"));
            }
        }
        "delete_file" => {
            if !policy_allows_capability(&policy, "workspace", "delete")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(
                    "builtin 'delete_file' exceeds workspace.delete ceiling".to_string(),
                );
            }
        }
        "apply_edit" => {
            if !policy_allows_capability(&policy, "workspace", "apply_edit")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(
                    "builtin 'apply_edit' exceeds workspace.apply_edit ceiling".to_string(),
                );
            }
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "run_command" => {
            if !policy_allows_tool(&policy, "run")
                || !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec")
            {
                return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
            }
        }
        "http_get" | "http_post" | "http_put" | "http_patch" | "http_delete" | "http_request" => {
            if !policy_allows_side_effect(&policy, "network") {
                return reject_policy(format!("builtin '{name}' exceeds network ceiling"));
            }
        }
        "mcp_connect"
        | "mcp_call"
        | "mcp_list_tools"
        | "mcp_list_resources"
        | "mcp_list_resource_templates"
        | "mcp_read_resource"
        | "mcp_list_prompts"
        | "mcp_get_prompt"
        | "mcp_server_info"
        | "mcp_disconnect" => {
            if !policy_allows_tool(&policy, "run")
                || !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec")
            {
                return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
            }
        }
        "host_call" => {
            let name = args.first().map(|v| v.display()).unwrap_or_default();
            let Some((capability, op)) = name.split_once('.') else {
                return reject_policy(format!(
                    "host_call '{name}' must use capability.operation naming"
                ));
            };
            if !policy_allows_capability(&policy, capability, op) {
                return reject_policy(format!(
                    "host_call {capability}.{op} exceeds capability ceiling"
                ));
            }
            let requested_side_effect = match (capability, op) {
                ("workspace", "write_text" | "apply_edit" | "delete") => "workspace_write",
                ("process", "exec") => "process_exec",
                _ => "read_only",
            };
            if !policy_allows_side_effect(&policy, requested_side_effect) {
                return reject_policy(format!(
                    "host_call {capability}.{op} exceeds side-effect ceiling"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn enforce_current_policy_for_bridge_builtin(name: &str) -> Result<(), VmError> {
    if current_execution_policy().is_some() {
        return reject_policy(format!(
            "bridged builtin '{name}' exceeds execution policy; declare an explicit capability/tool surface instead"
        ));
    }
    Ok(())
}

pub fn enforce_current_policy_for_tool(tool_name: &str) -> Result<(), VmError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    if !policy_allows_tool(&policy, tool_name) {
        return reject_policy(format!("tool '{tool_name}' exceeds tool ceiling"));
    }
    if let Some(metadata) = policy.tool_metadata.get(tool_name) {
        for (capability, ops) in &metadata.capabilities {
            for op in ops {
                if !policy_allows_capability(&policy, capability, op) {
                    return reject_policy(format!(
                        "tool '{tool_name}' exceeds capability ceiling: {capability}.{op}"
                    ));
                }
            }
        }
        if let Some(side_effect_level) = metadata.side_effect_level.as_deref() {
            if !policy_allows_side_effect(&policy, side_effect_level) {
                return reject_policy(format!(
                    "tool '{tool_name}' exceeds side-effect ceiling: {side_effect_level}"
                ));
            }
        }
    }
    Ok(())
}

// ── Transcript policy helpers ───────────────────────────────────────

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

pub(crate) fn apply_input_transcript_policy(
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

pub(crate) fn apply_output_transcript_policy(
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

pub fn builtin_ceiling() -> CapabilityPolicy {
    CapabilityPolicy {
        // Capabilities left empty — the host capability manifest is the sole
        // authority on which operations are available.  An explicit allowlist
        // here would silently block any capability the host adds later.
        tools: Vec::new(),
        capabilities: BTreeMap::new(),
        workspace_roots: Vec::new(),
        side_effect_level: Some("network".to_string()),
        recursion_limit: Some(8),
        tool_arg_constraints: Vec::new(),
        tool_metadata: BTreeMap::new(),
    }
}

// ── Tool approval policy ─────────────────────────────────────────────

/// Declarative policy for tool approval gating. Allows pipelines to
/// specify which tools are auto-approved, auto-denied, or require
/// host confirmation, plus write-path allowlists.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolApprovalPolicy {
    /// Glob patterns for tools that should be auto-approved.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// Glob patterns for tools that should always be denied.
    #[serde(default)]
    pub auto_deny: Vec<String>,
    /// Glob patterns for tools that require host confirmation.
    #[serde(default)]
    pub require_approval: Vec<String>,
    /// Glob patterns for writable paths.
    #[serde(default)]
    pub write_path_allowlist: Vec<String>,
}

/// Result of evaluating a tool call against a ToolApprovalPolicy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolApprovalDecision {
    /// Tool is auto-approved by policy.
    AutoApproved,
    /// Tool is auto-denied by policy.
    AutoDenied { reason: String },
    /// Tool requires explicit host approval.
    RequiresHostApproval { tool: String, args: serde_json::Value },
}

impl ToolApprovalPolicy {
    /// Evaluate whether a tool call should be approved, denied, or needs
    /// host confirmation.
    pub fn evaluate(&self, tool_name: &str, args: &serde_json::Value) -> ToolApprovalDecision {
        // Auto-deny takes precedence.
        for pattern in &self.auto_deny {
            if glob_match(pattern, tool_name) {
                return ToolApprovalDecision::AutoDenied {
                    reason: format!("tool '{tool_name}' matches deny pattern '{pattern}'"),
                };
            }
        }

        // Check write-path allowlist for tools that declare paths.
        if !self.write_path_allowlist.is_empty() {
            let paths = super::current_tool_declared_paths(tool_name, args);
            for path in &paths {
                let allowed = self
                    .write_path_allowlist
                    .iter()
                    .any(|pattern| glob_match(pattern, path));
                if !allowed {
                    return ToolApprovalDecision::AutoDenied {
                        reason: format!(
                            "tool '{tool_name}' writes to '{path}' which is not in the write-path allowlist"
                        ),
                    };
                }
            }
        }

        // Auto-approve.
        for pattern in &self.auto_approve {
            if glob_match(pattern, tool_name) {
                return ToolApprovalDecision::AutoApproved;
            }
        }

        // Require approval.
        for pattern in &self.require_approval {
            if glob_match(pattern, tool_name) {
                return ToolApprovalDecision::RequiresHostApproval {
                    tool: tool_name.to_string(),
                    args: args.clone(),
                };
            }
        }

        // Default: auto-approve if no pattern matched.
        ToolApprovalDecision::AutoApproved
    }

    /// Merge two approval policies, taking the most restrictive combination.
    pub fn intersect(&self, other: &ToolApprovalPolicy) -> ToolApprovalPolicy {
        let mut auto_approve = self.auto_approve.clone();
        auto_approve.extend(other.auto_approve.iter().cloned());
        let mut auto_deny = self.auto_deny.clone();
        auto_deny.extend(other.auto_deny.iter().cloned());
        let mut require_approval = self.require_approval.clone();
        require_approval.extend(other.require_approval.iter().cloned());
        // Write-path allowlist: intersection (both must allow).
        let write_path_allowlist = if self.write_path_allowlist.is_empty() {
            other.write_path_allowlist.clone()
        } else if other.write_path_allowlist.is_empty() {
            self.write_path_allowlist.clone()
        } else {
            // Keep patterns from both sides — actual path checking
            // requires all patterns to match, but we merge the lists
            // so the evaluation can check against both sets.
            let mut merged = self.write_path_allowlist.clone();
            merged.extend(other.write_path_allowlist.iter().cloned());
            merged
        };
        ToolApprovalPolicy {
            auto_approve,
            auto_deny,
            require_approval,
            write_path_allowlist,
        }
    }
}

#[cfg(test)]
mod approval_policy_tests {
    use super::*;

    #[test]
    fn auto_deny_takes_precedence_over_auto_approve() {
        let policy = ToolApprovalPolicy {
            auto_approve: vec!["*".to_string()],
            auto_deny: vec!["dangerous_*".to_string()],
            ..Default::default()
        };
        assert_eq!(
            policy.evaluate("dangerous_rm", &serde_json::json!({})),
            ToolApprovalDecision::AutoDenied {
                reason: "tool 'dangerous_rm' matches deny pattern 'dangerous_*'".to_string()
            }
        );
    }

    #[test]
    fn auto_approve_matches_glob() {
        let policy = ToolApprovalPolicy {
            auto_approve: vec!["read*".to_string(), "search*".to_string()],
            ..Default::default()
        };
        assert_eq!(
            policy.evaluate("read_file", &serde_json::json!({})),
            ToolApprovalDecision::AutoApproved
        );
        assert_eq!(
            policy.evaluate("search", &serde_json::json!({})),
            ToolApprovalDecision::AutoApproved
        );
    }

    #[test]
    fn require_approval_emits_decision() {
        let policy = ToolApprovalPolicy {
            require_approval: vec!["edit*".to_string()],
            ..Default::default()
        };
        let decision = policy.evaluate("edit_file", &serde_json::json!({"path": "foo.rs"}));
        assert!(matches!(
            decision,
            ToolApprovalDecision::RequiresHostApproval { .. }
        ));
    }

    #[test]
    fn unmatched_tool_defaults_to_approved() {
        let policy = ToolApprovalPolicy {
            auto_approve: vec!["read*".to_string()],
            require_approval: vec!["edit*".to_string()],
            ..Default::default()
        };
        assert_eq!(
            policy.evaluate("unknown_tool", &serde_json::json!({})),
            ToolApprovalDecision::AutoApproved
        );
    }

    #[test]
    fn intersect_merges_deny_lists() {
        let a = ToolApprovalPolicy {
            auto_deny: vec!["rm*".to_string()],
            ..Default::default()
        };
        let b = ToolApprovalPolicy {
            auto_deny: vec!["drop*".to_string()],
            ..Default::default()
        };
        let merged = a.intersect(&b);
        assert_eq!(merged.auto_deny.len(), 2);
    }
}

#[cfg(test)]
mod turn_policy_tests {
    use super::TurnPolicy;

    #[test]
    fn default_allows_done_sentinel() {
        let policy = TurnPolicy::default();
        assert!(policy.allow_done_sentinel);
        assert!(!policy.require_action_or_yield);
        assert!(policy.max_prose_chars.is_none());
    }

    #[test]
    fn deserializing_partial_dict_preserves_done_sentinel_pathway() {
        // Pre-existing workflows passed `turn_policy: { require_action_or_yield: true }`
        // without knowing about `allow_done_sentinel`. Deserializing such a dict
        // must keep the done-sentinel pathway enabled so persistent agent loops
        // don't lose their completion signal in this release.
        let policy: TurnPolicy =
            serde_json::from_value(serde_json::json!({ "require_action_or_yield": true }))
                .expect("deserialize");
        assert!(policy.require_action_or_yield);
        assert!(policy.allow_done_sentinel);
    }

    #[test]
    fn deserializing_explicit_false_disables_done_sentinel() {
        let policy: TurnPolicy = serde_json::from_value(serde_json::json!({
            "require_action_or_yield": true,
            "allow_done_sentinel": false,
        }))
        .expect("deserialize");
        assert!(policy.require_action_or_yield);
        assert!(!policy.allow_done_sentinel);
    }
}
