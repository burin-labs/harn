//! Policy types and capability-ceiling enforcement.

mod approval_rules;
mod effects;
mod nested_budget;
mod types;

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::thread_local;

use serde::{Deserialize, Serialize};

use crate::runtime_limits::RuntimeLimits;
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};
use crate::value::{VmError, VmValue};
use crate::workspace_path::{classify_workspace_path, WorkspacePathInfo};

pub use crate::tool_annotations::{ToolArgSchema, ToolKind};
pub use approval_rules::{
    clear_all_approval_policy_repeat_counts, clear_approval_policy_repeat_counts,
    next_approval_policy_repeat_count, ApprovalShape, PolicyAction, PolicyEvaluation,
    PolicyMatchedRule, PolicyRule, PolicyRuleMatch,
};
pub use effects::{
    compute_handoff_effects, effect_kind_label, effect_record_summary, effect_subset_violations,
    effects_from_metadata, EffectKind, EffectRecord, EffectScope,
};
pub use nested_budget::{
    annotate_nested_execution_options, enter_nested_execution_policy, NestedExecutionGuard,
    NestedExecutionKind, NESTED_KIND_OPTION_KEY, NESTED_LABEL_OPTION_KEY,
};
pub use types::{
    enforce_tool_arg_constraints, AutoCompactPolicy, BranchSemantics, CapabilityPolicy,
    ContextPolicy, EqIgnored, EscalationPolicy, FeedbackBounds, FeedbackPolicy, JoinPolicy,
    MapPolicy, ModelPolicy, NativeToolFallbackPolicy, ProcessSandboxPolicy, ProcessSandboxPreset,
    ReducePolicy, RetryPolicy, SandboxProfile, StageContract, ToolArgConstraint, TurnPolicy,
};

thread_local! {
    static EXECUTION_POLICY_STACK: RefCell<Vec<CapabilityPolicy>> = const { RefCell::new(Vec::new()) };
    static EXECUTION_APPROVAL_POLICY_STACK: RefCell<Vec<ToolApprovalPolicy>> = const { RefCell::new(Vec::new()) };
    static TRUSTED_BRIDGE_CALL_DEPTH: RefCell<usize> = const { RefCell::new(0) };
}

pub fn push_execution_policy(policy: CapabilityPolicy) {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub fn pop_execution_policy() {
    EXECUTION_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn clear_execution_policy_stacks() {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow_mut().clear());
    EXECUTION_APPROVAL_POLICY_STACK.with(|stack| stack.borrow_mut().clear());
    TRUSTED_BRIDGE_CALL_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

pub fn current_execution_policy() -> Option<CapabilityPolicy> {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

/// O(1) probe for whether any execution policy scope is active on this
/// thread/task. Lets hot paths (tool dispatch) skip policy enforcement
/// entirely without paying the `CapabilityPolicy` clone that
/// [`current_execution_policy`] performs.
pub fn execution_policy_active() -> bool {
    EXECUTION_POLICY_STACK.with(|stack| !stack.borrow().is_empty())
}

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

// --- Per-task ambient-scope swap primitives -------------------------------
//
// The policy/approval/trusted stacks are thread-locals managed as LIFO scopes.
// That invariant holds for a single synchronous call stack, but a guard held
// across an `.await` is unsound: under `spawn_local` (and any work-stealing
// multi-thread executor) a sibling task interleaves and reads/mutates the same
// thread-local top-of-stack. `AmbientExecutionScope` (see `ambient_scope`)
// gives each spawned worker its own scope by swapping these stacks in on
// poll-enter and back out on poll-exit; these `swap_*` helpers are the O(1)
// primitives it uses. They are intentionally `pub(crate)` — only the ambient
// combinator should move whole stacks; ordinary code uses push/pop/current.

pub(crate) fn swap_execution_policy_stack(next: Vec<CapabilityPolicy>) -> Vec<CapabilityPolicy> {
    EXECUTION_POLICY_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

pub(crate) fn swap_approval_policy_stack(next: Vec<ToolApprovalPolicy>) -> Vec<ToolApprovalPolicy> {
    EXECUTION_APPROVAL_POLICY_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

pub(crate) fn swap_trusted_bridge_depth(next: usize) -> usize {
    TRUSTED_BRIDGE_CALL_DEPTH.with(|depth| std::mem::replace(&mut *depth.borrow_mut(), next))
}

pub fn current_tool_annotations(tool: &str) -> Option<ToolAnnotations> {
    current_execution_policy().and_then(|policy| policy.tool_annotations.get(tool).cloned())
}

/// The explicit tool allowlist the active execution policy advertises, for
/// building actionable denial feedback that names what the model *can* call.
///
/// Prefers `policy.tools` (the explicit ceiling — what the eval lane sets);
/// falls back to the annotation registry keys when no explicit list is present.
/// Returns an empty `Vec` when no policy is active or the surface is unbounded
/// (allow-all), in which case callers keep their generic guidance.
pub fn current_allowed_tool_names() -> Vec<String> {
    let Some(policy) = current_execution_policy() else {
        return Vec::new();
    };
    if !policy.tools.is_empty() {
        return policy.tools;
    }
    policy.tool_annotations.keys().cloned().collect()
}

pub(super) fn tool_kind_participates_in_write_allowlist(tool_name: &str) -> bool {
    current_tool_annotations(tool_name)
        .map(|annotations| !annotations.kind.is_read_only())
        .unwrap_or(true)
}

pub struct TrustedBridgeCallGuard;

pub fn allow_trusted_bridge_calls() -> TrustedBridgeCallGuard {
    TRUSTED_BRIDGE_CALL_DEPTH.with(|depth| {
        *depth.borrow_mut() += 1;
    });
    TrustedBridgeCallGuard
}

impl Drop for TrustedBridgeCallGuard {
    fn drop(&mut self) {
        TRUSTED_BRIDGE_CALL_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

fn policy_allows_tool(policy: &CapabilityPolicy, tool: &str) -> bool {
    policy.tools.is_empty() || policy.tools.iter().any(|allowed| allowed == tool)
}

fn policy_grants_capability(policy: &CapabilityPolicy, capability: &str, op: &str) -> bool {
    policy
        .capabilities
        .get(capability)
        .is_some_and(|ops| ops.is_empty() || ops.iter().any(|allowed| allowed == op))
}

fn policy_allows_capability(policy: &CapabilityPolicy, capability: &str, op: &str) -> bool {
    if policy.capabilities.is_empty() {
        // Empty capability map = allow-all (e.g. the root agent policy).
        return true;
    }
    if policy_grants_capability(policy, capability, op) {
        return true;
    }
    // Capability subsumption: a stronger read grant implies the weaker
    // observations it already exposes. An existence/metadata probe
    // (`workspace.exists`, used by `file_exists`/`stat`) reveals strictly less
    // than reading file contents (`workspace.read_text`) or listing a directory
    // (`workspace.list`) — both of which already disclose whether a path
    // exists. A policy that grants read/list but withholds the existence probe
    // is incoherent, and silently wedges any tool that stats a path before
    // reading it (look, read_file, edit/scaffold preflight). Narrowed worker
    // policies derived from tool annotations hit this constantly because no
    // annotation declares `workspace.exists`. Encode the lattice once here so
    // every narrowed policy benefits, not one dispatch surface at a time.
    if capability == "workspace" && op == "exists" {
        return policy_grants_capability(policy, "workspace", "read_text")
            || policy_grants_capability(policy, "workspace", "list");
    }
    false
}

fn policy_allows_side_effect(policy: &CapabilityPolicy, requested: &str) -> bool {
    // Rank through the canonical `SideEffectLevel` ladder (single source of
    // truth). `requested` always comes from a typed `SideEffectLevel::as_str()`,
    // so it is a known value; a typo'd policy ceiling ranks as `none` (0),
    // conservatively granting nothing above `none` rather than the previous
    // `_ => 5` that silently allowed everything.
    let requested_rank = SideEffectLevel::rank_str(requested);
    policy
        .side_effect_level
        .as_ref()
        .map(|allowed| SideEffectLevel::rank_str(allowed) >= requested_rank)
        .unwrap_or(true)
}

pub(super) fn reject_policy(reason: String) -> Result<(), VmError> {
    Err(VmError::CategorizedError {
        message: reason,
        category: crate::value::ErrorCategory::ToolRejected,
    })
}

/// Structured refusal produced by the agent-tool capability gates
/// (`enforce_current_policy_for_tool`, `enforce_tool_arg_constraints`).
/// Records the gate identity and the exceeded capability so the dispatch
/// boundary can build a full [`crate::agent_events::ToolDenial`] for the
/// model and host. `From<PolicyDenial> for VmError` keeps the legacy
/// `?`-using callers — which only need the categorized error — unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDenial {
    pub gate: crate::agent_events::DenialGate,
    pub capability: Option<String>,
    pub reason: String,
}

impl From<PolicyDenial> for VmError {
    fn from(denial: PolicyDenial) -> Self {
        VmError::CategorizedError {
            message: denial.reason,
            category: crate::value::ErrorCategory::ToolRejected,
        }
    }
}

pub(super) fn reject_tool(
    gate: crate::agent_events::DenialGate,
    capability: Option<String>,
    reason: String,
) -> Result<(), PolicyDenial> {
    Err(PolicyDenial {
        gate,
        capability,
        reason,
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
    current_tool_declared_path_entries(tool_name, args)
        .into_iter()
        .map(|entry| entry.display_path().to_string())
        .collect()
}

/// Rich workspace-path descriptors declared by this tool call. Each
/// entry preserves the original input while also projecting the path
/// into workspace-relative and host-absolute forms when that mapping is
/// known.
pub fn current_tool_declared_path_entries(
    tool_name: &str,
    args: &serde_json::Value,
) -> Vec<WorkspacePathInfo> {
    let Some(map) = args.as_object() else {
        return Vec::new();
    };
    let Some(annotations) = current_tool_annotations(tool_name) else {
        return Vec::new();
    };
    let workspace_root = crate::stdlib::process::execution_root_path();
    let mut entries = Vec::new();
    for key in &annotations.arg_schema.path_params {
        if let Some(value) = map.get(key) {
            match value {
                serde_json::Value::String(path) if !path.is_empty() => {
                    entries.push(classify_workspace_path(path, Some(&workspace_root)));
                }
                serde_json::Value::Array(items) => {
                    for item in items.iter().filter_map(|item| item.as_str()) {
                        if !item.is_empty() {
                            entries.push(classify_workspace_path(item, Some(&workspace_root)));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    entries.sort_by(|a, b| a.display_path().cmp(b.display_path()));
    entries.dedup_by(|left, right| left.policy_candidates() == right.policy_candidates());
    entries
}

pub fn enforce_current_policy_for_builtin(name: &str, args: &[VmValue]) -> Result<(), VmError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    match name {
        "find_text"
            if !policy_allows_capability(&policy, "workspace", "read_text")
                || !policy_allows_capability(&policy, "workspace", "list") =>
        {
            return reject_policy(
                "builtin 'find_text' exceeds workspace.read_text/workspace.list ceiling"
                    .to_string(),
            );
        }
        "read_file"
        | "read_file_result"
        | "read_file_bytes"
        | "render"
        | "render_prompt"
        | "render_with_provenance"
        | "read_lines"
            if !policy_allows_capability(&policy, "workspace", "read_text") =>
        {
            return reject_policy(format!(
                "builtin '{name}' exceeds workspace.read_text ceiling"
            ));
        }
        "list_dir" | "walk_dir" | "glob"
            if !policy_allows_capability(&policy, "workspace", "list") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds workspace.list ceiling"));
        }
        "file_exists" | "stat" if !policy_allows_capability(&policy, "workspace", "exists") => {
            return reject_policy(format!("builtin '{name}' exceeds workspace.exists ceiling"));
        }
        "write_file" | "write_file_bytes" | "append_file" | "mkdir" | "copy_file" | "move_file"
            if !policy_allows_capability(&policy, "workspace", "write_text")
                || !policy_allows_side_effect(&policy, "workspace_write") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds workspace write ceiling"));
        }
        "delete_file"
            if !policy_allows_capability(&policy, "workspace", "delete")
                || !policy_allows_side_effect(&policy, "workspace_write") =>
        {
            return reject_policy(
                "builtin 'delete_file' exceeds workspace.delete ceiling".to_string(),
            );
        }
        "apply_edit"
            if !policy_allows_capability(&policy, "workspace", "apply_edit")
                || !policy_allows_side_effect(&policy, "workspace_write") =>
        {
            return reject_policy(
                "builtin 'apply_edit' exceeds workspace.apply_edit ceiling".to_string(),
            );
        }
        "exec"
        | "exec_at"
        | "shell"
        | "shell_at"
        | "git.repo.discover"
        | "git.worktree.create"
        | "git.worktree.remove"
        | "git.fetch"
        | "git.rebase"
        | "git.status"
        | "git.conflicts"
        | "git.push"
        | "git.diff"
        | "git.merge_base"
            if !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
        }
        "http_get"
        | "http_post"
        | "http_put"
        | "http_patch"
        | "http_delete"
        | "http_download"
        | "http_request"
        | "unix_socket_json_request"
        | "__net_unix_socket_json_request"
            if !policy_allows_side_effect(&policy, "network") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds network ceiling"));
        }
        "__files_upload"
            if !policy_allows_capability(&policy, "workspace", "read_text")
                || !policy_allows_side_effect(&policy, "network") =>
        {
            return reject_policy(
                "builtin '__files_upload' exceeds workspace.read_text/network ceiling".to_string(),
            );
        }
        "http_session_request"
        | "http_stream_open"
        | "http_stream_read"
        | "http_stream_close"
        | "http_stream_info"
        | "sse_connect"
        | "sse_receive"
        | "websocket_accept"
        | "websocket_connect"
        | "websocket_route"
        | "websocket_send"
        | "websocket_receive"
        | "websocket_server"
            if !policy_allows_side_effect(&policy, "network") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds network ceiling"));
        }
        "llm_call" | "llm_call_safe" | "llm_completion" | "llm_stream" | "llm_stream_call"
        | "llm_healthcheck" | "agent_loop"
            if !policy_allows_capability(&policy, "llm", "call") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds llm.call ceiling"));
        }
        "connector_call"
            if !policy_allows_capability(&policy, "connector", "call")
                || !policy_allows_side_effect(&policy, "network") =>
        {
            return reject_policy(
                "builtin 'connector_call' exceeds connector.call/network ceiling".to_string(),
            );
        }
        "secret_get" if !policy_allows_capability(&policy, "connector", "secret_get") => {
            return reject_policy(
                "builtin 'secret_get' exceeds connector.secret_get ceiling".to_string(),
            );
        }
        "event_log_emit" if !policy_allows_capability(&policy, "connector", "event_log_emit") => {
            return reject_policy(
                "builtin 'event_log_emit' exceeds connector.event_log_emit ceiling".to_string(),
            );
        }
        "metrics_inc" if !policy_allows_capability(&policy, "connector", "metrics_inc") => {
            return reject_policy(
                "builtin 'metrics_inc' exceeds connector.metrics_inc ceiling".to_string(),
            );
        }
        "project_fingerprint"
        | "project_context_profile_native"
        | "project_scan_native"
        | "project_scan_tree_native"
        | "project_walk_tree_native"
        | "project_catalog_native"
            if !policy_allows_capability(&policy, "workspace", "list")
                || !policy_allows_side_effect(&policy, "read_only") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds workspace.list ceiling"));
        }
        "__agent_state_init"
        | "__agent_state_resume"
        | "__agent_state_write"
        | "__agent_state_read"
        | "__agent_state_list"
        | "__agent_state_delete"
        | "__agent_state_handoff"
            if !policy_allows_capability(&policy, "agent_state", "access") =>
        {
            return reject_policy(format!(
                "builtin '{name}' exceeds agent_state.access ceiling"
            ));
        }
        "vision_ocr"
            if !policy_allows_capability(&policy, "vision", "ocr")
                || !policy_allows_side_effect(&policy, "process_exec") =>
        {
            return reject_policy(format!(
                "builtin '{name}' exceeds vision.ocr/process ceiling"
            ));
        }
        "mcp_connect"
        | "mcp_ensure_active"
        | "mcp_call"
        | "mcp_list_tools"
        | "mcp_list_resources"
        | "mcp_list_resource_templates"
        | "mcp_read_resource"
        | "mcp_list_prompts"
        | "mcp_get_prompt"
        | "mcp_server_info"
        | "mcp_disconnect"
            if !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
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
        "host_tool_list" | "host_tool_call"
            if !policy_allows_capability(&policy, "host", "tool_call") =>
        {
            return reject_policy(format!("builtin '{name}' exceeds host.tool_call ceiling"));
        }
        _ => {}
    }
    Ok(())
}

pub fn enforce_current_policy_for_bridge_builtin(name: &str) -> Result<(), VmError> {
    let trusted = TRUSTED_BRIDGE_CALL_DEPTH.with(|depth| *depth.borrow() > 0);
    if trusted {
        return Ok(());
    }
    if current_execution_policy().is_some() {
        return reject_policy(format!(
            "bridged builtin '{name}' exceeds execution policy; declare an explicit capability/tool surface instead"
        ));
    }
    Ok(())
}

pub fn enforce_current_policy_for_tool(tool_name: &str) -> Result<(), PolicyDenial> {
    use crate::agent_events::DenialGate;
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    if !policy_allows_tool(&policy, tool_name) {
        return reject_tool(
            DenialGate::ToolCeiling,
            None,
            format!("tool '{tool_name}' exceeds tool ceiling"),
        );
    }
    if let Some(annotations) = policy.tool_annotations.get(tool_name) {
        for (capability, ops) in &annotations.capabilities {
            for op in ops {
                if !policy_allows_capability(&policy, capability, op) {
                    return reject_tool(
                        DenialGate::CapabilityCeiling,
                        Some(format!("{capability}.{op}")),
                        format!("tool '{tool_name}' exceeds capability ceiling: {capability}.{op}"),
                    );
                }
            }
        }
        let requested_level = annotations.side_effect_level;
        if requested_level != SideEffectLevel::None
            && !policy_allows_side_effect(&policy, requested_level.as_str())
        {
            return reject_tool(
                DenialGate::SideEffectCeiling,
                None,
                format!(
                    "tool '{tool_name}' exceeds side-effect ceiling: {}",
                    requested_level.as_str()
                ),
            );
        }
    }
    Ok(())
}

// ── Output visibility redaction ─────────────────────────────────────
//
// Transcript lifecycle (reset, fork, trim, compact) now lives on
// `crate::agent_sessions` as explicit imperative builtins. All that
// remains here is the per-call visibility filter, which is
// output-shaping (not lifecycle).

/// Filter a transcript dict down to the caller-visible subset, based
/// on the `output_visibility` node option. `None` or any unknown
/// visibility returns the transcript unchanged — callers are expected
/// to validate the string against a known set upstream.
pub fn redact_transcript_visibility(
    transcript: &VmValue,
    visibility: Option<&str>,
) -> Option<VmValue> {
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
            .filter_map(redact_public_message)
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
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(public_messages)),
    );
    redacted.insert(
        crate::value::intern_key("events"),
        VmValue::List(std::sync::Arc::new(public_events)),
    );
    Some(VmValue::dict(redacted))
}

fn redact_public_message(message: &VmValue) -> Option<VmValue> {
    let Some(dict) = message.as_dict() else {
        return Some(message.clone());
    };
    if dict.get("role").map(|value| value.display()).as_deref() == Some("tool_result") {
        return None;
    }
    if dict
        .get("visibility")
        .map(|value| value.display())
        .is_some_and(|visibility| visibility != "public")
    {
        return None;
    }

    let mut redacted = dict.clone();
    let mut saw_structured_blocks = false;
    let mut public_text = Vec::new();
    for key in ["content", "blocks"] {
        if let Some(VmValue::List(blocks)) = dict.get(key) {
            saw_structured_blocks = true;
            let public_blocks = blocks
                .iter()
                .filter_map(redact_public_block)
                .collect::<Vec<_>>();
            if key == "blocks" || public_text.is_empty() {
                public_text = text_fragments_from_blocks(&public_blocks);
            }
            redacted.insert(
                crate::value::intern_key(key),
                VmValue::List(std::sync::Arc::new(public_blocks)),
            );
        }
    }
    if saw_structured_blocks {
        if public_text.is_empty() {
            redacted.remove("text");
        } else {
            redacted.put_str("text", public_text.join("\n"));
        }
    }
    Some(VmValue::dict(redacted))
}

fn redact_public_block(block: &VmValue) -> Option<VmValue> {
    let Some(dict) = block.as_dict() else {
        return Some(block.clone());
    };
    if dict
        .get("visibility")
        .map(|value| value.display())
        .is_some_and(|visibility| visibility != "public")
    {
        return None;
    }
    Some(block.clone())
}

fn text_fragments_from_blocks(blocks: &[VmValue]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|block| block.as_dict())
        .filter_map(|dict| dict.get("text"))
        .filter_map(|text| match text {
            VmValue::String(value) if !value.is_empty() => Some(value.to_string()),
            _ => None,
        })
        .collect()
}

pub fn builtin_ceiling() -> CapabilityPolicy {
    CapabilityPolicy {
        // `capabilities` is intentionally empty: the host capability manifest
        // is the sole authority, and an allowlist here would silently block
        // any capability the host adds later.
        tools: Vec::new(),
        capabilities: BTreeMap::new(),
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        // The builtin ceiling is the runtime's OUTERMOST bound — the top of the
        // side-effect ladder. Every real policy intersects DOWN from here, so this
        // must be the maximum level or it would silently cap more-invasive tools
        // out entirely. It tracks the top of the ladder: `desktop_control`. This
        // does not loosen anything — a normal agent's surface policy still caps at
        // the max of ITS tools (e.g. `network`); only a surface that actually
        // carries a `desktop_control` tool (computer use, gated by the off-by-
        // default flag) can reach the top.
        // Tracks the ladder top via `SideEffectLevel::MAX` (never a hardcoded level).
        side_effect_level: Some(SideEffectLevel::MAX.as_str().to_string()),
        recursion_limit: Some(RuntimeLimits::DEFAULT.max_nested_execution_depth),
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: Default::default(),
    }
}

/// Declarative policy for tool approval gating. Allows pipelines to
/// specify which tools are auto-approved, auto-denied, or require
/// host confirmation, plus write-path allowlists.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolApprovalPolicy {
    /// Ordered allow/ask/deny rules over tool metadata, path, command,
    /// URL, MCP, agent/persona/mode, and repeat-count dimensions.
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
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
    /// Explicit opt-out for the deny-by-default sensitive-path guard.
    #[serde(default)]
    pub allow_sensitive_paths: bool,
    /// Additional or replacement sensitive path globs. Empty uses the
    /// runtime defaults such as `.env`, private keys, and credential files.
    #[serde(default)]
    pub sensitive_path_patterns: Vec<String>,
    /// Explicit opt-out for the external-path guard on declared path args.
    #[serde(default)]
    pub allow_external_paths: bool,
    /// Host-absolute roots allowed when `allow_external_paths` is false.
    #[serde(default)]
    pub external_roots: Vec<String>,
    /// Optional repeated-call threshold for the same `(session, tool, args)`.
    #[serde(default, alias = "repeated_call_limit")]
    pub repeat_limit: Option<u64>,
    /// Action for `repeat_limit`; defaults to `ask`.
    #[serde(default, alias = "repeated_call_action")]
    pub repeat_action: Option<PolicyAction>,
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
    pub fn evaluate_detailed(&self, tool_name: &str, args: &serde_json::Value) -> PolicyEvaluation {
        approval_rules::evaluate_tool_approval_policy(self, tool_name, args, None)
    }

    pub fn evaluate_detailed_with_repeat(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        repeat_count: u64,
    ) -> PolicyEvaluation {
        approval_rules::evaluate_tool_approval_policy(self, tool_name, args, Some(repeat_count))
    }

    /// Evaluate whether a tool call should be approved, denied, or needs
    /// host confirmation.
    pub fn evaluate(&self, tool_name: &str, args: &serde_json::Value) -> ToolApprovalDecision {
        let decision = self.evaluate_detailed(tool_name, args);
        if decision.is_deny() {
            return ToolApprovalDecision::AutoDenied {
                reason: decision.reason,
            };
        }
        if decision.is_ask() {
            return ToolApprovalDecision::RequiresHostApproval;
        }
        ToolApprovalDecision::AutoApproved
    }

    /// Merge two approval policies, taking the most restrictive combination.
    /// - auto_approve: only tools approved by BOTH policies stay approved
    ///   (if either policy has no patterns, the other's patterns are used)
    /// - auto_deny / require_approval: union (either policy can deny/gate)
    /// - write_path_allowlist: intersection (both must allow the path)
    pub fn intersect(&self, other: &ToolApprovalPolicy) -> ToolApprovalPolicy {
        let auto_approve = if self.auto_approve.is_empty() {
            other.auto_approve.clone()
        } else if other.auto_approve.is_empty() {
            self.auto_approve.clone()
        } else {
            self.auto_approve
                .iter()
                .filter(|p| other.auto_approve.contains(p))
                .cloned()
                .collect()
        };
        let mut auto_deny = self.auto_deny.clone();
        auto_deny.extend(other.auto_deny.iter().cloned());
        let mut require_approval = self.require_approval.clone();
        require_approval.extend(other.require_approval.iter().cloned());
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
        let mut rules = self.rules.clone();
        rules.extend(other.rules.iter().cloned());
        let mut sensitive_path_patterns = self.sensitive_path_patterns.clone();
        sensitive_path_patterns.extend(other.sensitive_path_patterns.iter().cloned());
        sensitive_path_patterns.sort();
        sensitive_path_patterns.dedup();
        let external_roots = if self.external_roots.is_empty() {
            other.external_roots.clone()
        } else if other.external_roots.is_empty() {
            self.external_roots.clone()
        } else {
            self.external_roots
                .iter()
                .filter(|root| other.external_roots.contains(root))
                .cloned()
                .collect()
        };
        ToolApprovalPolicy {
            rules,
            auto_approve,
            auto_deny,
            require_approval,
            write_path_allowlist,
            allow_sensitive_paths: self.allow_sensitive_paths && other.allow_sensitive_paths,
            sensitive_path_patterns,
            allow_external_paths: self.allow_external_paths && other.allow_external_paths,
            external_roots,
            repeat_limit: match (self.repeat_limit, other.repeat_limit) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(left), None) => Some(left),
                (None, Some(right)) => Some(right),
                (None, None) => None,
            },
            repeat_action: match (self.repeat_action, other.repeat_action) {
                (Some(PolicyAction::Deny), _) | (_, Some(PolicyAction::Deny)) => {
                    Some(PolicyAction::Deny)
                }
                (Some(PolicyAction::Ask), _) | (_, Some(PolicyAction::Ask)) => {
                    Some(PolicyAction::Ask)
                }
                (Some(PolicyAction::Allow), Some(PolicyAction::Allow)) => Some(PolicyAction::Allow),
                (Some(action), None) | (None, Some(action)) => Some(action),
                (None, None) => None,
            },
        }
    }
}

#[cfg(test)]
mod approval_policy_tests {
    use super::*;
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    use crate::tool_annotations::{ToolAnnotations, ToolArgSchema, ToolKind};

    fn workspace_caps(ops: &[&str]) -> CapabilityPolicy {
        CapabilityPolicy {
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                ops.iter().map(|s| s.to_string()).collect(),
            )]),
            ..Default::default()
        }
    }

    #[test]
    fn builtin_ceiling_permits_desktop_control_but_a_lower_ceiling_denies_it() {
        // The runtime's outer bound must admit the most-invasive level, or a
        // desktop-control (computer-use) tool would be exposed-but-denied under
        // the default ceiling.
        let builtin = builtin_ceiling();
        assert!(policy_allows_side_effect(
            &builtin,
            SideEffectLevel::DesktopControl.as_str()
        ));

        // A narrower policy (e.g. a normal agent whose tools top out at network)
        // still denies a desktop-control tool — the level is a real gate, not a
        // no-op.
        let network_ceiling = CapabilityPolicy {
            side_effect_level: Some(SideEffectLevel::Network.as_str().to_string()),
            ..Default::default()
        };
        assert!(!policy_allows_side_effect(
            &network_ceiling,
            SideEffectLevel::DesktopControl.as_str()
        ));
        // ...but that same network ceiling still admits everything at or below it.
        assert!(policy_allows_side_effect(
            &network_ceiling,
            SideEffectLevel::ProcessExec.as_str()
        ));
    }

    #[test]
    fn read_text_subsumes_exists_probe() {
        // A narrowed worker policy that grants read_text/list (the shape derived
        // from look/edit/scaffold tool annotations) but never declares the
        // weaker `workspace.exists` op must still permit `file_exists`/`stat`:
        // existence is strictly less information than reading the file. Without
        // subsumption this silently wedged every parallel sub-agent (look denied
        // -> zero progress -> zero edits).
        push_execution_policy(workspace_caps(&[
            "read_text",
            "list",
            "write_text",
            "apply_edit",
        ]));
        assert!(enforce_current_policy_for_builtin("file_exists", &[]).is_ok());
        assert!(enforce_current_policy_for_builtin("stat", &[]).is_ok());
        pop_execution_policy();
    }

    #[test]
    fn list_alone_subsumes_exists_probe() {
        // Listing a directory already reveals which entries exist.
        push_execution_policy(workspace_caps(&["list"]));
        assert!(enforce_current_policy_for_builtin("file_exists", &[]).is_ok());
        pop_execution_policy();
    }

    #[test]
    fn exists_probe_rejected_without_any_read_grant() {
        // A write-only grant exposes no read surface, so the existence probe is
        // genuinely above the ceiling and must still be rejected.
        push_execution_policy(workspace_caps(&["write_text", "apply_edit"]));
        assert!(enforce_current_policy_for_builtin("file_exists", &[]).is_err());
        pop_execution_policy();
    }

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

    #[test]
    fn write_path_allowlist_matches_recovered_workspace_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("packages/demo")).unwrap();
        std::fs::write(temp.path().join("packages/demo/file.txt"), "ok").unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(temp.path().to_string_lossy().into_owned()),
                source_dir: Some(temp.path().to_string_lossy().into_owned()),
                env: BTreeMap::new(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));

        let mut tool_annotations = BTreeMap::new();
        tool_annotations.insert(
            "write_file".to_string(),
            ToolAnnotations {
                kind: ToolKind::Edit,
                arg_schema: ToolArgSchema {
                    path_params: vec!["path".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        push_execution_policy(CapabilityPolicy {
            tool_annotations,
            ..Default::default()
        });

        let policy = ToolApprovalPolicy {
            write_path_allowlist: vec!["packages/demo/file.txt".to_string()],
            ..Default::default()
        };
        let decision = policy.evaluate(
            "write_file",
            &serde_json::json!({"path": "/packages/demo/file.txt"}),
        );
        assert_eq!(decision, ToolApprovalDecision::AutoApproved);

        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn write_path_allowlist_does_not_block_read_only_tools() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("packages/demo")).unwrap();
        std::fs::write(temp.path().join("packages/demo/context.txt"), "ok").unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(temp.path().to_string_lossy().into_owned()),
                source_dir: Some(temp.path().to_string_lossy().into_owned()),
                env: BTreeMap::new(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));

        let mut tool_annotations = BTreeMap::new();
        tool_annotations.insert(
            "read_file".to_string(),
            ToolAnnotations {
                kind: ToolKind::Read,
                arg_schema: ToolArgSchema {
                    path_params: vec!["path".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        push_execution_policy(CapabilityPolicy {
            tool_annotations,
            ..Default::default()
        });

        let policy = ToolApprovalPolicy {
            write_path_allowlist: vec!["packages/demo/file.txt".to_string()],
            ..Default::default()
        };
        let decision = policy.evaluate(
            "read_file",
            &serde_json::json!({"path": "/packages/demo/context.txt"}),
        );
        assert_eq!(decision, ToolApprovalDecision::AutoApproved);

        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn builtin_policy_covers_fs_read_and_list_helpers() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([("workspace".to_string(), vec!["exists".to_string()])]),
            side_effect_level: Some("read_only".to_string()),
            ..CapabilityPolicy::default()
        });

        for name in [
            "read_lines",
            "find_text",
            "walk_dir",
            "glob",
            "project_context_profile_native",
        ] {
            assert!(
                enforce_current_policy_for_builtin(name, &[]).is_err(),
                "{name} should be rejected when the matching workspace capability is absent"
            );
        }

        pop_execution_policy();
    }

    #[test]
    fn move_file_requires_workspace_write_side_effect() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["write_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            ..CapabilityPolicy::default()
        });

        let error = enforce_current_policy_for_builtin("move_file", &[]).unwrap_err();
        assert!(
            error.to_string().contains("workspace write ceiling"),
            "unexpected error: {error}"
        );

        pop_execution_policy();
    }

    #[test]
    fn unix_socket_json_request_requires_network_side_effect() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            side_effect_level: Some("read_only".to_string()),
            ..CapabilityPolicy::default()
        });

        let error =
            enforce_current_policy_for_builtin("__net_unix_socket_json_request", &[]).unwrap_err();
        assert!(
            error.to_string().contains("network ceiling"),
            "unexpected error: {error}"
        );

        pop_execution_policy();
    }

    #[test]
    fn files_upload_requires_workspace_read_and_network_side_effect() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            ..CapabilityPolicy::default()
        });

        let network_error = enforce_current_policy_for_builtin("__files_upload", &[]).unwrap_err();
        assert!(
            network_error.to_string().contains("network ceiling"),
            "unexpected error: {network_error}"
        );
        pop_execution_policy();

        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([("workspace".to_string(), vec!["exists".to_string()])]),
            side_effect_level: Some("network".to_string()),
            ..CapabilityPolicy::default()
        });
        let read_error = enforce_current_policy_for_builtin("__files_upload", &[]).unwrap_err();
        assert!(
            read_error.to_string().contains("workspace.read_text"),
            "unexpected error: {read_error}"
        );

        pop_execution_policy();
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
        // must keep the done-sentinel pathway enabled so loop-until-done agents
        // don't lose their completion signal.
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
mod visibility_redaction_tests {
    use super::*;
    use crate::value::VmValue;

    fn mock_transcript() -> VmValue {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "content": "hello"}),
            serde_json::json!({"role": "tool_result", "content": "internal tool output"}),
        ];
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
    fn visibility_none_returns_unchanged() {
        let t = mock_transcript();
        let result = redact_transcript_visibility(&t, None).unwrap();
        assert_eq!(message_count(&result), 3);
    }

    #[test]
    fn visibility_public_drops_tool_results() {
        let t = mock_transcript();
        let result = redact_transcript_visibility(&t, Some("public")).unwrap();
        assert_eq!(message_count(&result), 2);
    }

    #[test]
    fn visibility_public_drops_private_content_blocks() {
        let t = crate::schema::json_to_vm_value(&serde_json::json!({
            "messages": [
                {
                    "role": "assistant",
                    "visibility": "public",
                    "text": "visible answer\nsecret chain",
                    "content": [
                        {"type": "output_text", "text": "visible answer", "visibility": "public"},
                        {"type": "reasoning", "text": "secret chain", "visibility": "private"}
                    ],
                    "blocks": [
                        {"type": "output_text", "text": "visible block", "visibility": "public"},
                        {"type": "tool_call", "text": "internal args", "visibility": "internal"}
                    ]
                }
            ],
            "events": []
        }));

        let result = redact_transcript_visibility(&t, Some("public")).unwrap();
        let rendered = result.display();
        assert!(rendered.contains("visible answer"));
        assert!(rendered.contains("visible block"));
        assert!(!rendered.contains("secret chain"));
        assert!(!rendered.contains("internal args"));
    }

    #[test]
    fn visibility_unknown_string_is_pass_through() {
        let t = mock_transcript();
        let result = redact_transcript_visibility(&t, Some("internal")).unwrap();
        assert_eq!(message_count(&result), 3);
    }
}
