//! Per-session policy installation and teardown.
//!
//! Installing an agent session's execution, approval, command, and dynamic
//! permission scopes is one concern with one invariant: every scope
//! intersects the active outer policy so a sub-agent can only narrow its
//! parent's ceiling, and a partial install unwinds rather than leaking.
//! Keeping it here means the host module owns dispatch, not policy plumbing.

use super::{vm_to_json, InstalledPolicies, SessionPolicyGuard};
use crate::llm::permissions;
use crate::orchestration::{
    enter_nested_execution_policy, pop_approval_policy, pop_execution_policy, push_approval_policy,
    push_command_policy, push_execution_policy, CapabilityPolicy, NestedExecutionGuard,
    NestedExecutionKind, ToolApprovalPolicy, NESTED_KIND_OPTION_KEY, NESTED_LABEL_OPTION_KEY,
};
use crate::value::{VmError, VmValue};

/// Install per-agent execution / approval / command / dynamic permission
/// policies onto the thread-local stacks for the lifetime of a guarded
/// tool dispatch. Each scope intersects with the currently-active outer
/// policy (when any) so a sub-agent cannot widen its parent's ceiling —
/// only narrow it. Dynamic permissions are stack-checked, so push as-is
/// and rely on the dispatch path to honour every active scope.
///
/// On any failure the partially-pushed stacks are unwound before
/// returning, so the caller never has to worry about leaked policy
/// state.
pub(crate) fn install_session_policy_guard(
    opts_map: &crate::value::DictMap,
) -> Result<SessionPolicyGuard, VmError> {
    let mut installed = InstalledPolicies::default();
    match install_session_policies_inner(opts_map, &mut installed) {
        Ok(()) => Ok(SessionPolicyGuard { installed }),
        Err(error) => {
            release_session_policies(&installed);
            Err(error)
        }
    }
}

/// The exact option keys [`install_session_policies_inner`] reads. Kept
/// adjacent to that function so the list cannot drift: any new policy-shaped
/// option MUST be added here, otherwise the tool-dispatch fast path would
/// skip installing it.
const SESSION_POLICY_OPTION_KEYS: [&str; 6] = [
    "policy",
    "approval_policy",
    "command_policy",
    "permissions",
    "tool_precheck",
    // Load-bearing. A loop that installs ONLY a reviewer names no other key
    // here, so leaving it out would send that call down the fast path and the
    // reviewer would never be installed -- present in the source, unreachable
    // at runtime, and silent about it.
    "approval_reviewer",
];

/// Whether `opts_map` carries any policy/permission-shaped key that
/// [`install_session_policy_guard`] would act on. Presence is checked, not
/// validity: a key that is present but nil/invalid still routes the caller
/// through the guard (which no-ops or errors exactly as before), so the fast
/// path only ever skips a provable no-op.
pub(crate) fn options_request_session_policies(opts_map: &crate::value::DictMap) -> bool {
    SESSION_POLICY_OPTION_KEYS
        .iter()
        .any(|key| opts_map.get(*key).is_some())
}

fn install_session_policies_inner(
    opts_map: &crate::value::DictMap,
    installed: &mut InstalledPolicies,
) -> Result<(), VmError> {
    if let Some(requested) = parse_capability_policy(opts_map.get("policy"))? {
        let effective = match crate::orchestration::current_execution_policy() {
            Some(outer) => outer.intersect(&requested).map_err(VmError::Runtime)?,
            None => requested,
        };
        push_execution_policy(effective);
        installed.pushed_execution = true;
    }

    if let Some(requested) = parse_approval_policy(opts_map.get("approval_policy"))? {
        let effective = match crate::orchestration::current_approval_policy() {
            Some(outer) => outer.intersect(&requested),
            None => requested,
        };
        push_approval_policy(effective);
        installed.pushed_approval = true;
    }

    if let Some(policy) = crate::orchestration::parse_command_policy_value(
        opts_map.get("command_policy"),
        "agent_loop.command_policy",
    )? {
        push_command_policy(policy);
        installed.pushed_command = true;
    }

    if let Some(precheck) = crate::orchestration::parse_tool_precheck_value(
        opts_map.get("tool_precheck"),
        "agent_loop.tool_precheck",
    )? {
        crate::orchestration::push_tool_precheck(precheck);
        installed.pushed_precheck = true;
    }

    // The `AutoReview` answerer. Typed input, never ambient: a resolver picked
    // up from the environment would make "which policy did this run enforce" a
    // question the receipt could not answer.
    if let Some(reviewer) = crate::orchestration::parse_approval_reviewer_value(
        opts_map.get("approval_reviewer"),
        "agent_loop.approval_reviewer",
    )? {
        crate::orchestration::push_approval_reviewer(reviewer);
        installed.pushed_approval_reviewer = true;
    }

    if let Some(permissions) = permissions::parse_dynamic_permission_policy(
        opts_map.get("permissions"),
        "agent_loop.permissions",
    )? {
        permissions::push_dynamic_permission_policy(permissions);
        installed.pushed_permissions = true;
    }

    Ok(())
}

pub(super) fn release_session_policies(installed: &InstalledPolicies) {
    if installed.pushed_approval_reviewer {
        crate::orchestration::pop_approval_reviewer();
    }
    if installed.pushed_precheck {
        crate::orchestration::pop_tool_precheck();
    }
    if installed.pushed_permissions {
        permissions::pop_dynamic_permission_policy();
    }
    if installed.pushed_command {
        crate::orchestration::pop_command_policy();
    }
    if installed.pushed_approval {
        pop_approval_policy();
    }
    if installed.pushed_execution {
        pop_execution_policy();
    }
}

fn parse_capability_policy(value: Option<&VmValue>) -> Result<Option<CapabilityPolicy>, VmError> {
    let Some(value) = value else { return Ok(None) };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    serde_json::from_value::<CapabilityPolicy>(crate::llm::vm_value_to_json(value))
        .map(Some)
        .map_err(|error| VmError::Runtime(format!("agent_loop.policy: invalid policy: {error}")))
}

/// Apply the nested-execution budget check at `agent_loop` entry and
/// install the decremented per-session execution policy. The caller
/// (sub_agent_run / spawn_agent / workflow stage / direct invocation)
/// can pass `_nested_kind` and `_nested_label` to refine the audit and
/// error wording; we default to `agent_loop` + the session id.
pub(super) fn install_session_nested_budget(
    opts_map: &crate::value::DictMap,
    session_id: &str,
) -> Result<NestedExecutionGuard, VmError> {
    let requested = parse_capability_policy(opts_map.get("policy"))?;
    let kind =
        NestedExecutionKind::parse_or_default(opts_map.get(NESTED_KIND_OPTION_KEY).and_then(|v| {
            match v {
                VmValue::String(text) => Some(text.as_str()),
                _ => None,
            }
        }));
    let label = opts_map
        .get(NESTED_LABEL_OPTION_KEY)
        .and_then(|v| match v {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| session_id.to_string());
    enter_nested_execution_policy(requested, kind, &label)
}

/// Build a categorized `nested_execution_budget` denial payload that
/// the Harn-side `agent_loop` returns verbatim when the budget gate
/// rejects the launch. Mirrors `build_user_prompt_block_result` —
/// session is opened so transcript readers see the rejection event,
/// then we surface the canonical error envelope.
pub(super) fn build_nested_budget_denial(
    session_id: &str,
    prompt: &str,
    error: &VmError,
) -> VmValue {
    let (message, category) = match error {
        VmError::CategorizedError { message, category } => (message.clone(), category.as_str()),
        other => (other.to_string(), "tool_rejected"),
    };
    let _ = crate::agent_sessions::append_event(
        session_id,
        crate::llm::helpers::transcript_event(
            "nested_execution_budget_denied",
            "system",
            "internal",
            &message,
            Some(serde_json::json!({
                "category": category,
                "session_id": session_id,
            })),
        ),
    );
    let transcript_json = crate::agent_sessions::transcript(session_id)
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let result = serde_json::json!({
        "status": "blocked",
        "final_status": "budget_exhausted",
        "stop_reason": "nested_execution_budget_exhausted",
        "error": {
            "category": category,
            "kind": "budget_exhausted",
            "reason": "nested_execution_budget_exhausted",
            "message": message,
        },
        "text": "",
        "visible_text": "",
        "private_reasoning": serde_json::Value::Null,
        "thinking_summary": serde_json::Value::Null,
        "llm": {"iterations": 0, "duration_ms": 0, "input_tokens": 0, "output_tokens": 0},
        "tools": {"calls": [], "successful": [], "rejected": [], "mode": ""},
        "transcript": transcript_json,
        "trace": serde_json::Value::Null,
        "tokens_used": 0,
        "cost_usd": 0.0,
        "session_id": session_id,
        "task": prompt,
        "daemon_state": serde_json::Value::Null,
        "daemon_snapshot_path": serde_json::Value::Null,
    });
    crate::stdlib::json_to_vm_value(&result)
}

fn parse_approval_policy(value: Option<&VmValue>) -> Result<Option<ToolApprovalPolicy>, VmError> {
    let Some(value) = value else { return Ok(None) };
    if matches!(value, VmValue::Nil) {
        return Ok(None);
    }
    serde_json::from_value::<ToolApprovalPolicy>(crate::llm::vm_value_to_json(value))
        .map(Some)
        .map_err(|error| {
            VmError::Runtime(format!(
                "agent_loop.approval_policy: invalid policy: {error}"
            ))
        })
}
