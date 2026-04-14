//! Policy types and capability-ceiling enforcement.

mod types;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::thread_local;

use serde::{Deserialize, Serialize};

use super::{glob_match, new_id};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};
use crate::value::{VmError, VmValue};

pub use crate::tool_annotations::{ToolArgSchema, ToolKind};
pub use types::{
    enforce_tool_arg_constraints, BranchSemantics, CapabilityPolicy, ContextPolicy,
    EqIgnored, EscalationPolicy, JoinPolicy, MapPolicy, ModelPolicy, ReducePolicy, RetryPolicy,
    StageContract, ToolArgConstraint, TranscriptPolicy, TurnPolicy,
};

thread_local! {
    static EXECUTION_POLICY_STACK: RefCell<Vec<CapabilityPolicy>> = const { RefCell::new(Vec::new()) };
    static EXECUTION_APPROVAL_POLICY_STACK: RefCell<Vec<ToolApprovalPolicy>> = const { RefCell::new(Vec::new()) };
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

// ── Approval policy stack ───────────────────────────────────────────

pub fn push_approval_policy(policy: ToolApprovalPolicy) {
    EXECUTION_APPROVAL_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub fn pop_approval_policy() {
    EXECUTION_APPROVAL_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn current_approval_policy() -> Option<ToolApprovalPolicy> {
    EXECUTION_APPROVAL_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

pub fn current_tool_annotations(tool: &str) -> Option<ToolAnnotations> {
    current_execution_policy().and_then(|policy| policy.tool_annotations.get(tool).cloned())
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

pub(super) fn reject_policy(reason: String) -> Result<(), VmError> {
    Err(VmError::CategorizedError {
        message: reason,
        category: crate::value::ErrorCategory::ToolRejected,
    })
}

/// Mutation classification for a tool, derived from the pipeline's
/// declared `ToolKind`. Used in telemetry and pre/post-bridge payloads
/// while those methods still exist. Returns `"other"` for unannotated
/// tools (fail-safe; unknown tools don't auto-classify).
pub fn current_tool_mutation_classification(tool_name: &str) -> String {
    current_tool_annotations(tool_name)
        .map(|annotations| annotations.kind.mutation_class().to_string())
        .unwrap_or_else(|| "other".to_string())
}

/// Workspace paths declared by this tool call, read from the tool's
/// annotated `arg_schema.path_params`. Unannotated tools declare no
/// paths — the VM no longer guesses by common argument names.
pub fn current_tool_declared_paths(tool_name: &str, args: &serde_json::Value) -> Vec<String> {
    let Some(map) = args.as_object() else {
        return Vec::new();
    };
    let Some(annotations) = current_tool_annotations(tool_name) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for key in &annotations.arg_schema.path_params {
        if let Some(value) = map.get(key).and_then(|value| value.as_str()) {
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
        "read_file" => {
            if !policy_allows_capability(&policy, "workspace", "read_text") {
                return reject_policy(format!(
                    "builtin '{name}' exceeds workspace.read_text ceiling"
                ));
            }
        }
        "list_dir" => {
            if !policy_allows_capability(&policy, "workspace", "list") {
                return reject_policy(format!("builtin '{name}' exceeds workspace.list ceiling"));
            }
        }
        "file_exists" | "stat" => {
            if !policy_allows_capability(&policy, "workspace", "exists") {
                return reject_policy(format!("builtin '{name}' exceeds workspace.exists ceiling"));
            }
        }
        "write_file" | "append_file" | "mkdir" | "copy_file" => {
            if !policy_allows_capability(&policy, "workspace", "write_text")
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
        "exec" | "exec_at" | "shell" | "shell_at" => {
            if !policy_allows_capability(&policy, "process", "exec")
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
            if !policy_allows_capability(&policy, "process", "exec")
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
    if let Some(annotations) = policy.tool_annotations.get(tool_name) {
        for (capability, ops) in &annotations.capabilities {
            for op in ops {
                if !policy_allows_capability(&policy, capability, op) {
                    return reject_policy(format!(
                        "tool '{tool_name}' exceeds capability ceiling: {capability}.{op}"
                    ));
                }
            }
        }
        let requested_level = annotations.side_effect_level;
        if requested_level != SideEffectLevel::None
            && !policy_allows_side_effect(&policy, requested_level.as_str())
        {
            return reject_policy(format!(
                "tool '{tool_name}' exceeds side-effect ceiling: {}",
                requested_level.as_str()
            ));
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
        tool_annotations: BTreeMap::new(),
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
    /// Tool requires explicit host approval; the caller already owns the
    /// tool name and args and forwards them to the host bridge.
    RequiresHostApproval,
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
                return ToolApprovalDecision::RequiresHostApproval;
            }
        }

        // Default: auto-approve if no pattern matched.
        ToolApprovalDecision::AutoApproved
    }

    /// Merge two approval policies, taking the most restrictive combination.
    /// - auto_approve: only tools approved by BOTH policies stay approved
    ///   (if either policy has no patterns, the other's patterns are used)
    /// - auto_deny / require_approval: union (either policy can deny/gate)
    /// - write_path_allowlist: intersection (both must allow the path)
    pub fn intersect(&self, other: &ToolApprovalPolicy) -> ToolApprovalPolicy {
        // auto_approve: intersection semantics — a tool should only be
        // auto-approved if both policies agree. If one side has no patterns,
        // defer to the other.
        let auto_approve = if self.auto_approve.is_empty() {
            other.auto_approve.clone()
        } else if other.auto_approve.is_empty() {
            self.auto_approve.clone()
        } else {
            // Keep only patterns that appear in both lists.
            self.auto_approve
                .iter()
                .filter(|p| other.auto_approve.contains(p))
                .cloned()
                .collect()
        };
        // auto_deny / require_approval: union (more restrictive).
        let mut auto_deny = self.auto_deny.clone();
        auto_deny.extend(other.auto_deny.iter().cloned());
        let mut require_approval = self.require_approval.clone();
        require_approval.extend(other.require_approval.iter().cloned());
        // write_path_allowlist: intersection (both must allow the path).
        let write_path_allowlist = if self.write_path_allowlist.is_empty() {
            other.write_path_allowlist.clone()
        } else if other.write_path_allowlist.is_empty() {
            self.write_path_allowlist.clone()
        } else {
            self.write_path_allowlist
                .iter()
                .filter(|p| other.write_path_allowlist.contains(p))
                .cloned()
                .collect()
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
            ToolApprovalDecision::RequiresHostApproval
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

    #[test]
    fn intersect_restricts_auto_approve_to_common_patterns() {
        let a = ToolApprovalPolicy {
            auto_approve: vec!["read*".to_string(), "search*".to_string()],
            ..Default::default()
        };
        let b = ToolApprovalPolicy {
            auto_approve: vec!["read*".to_string(), "write*".to_string()],
            ..Default::default()
        };
        let merged = a.intersect(&b);
        // Only "read*" is in both — "search*" and "write*" dropped.
        assert_eq!(merged.auto_approve, vec!["read*".to_string()]);
    }

    #[test]
    fn intersect_defers_auto_approve_when_one_side_empty() {
        let a = ToolApprovalPolicy {
            auto_approve: vec!["read*".to_string()],
            ..Default::default()
        };
        let b = ToolApprovalPolicy::default();
        let merged = a.intersect(&b);
        assert_eq!(merged.auto_approve, vec!["read*".to_string()]);
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

#[cfg(test)]
mod transcript_policy_tests {
    use super::*;
    use crate::value::VmValue;

    fn mock_transcript(message_count: usize) -> VmValue {
        let messages: Vec<serde_json::Value> = (0..message_count)
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                serde_json::json!({"role": role, "content": format!("message {i}")})
            })
            .collect();
        crate::llm::helpers::transcript_to_vm_with_events(
            Some("test-id".to_string()),
            None,
            None,
            &messages,
            Vec::new(),
            Vec::new(),
            Some("active"),
        )
    }

    fn message_count(transcript: &VmValue) -> usize {
        transcript
            .as_dict()
            .and_then(|d| d.get("messages"))
            .and_then(|v| match v {
                VmValue::List(list) => Some(list.len()),
                _ => None,
            })
            .unwrap_or(0)
    }

    #[test]
    fn continue_mode_passes_transcript_through() {
        let transcript = mock_transcript(4);
        let policy = TranscriptPolicy {
            mode: Some("continue".to_string()),
            ..Default::default()
        };
        let result = apply_input_transcript_policy(Some(transcript), &policy);
        assert!(result.is_some());
        assert_eq!(message_count(&result.unwrap()), 4);
    }

    #[test]
    fn default_mode_passes_transcript_through() {
        let transcript = mock_transcript(3);
        let policy = TranscriptPolicy::default();
        let result = apply_input_transcript_policy(Some(transcript), &policy);
        assert!(result.is_some());
        assert_eq!(message_count(&result.unwrap()), 3);
    }

    #[test]
    fn reset_mode_clears_transcript() {
        let transcript = mock_transcript(4);
        let policy = TranscriptPolicy {
            mode: Some("reset".to_string()),
            ..Default::default()
        };
        let result = apply_input_transcript_policy(Some(transcript), &policy);
        assert!(result.is_none());
    }

    #[test]
    fn fork_mode_assigns_new_id() {
        let transcript = mock_transcript(3);
        let policy = TranscriptPolicy {
            mode: Some("fork".to_string()),
            ..Default::default()
        };
        let result = apply_input_transcript_policy(Some(transcript), &policy);
        let result = result.expect("fork should return a transcript");
        let dict = result.as_dict().expect("must be a dict");
        let id = dict.get("id").map(|v| v.display()).unwrap_or_default();
        assert_ne!(id, "test-id", "fork should assign a new transcript ID");
        assert_eq!(message_count(&result), 3, "fork should preserve messages");
    }

    #[test]
    fn none_input_stays_none_for_all_modes() {
        for mode in &["continue", "reset", "fork"] {
            let policy = TranscriptPolicy {
                mode: Some(mode.to_string()),
                ..Default::default()
            };
            let result = apply_input_transcript_policy(None, &policy);
            assert!(
                result.is_none(),
                "mode {mode} with None input should return None"
            );
        }
    }
}
