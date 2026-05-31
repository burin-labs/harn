//! Host primitives backing the Harn-driven agent loop in
//! `std/agent/loop.harn`.
//!
//! These are CRUD-shaped primitives over per-session host state. The
//! decision logic (iterate, sentinel-check, dispatch tools, judge,
//! continue/break) lives in Harn; Rust is reduced to data plumbing,
//! provider/tool capability surfaces, and resource lifecycle.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::orchestration::{
    enter_nested_execution_policy, pop_approval_policy, pop_execution_policy, push_approval_policy,
    push_command_policy, push_execution_policy, CapabilityPolicy, NestedExecutionGuard,
    NestedExecutionKind, ToolApprovalPolicy, NESTED_KIND_OPTION_KEY, NESTED_LABEL_OPTION_KEY,
};
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::cost::calculate_cost_for_provider;
use super::permissions;
use super::tools::build_assistant_response_message;

const HOST_SESSION_FINALIZE: &str = "__host_agent_session_finalize";
const HOST_SESSION_RECORD_ASSISTANT: &str = "__host_agent_session_record_assistant";
const HOST_SESSION_RECORD_TOOL_RESULTS: &str = "__host_agent_session_record_tool_results";
const HOST_SESSION_RECORD_USAGE: &str = "__host_agent_session_record_usage";
const HOST_SESSION_DRAIN_FEEDBACK: &str = "__host_agent_session_drain_feedback";
const HOST_SESSION_DRAIN_BRIDGE_INJECTIONS: &str = "__host_agent_session_drain_bridge_injections";
const HOST_SESSION_PUSH_BRIDGE_INJECTION: &str = "__host_agent_session_push_bridge_injection";
const HOST_SESSION_PENDING_INJECTIONS: &str = "__host_agent_session_pending_injections";
const HOST_SESSION_REVOKE_REMINDER: &str = "__host_agent_session_revoke_reminder";
const HOST_SESSION_TOTALS: &str = "__host_agent_session_totals";
const HOST_SESSION_POST_EVENT: &str = "__host_agent_session_post_event";
const HOST_SESSION_APPLY_REMINDER_POST_TURN: &str = "__host_agent_session_apply_reminder_post_turn";
const HOST_SESSION_SET_ACTIVE_SKILLS: &str = "__host_agent_session_set_active_skills";
const HOST_SESSION_ACTIVE_SKILLS: &str = "__host_agent_session_active_skills";
const HOST_SESSION_REPLACE_MESSAGES: &str = "__host_agent_session_replace_messages";
const HOST_SESSION_PROJECT_TURN: &str = "__host_agent_session_project_turn";
const HOST_SESSION_CLAIM_TOOL_FORMAT: &str = "__host_agent_session_claim_tool_format";
const HOST_DAEMON_SNAPSHOT: &str = "__host_agent_daemon_snapshot";
const HOST_DAEMON_WAIT: &str = "__host_agent_daemon_wait";
const HOST_AGENT_EMIT_EVENT: &str = "__host_agent_emit_event";
const HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK: &str = "__host_agent_record_native_tool_fallback";
const HOST_AGENT_RECORD_COMPACTION: &str = "__host_agent_record_compaction";

/// Session-keyed record for Harn-driven agent loops. The Harn loop owns
/// iteration and decision logic; this struct holds only session-scoped
/// scalars (totals, active skills) that primitives need to read/write
/// atomically. Larger per-session state (transcript, subscribers) lives
/// in `crate::agent_sessions`.
struct AgentHostSession {
    session_id: String,
    task: String,
    tokens_used: i64,
    cost_used: f64,
    input_tokens: i64,
    output_tokens: i64,
    active_skills: Vec<String>,
    tool_calls: Vec<serde_json::Value>,
    successful_tools: Vec<String>,
    rejected_tools: Vec<String>,
    tool_mode: String,
    last_provider: Option<String>,
    last_model: Option<String>,
    pushed_transcript_dir: bool,
    started_at: String,
    /// Iteration cap from `agent_loop(options.max_iterations)`. Captured
    /// here so finalize can disambiguate `final_status == "budget_exhausted"`
    /// caused by hitting the cap (→ ACP `max_turn_requests`) from other
    /// budget paths.
    max_iterations: i64,
    daemon_state: Option<String>,
    daemon_snapshot_path: Option<String>,
    resumed_iterations: usize,
    daemon_watch_state: BTreeMap<String, u64>,
    daemon_idle_backoff_ms: u64,
    host_bridge: Option<Rc<crate::bridge::HostBridge>>,
    /// Provider-reported `stop_reason` from the most recent `llm_call`
    /// in this loop. Used by finalize to detect ACP `max_tokens` (when
    /// the last call truncated due to its `max_tokens` parameter) and
    /// `refusal` (Anthropic refusal stop_reason).
    last_llm_stop_reason: Option<String>,
    /// Pops the per-session capability policy off the execution stack
    /// on drop. Declared last so it Drops last in `AgentHostSession`'s
    /// natural field-order drop, after every other cleanup completes.
    #[allow(dead_code, reason = "held for Drop side effect")]
    nested_policy_guard: Option<NestedExecutionGuard>,
}

/// Tracks which scoped policy stacks were pushed for a guarded tool
/// dispatch so `Drop` can pop them in reverse order. The agent loop
/// honours per-agent ceilings by intersecting outer policies with the
/// requested per-agent ones before pushing, so child sub-agents never
/// widen permissions beyond their parents.
#[derive(Default)]
struct InstalledPolicies {
    pushed_execution: bool,
    pushed_approval: bool,
    pushed_command: bool,
    pushed_permissions: bool,
}

pub(crate) struct SessionPolicyGuard {
    installed: InstalledPolicies,
}

impl Drop for SessionPolicyGuard {
    fn drop(&mut self) {
        release_session_policies(&self.installed);
    }
}

thread_local! {
    static AGENT_HOST_SESSIONS: RefCell<BTreeMap<String, AgentHostSession>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_agent_session_host_state() {
    AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow_mut().clear());
}

fn with_session<R>(
    session_id: &str,
    label: &str,
    f: impl FnOnce(&mut AgentHostSession) -> Result<R, VmError>,
) -> Result<R, VmError> {
    AGENT_HOST_SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let session = sessions.get_mut(session_id).ok_or_else(|| {
            VmError::Runtime(format!("{label}: unknown agent session `{session_id}`"))
        })?;
        f(session)
    })
}

fn opts_dict(value: Option<&VmValue>) -> BTreeMap<String, VmValue> {
    match value {
        Some(VmValue::Dict(d)) => (**d).clone(),
        _ => BTreeMap::new(),
    }
}

fn json_to_vm(value: &serde_json::Value) -> VmValue {
    crate::stdlib::json_to_vm_value(value)
}

fn vm_to_json(value: &VmValue) -> serde_json::Value {
    crate::llm::vm_value_to_json(value)
}

fn list_items(value: &VmValue) -> Vec<VmValue> {
    match value {
        VmValue::List(items) => (**items).clone(),
        _ => Vec::new(),
    }
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> Option<&'a VmValue> {
    match value {
        VmValue::Dict(d) => d.get(key),
        _ => None,
    }
}

fn opt_str(map: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| match v {
        VmValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

fn opt_int(map: &BTreeMap<String, VmValue>, key: &str) -> Option<i64> {
    map.get(key).and_then(|v| match v {
        VmValue::Int(i) => Some(*i),
        VmValue::Float(f) => Some(*f as i64),
        _ => None,
    })
}

fn opt_json(map: &BTreeMap<String, VmValue>, key: &str) -> Option<serde_json::Value> {
    map.get(key)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(vm_to_json)
}

fn initial_user_content(
    opts_map: &BTreeMap<String, VmValue>,
    fallback_message: &str,
) -> serde_json::Value {
    opt_json(opts_map, "initial_user_content")
        .or_else(|| opt_json(opts_map, "initial_message_content"))
        .unwrap_or_else(|| serde_json::Value::String(fallback_message.to_string()))
}

fn now_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Initialize a Harn-driven agent session: open transcript, seed user message.
#[harn_builtin(
    sig = "__host_agent_session_init(message: string, system?: string|nil, options?: dict|nil) -> string",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_init(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let message = args.first().map(|v| v.display()).unwrap_or_default();
    let system = match args.get(1) {
        Some(VmValue::String(s)) => Some(s.to_string()),
        _ => None,
    };
    let opts_map = opts_dict(args.get(2));
    let host_bridge = super::agent_runtime::current_host_bridge();
    let session_id = opt_str(&opts_map, "session_id")
        .or_else(crate::agent_sessions::current_session_id)
        .unwrap_or_else(|| format!("agent_session_{}", now_id()));

    // Open the session record up front so hook tape capture works even
    // when `user_prompt_submit` vetoes the turn — the resulting blocked
    // result still surfaces a transcript with `hook_call`/`hook_vetoed`
    // entries.
    let prompt_session_id = crate::agent_sessions::open_or_create(Some(session_id.clone()));

    let prompt_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::UserPromptSubmit.as_str(),
        "session": {"id": &prompt_session_id},
        "prompt": &message,
        "system": system.clone().unwrap_or_default(),
    });
    if let crate::orchestration::HookControl::Block { reason } =
        crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::UserPromptSubmit,
            &prompt_payload,
        )
        .await?
    {
        let blocked = build_user_prompt_block_result(&prompt_session_id, &message, &reason);
        return Ok(agent_init_control_done(
            &prompt_session_id,
            &message,
            system.as_deref(),
            blocked,
        ));
    }

    let autonomy_budget = match check_autonomy_budget(&opts_map, &session_id).await? {
        AutonomyCheck::NoBudget => None,
        AutonomyCheck::Approved(config) => Some(config),
        AutonomyCheck::Denied(result) => {
            return Ok(agent_init_control_done(
                &session_id,
                &message,
                system.as_deref(),
                result,
            ));
        }
    };

    let session_system_prompt =
        super::helpers::compose_system_prompt(system.clone(), Some(&opts_map))?;
    let resolved = crate::agent_sessions::open_or_create(Some(session_id));
    if let Some(system_prompt) = session_system_prompt.as_deref() {
        crate::agent_sessions::record_system_prompt(&resolved, system_prompt)
            .map_err(VmError::Runtime)?;
    }

    let nested_policy_guard = match install_session_nested_budget(&opts_map, &resolved) {
        Ok(guard) => Some(guard),
        Err(error) => {
            let denial = build_nested_budget_denial(&resolved, &message, &error);
            return Ok(agent_init_control_done(
                &resolved,
                &message,
                system.as_deref(),
                denial,
            ));
        }
    };

    let max_iterations = opt_int(&opts_map, "max_iterations").unwrap_or(50).max(1);
    let max_verify_attempts = opt_int(&opts_map, "max_verify_attempts")
        .unwrap_or(20)
        .max(0);
    let daemon_config = super::daemon::parse_daemon_loop_config(Some(&opts_map));
    let resumed_iterations = match daemon_config.resume_path.as_deref() {
        Some(path) => super::daemon::load_snapshot(path)?.total_iterations,
        None => 0,
    };

    if let Some(config) = autonomy_budget.as_ref() {
        super::autonomy_budget::note_decision(config);
    }

    let persisted_active_skills = crate::agent_sessions::active_skills(&resolved);

    let tool_format = opt_str(&opts_map, "tool_format").unwrap_or_default();
    if !tool_format.is_empty() {
        crate::agent_sessions::claim_tool_format(&resolved, &tool_format)
            .map_err(VmError::Runtime)?;
    }

    let llm_transcript_dir = opt_str(&opts_map, "llm_transcript_dir").unwrap_or_default();
    let pushed_transcript_dir = !llm_transcript_dir.is_empty();
    if pushed_transcript_dir {
        super::agent_observe::push_llm_transcript_dir(&llm_transcript_dir);
    }
    let user_msg = serde_json::json!({
        "role": "user",
        "content": initial_user_content(&opts_map, &message),
    });
    crate::agent_sessions::inject_message(&resolved, json_to_vm(&user_msg))
        .map_err(VmError::Runtime)?;

    let session = AgentHostSession {
        session_id: resolved.clone(),
        task: message.clone(),
        tokens_used: 0,
        cost_used: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        active_skills: persisted_active_skills,
        tool_calls: Vec::new(),
        successful_tools: Vec::new(),
        rejected_tools: Vec::new(),
        tool_mode: tool_format,
        last_provider: None,
        last_model: None,
        pushed_transcript_dir,
        started_at: now_id(),
        max_iterations,
        daemon_state: None,
        daemon_snapshot_path: None,
        resumed_iterations,
        daemon_watch_state: BTreeMap::new(),
        daemon_idle_backoff_ms: 100,
        host_bridge,
        last_llm_stop_reason: None,
        nested_policy_guard,
    };

    AGENT_HOST_SESSIONS.with(|sessions| {
        sessions.borrow_mut().insert(resolved.clone(), session);
    });
    // Push the session id onto the thread-local current-session stack so
    // tool handlers + nested calls inside the loop see it via
    // `agent_session_current_id()`. Paired with the pop in finalize.
    crate::agent_sessions::push_current_session(resolved.clone());

    let start_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::SessionStart.as_str(),
        "session": {"id": &resolved},
        "task": &message,
        "system": system.clone().unwrap_or_default(),
        "max_iterations": max_iterations,
    });
    crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(&ctx),
        crate::orchestration::HookEvent::SessionStart,
        &start_payload,
    )
    .await?;
    // SessionStart is a paired event: hooks above run any user-registered
    // `session_start` closures, and this call lets canonical reminder
    // providers (currently `project_facts`) inject pre-turn context.
    // Mirrors the pattern used at the `PostToolUse` and `PostCompact` call
    // sites so adding new providers does not require new wiring.
    let _ = super::reminder_providers::evaluate_and_inject(
        Some(&ctx),
        crate::orchestration::HookEvent::SessionStart,
        &resolved,
        start_payload,
        super::reminder_providers::options_map_to_json(&opts_map),
    )
    .await?;

    let mut control = BTreeMap::new();
    control.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(resolved)),
    );
    control.insert("task".to_string(), VmValue::String(Rc::from(message)));
    control.insert(
        "system".to_string(),
        system
            .map(|s| VmValue::String(Rc::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    control.insert("max_iterations".to_string(), VmValue::Int(max_iterations));
    control.insert(
        "max_verify_attempts".to_string(),
        VmValue::Int(max_verify_attempts),
    );
    control.insert("done".to_string(), VmValue::Bool(false));
    Ok(VmValue::Dict(Rc::new(control)))
}

enum AutonomyCheck {
    NoBudget,
    Approved(super::autonomy_budget::AgentAutonomyBudget),
    Denied(VmValue),
}

async fn check_autonomy_budget(
    opts_map: &BTreeMap<String, VmValue>,
    session_id: &str,
) -> Result<AutonomyCheck, VmError> {
    let Some(config) =
        super::autonomy_budget::parse_autonomy_budget(Some(opts_map), session_id, "agent_loop")?
    else {
        return Ok(AutonomyCheck::NoBudget);
    };
    let trace_id = crate::triggers::dispatcher::current_dispatch_context()
        .map(|context| context.trigger_event.trace_id.0)
        .unwrap_or_else(|| format!("trace_{}", uuid::Uuid::now_v7()));
    match super::autonomy_budget::enforce_budget(config, session_id, &trace_id).await? {
        super::autonomy_budget::BudgetCheckOutcome::Approved(config) => {
            Ok(AutonomyCheck::Approved(config))
        }
        super::autonomy_budget::BudgetCheckOutcome::Denied { result } => {
            Ok(AutonomyCheck::Denied(json_to_vm(&result)))
        }
    }
}

fn session_status_indicates_error(final_status: &str) -> bool {
    matches!(
        final_status,
        "error" | "failed" | "provider_error" | "verify_exhausted" | "stuck"
    )
}

fn build_user_prompt_block_result(session_id: &str, prompt: &str, reason: &str) -> VmValue {
    let transcript_json = crate::agent_sessions::transcript(session_id)
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let result = serde_json::json!({
        "status": "blocked",
        "final_status": "blocked",
        "stop_reason": "user_prompt_submit_blocked",
        "error": {
            "category": "hook_denied",
            "event": crate::orchestration::HookEvent::UserPromptSubmit.as_str(),
            "reason": reason,
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

fn agent_init_control_done(
    session_id: &str,
    task: &str,
    system: Option<&str>,
    result: VmValue,
) -> VmValue {
    let mut control = BTreeMap::new();
    control.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.to_string())),
    );
    control.insert(
        "task".to_string(),
        VmValue::String(Rc::from(task.to_string())),
    );
    control.insert(
        "system".to_string(),
        system
            .map(|s| VmValue::String(Rc::from(s.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    control.insert("max_iterations".to_string(), VmValue::Int(0));
    control.insert("max_verify_attempts".to_string(), VmValue::Int(0));
    control.insert("done".to_string(), VmValue::Bool(true));
    control.insert("result".to_string(), result);
    VmValue::Dict(Rc::new(control))
}

/// Tear down a Harn-driven agent session and emit the final result dict.
#[harn_builtin(
    sig = "__host_agent_session_finalize(session_id: string, status: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_finalize(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{HOST_SESSION_FINALIZE}: missing session_id")))?;
    let status_dict = opts_dict(args.get(1));
    let final_status = opt_str(&status_dict, "final_status").unwrap_or_default();
    let stop_reason = opt_str(&status_dict, "stop_reason").unwrap_or_default();
    let terminal_error = opt_json(&status_dict, "error");

    let session = AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow_mut().remove(&session_id))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_SESSION_FINALIZE}: unknown session `{session_id}`"
            ))
        })?;
    permissions::clear_session_grants(&session_id);
    crate::orchestration::clear_approval_policy_repeat_counts(&session_id);
    if session.pushed_transcript_dir {
        super::agent_observe::pop_llm_transcript_dir();
    }

    let canonical_status = if final_status.is_empty() {
        "done".to_string()
    } else {
        final_status.clone()
    };
    if terminal_error.is_some() || session_status_indicates_error(&final_status) {
        let error_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::SessionError.as_str(),
            "session": {"id": &session_id},
            "final_status": &canonical_status,
            "stop_reason": stop_reason,
            "error": terminal_error.clone(),
        });
        // SessionError hooks are advisory — log but do not propagate so
        // session cleanup always runs.
        if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
            Some(&ctx),
            crate::orchestration::HookEvent::SessionError,
            &error_payload,
        )
        .await
        {
            crate::events::log_warn(
                "agent.session_error_hook",
                &format!("session={session_id} hook error: {err}"),
            );
        }
    }

    let end_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::SessionEnd.as_str(),
        "session": {"id": &session_id},
        "final_status": &canonical_status,
        "stop_reason": stop_reason,
        "iterations": opt_int(&status_dict, "iterations").unwrap_or(0),
    });
    if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(&ctx),
        crate::orchestration::HookEvent::SessionEnd,
        &end_payload,
    )
    .await
    {
        crate::events::log_warn(
            "agent.session_end_hook",
            &format!("session={session_id} hook error: {err}"),
        );
    }

    // Pair with the push in init so subsequent loops see the right stack.
    crate::agent_sessions::pop_current_session();
    // Fire registered native session-end hooks (e.g. cancelling orphaned
    // long-running handles) after the session has been removed from
    // the active map so hooks observe a fully-quiesced session.
    super::agent_runtime::fire_session_end_hooks(&session_id);

    let iterations = opt_int(&status_dict, "iterations").unwrap_or(0);
    let tool_mode = opt_str(&status_dict, "tool_mode").unwrap_or(session.tool_mode);
    let acp_stop_reason = canonical_acp_stop_reason(
        &final_status,
        iterations,
        session.max_iterations,
        session.last_llm_stop_reason.as_deref(),
    );
    // Surface the canonical reason to the host bridge so an outer ACP
    // adapter can populate `session/prompt`'s `stopReason`. The bridge
    // is opt-in: pipelines that don't run under ACP simply leave the
    // slot unset.
    if let Some(bridge) = super::agent_runtime::current_host_bridge() {
        bridge.set_prompt_stop_reason(acp_stop_reason);
    }
    if let Some(error) = terminal_error.as_ref() {
        let transcript_event = super::helpers::transcript_event(
            "agent_loop_terminal_error",
            "assistant",
            "internal",
            "Agent loop ended with a provider/tool-protocol failure",
            Some(serde_json::json!({
                "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
                "final_status": final_status,
                "stop_reason": stop_reason,
                "error": error,
            })),
        );
        crate::agent_sessions::append_event(&session_id, transcript_event)
            .map_err(VmError::Runtime)?;
    }
    let snapshot = crate::agent_sessions::transcript(&session_id);
    let transcript_json = snapshot
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let visible_text = snapshot
        .as_ref()
        .and_then(last_assistant_text)
        .unwrap_or_default();

    let trace_summary = super::trace::agent_trace_summary();
    let result = serde_json::json!({
        "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
        "final_status": final_status,
        "stop_reason": stop_reason,
        "acp_stop_reason": acp_stop_reason,
        "error": terminal_error,
        "text": visible_text,
        "visible_text": visible_text,
        "private_reasoning": serde_json::Value::Null,
        "thinking_summary": serde_json::Value::Null,
        "llm": {
            "iterations": iterations,
            "duration_ms": 0,
            "input_tokens": session.input_tokens,
            "output_tokens": session.output_tokens,
        },
        "tools": {
            "calls": session.tool_calls,
            "successful": session.successful_tools,
            "rejected": session.rejected_tools,
            "mode": tool_mode,
        },
        "transcript": transcript_json,
        "trace": trace_summary,
        "tokens_used": session.tokens_used,
        "cost_usd": session.cost_used,
        "session_id": session.session_id,
        "started_at": session.started_at,
        "task": session.task,
        "daemon_state": session.daemon_state,
        "daemon_snapshot_path": session.daemon_snapshot_path,
    });
    Ok(json_to_vm(&result))
}

/// Map an agent-loop terminal state to the canonical ACP `stopReason`
/// enumeration documented at <https://agentclientprotocol.com/protocol/prompt-turn>.
///
/// ACP defines five values: `end_turn`, `max_tokens`, `max_turn_requests`,
/// `refusal`, and `cancelled`. `cancelled` is decided one layer up by the
/// adapter (it observes the cancel notification directly) so this
/// function only chooses among the other four.
///
/// Precedence: a loop that ran out of turn budget overrides any
/// per-call signal — the caller stopped the agent before the model
/// could refuse or truncate again. When the loop exited cleanly we fall
/// through to the most recent provider stop_reason.
pub(crate) fn canonical_acp_stop_reason(
    final_status: &str,
    iterations: i64,
    max_iterations: i64,
    last_llm_stop_reason: Option<&str>,
) -> &'static str {
    if final_status == "budget_exhausted" {
        if max_iterations > 0 && iterations >= max_iterations {
            return "max_turn_requests";
        }
        // Token / cost / autonomy budgets all cap how many requests the
        // loop will issue, so they collapse to the same canonical
        // reason. ACP's `max_tokens` is reserved for a single response
        // truncated by the provider's `max_tokens` parameter.
        return "max_turn_requests";
    }
    canonical_provider_stop_reason(last_llm_stop_reason)
}

pub(crate) fn canonical_provider_stop_reason(last_llm_stop_reason: Option<&str>) -> &'static str {
    match last_llm_stop_reason {
        Some(reason) if reason.eq_ignore_ascii_case("max_tokens") => "max_tokens",
        Some(reason) if reason.eq_ignore_ascii_case("length") => "max_tokens",
        Some(reason) if reason.eq_ignore_ascii_case("refusal") => "refusal",
        _ => "end_turn",
    }
}

fn last_assistant_text(snapshot: &VmValue) -> Option<String> {
    let messages_value = dict_get(snapshot, "messages")?;
    let messages = list_items(messages_value);
    for msg in messages.iter().rev() {
        let role = dict_get(msg, "role")
            .map(|v| v.display())
            .unwrap_or_default();
        if role == "assistant" {
            let visible = dict_get(msg, "content")
                .map(|v| crate::visible_text::sanitize_visible_assistant_text(&v.display(), false))
                .unwrap_or_default();
            if !visible.trim().is_empty() {
                return Some(visible);
            }
        }
    }
    None
}

/// Return the visible message list for an agent session.
#[harn_builtin(
    sig = "__host_agent_session_messages(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let snapshot = crate::agent_sessions::transcript(&session_id);
    let messages = snapshot
        .as_ref()
        .and_then(|v| dict_get(v, "messages"))
        .cloned()
        .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new())));
    Ok(messages)
}

fn assistant_message_from_llm_result(llm_result: &VmValue) -> VmValue {
    let text = dict_get(llm_result, "text")
        .map(|v| v.display())
        .unwrap_or_default();
    let provider = dict_get(llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    // Only attach provider-native tool calls to the assistant envelope.
    // Text-mode calls remain inline in `text` and are parsed from there.
    let native_calls_value = dict_get(llm_result, "native_tool_calls")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let native_calls_json = list_items(&native_calls_value)
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    if native_calls_json.is_empty() {
        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from("assistant")));
        msg.insert("content".to_string(), VmValue::String(Rc::from(text)));
        return VmValue::Dict(Rc::new(msg));
    }

    let thinking = dict_get(llm_result, "thinking").map(|v| v.display());
    let msg = build_assistant_response_message(
        &text,
        &[],
        &native_calls_json,
        thinking.as_deref(),
        &provider,
        &model,
    );
    json_to_vm(&msg)
}

/// Append the assistant turn from an llm_call result to the session log.
#[harn_builtin(
    sig = "__host_agent_session_record_assistant(session_id: string, llm_result: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let provider = dict_get(&llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(&llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    let raw_tool_calls = dict_get(&llm_result, "tool_calls")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let calls_json = list_items(&raw_tool_calls)
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    )
    .map_err(VmError::Runtime)?;
    let _ = with_session(&session_id, HOST_SESSION_RECORD_ASSISTANT, |session| {
        session.tool_calls.extend(calls_json);
        if !provider.is_empty() {
            session.last_provider = Some(provider);
        }
        if !model.is_empty() {
            session.last_model = Some(model);
        }
        Ok(())
    });
    Ok(VmValue::Nil)
}

/// Pop the trailing assistant turn from the session transcript. Used by
/// step_judge replace mode to discard a vetoed turn before regeneration.
/// Errors if the trailing message is not an assistant turn.
#[harn_builtin(
    sig = "__host_agent_session_pop_last_assistant(session_id: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_pop_last_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let popped =
        crate::agent_sessions::pop_last_if_assistant(&session_id).map_err(VmError::Runtime)?;
    Ok(VmValue::Bool(popped))
}

fn tool_result_message_for_provider(
    provider: &str,
    model: &str,
    tool_format: &str,
    name: &str,
    tool_call_id: &str,
    observation: &str,
) -> VmValue {
    let mut msg = BTreeMap::new();
    if tool_format == "text" {
        msg.insert("role".to_string(), VmValue::String(Rc::from("user")));
    } else if crate::llm::provider::provider_uses_anthropic_messages(provider, model) {
        msg.insert("role".to_string(), VmValue::String(Rc::from("tool_result")));
        msg.insert(
            "tool_use_id".to_string(),
            VmValue::String(Rc::from(tool_call_id)),
        );
    } else {
        msg.insert("role".to_string(), VmValue::String(Rc::from("tool")));
        msg.insert("name".to_string(), VmValue::String(Rc::from(name)));
        if !tool_call_id.is_empty() {
            msg.insert(
                "tool_call_id".to_string(),
                VmValue::String(Rc::from(tool_call_id)),
            );
        }
    }
    msg.insert(
        "content".to_string(),
        VmValue::String(Rc::from(observation)),
    );
    VmValue::Dict(Rc::new(msg))
}

/// Recover the plan artifact from a dispatched emit_plan/update_plan result.
///
/// The local short-circuit handler (`handle_tool_locally`) returns the
/// pretty-printed plan JSON as a string, so the dispatch result's
/// `result` field is typically a string. We try parsing it; if that
/// fails, fall back to renormalizing from the tool arguments. Either
/// way we get a structured plan value the transcript "plan" event can
/// carry under `metadata.plan`.
fn plan_artifact_from_result(result: &VmValue) -> Option<serde_json::Value> {
    if let Some(VmValue::String(rendered)) = dict_get(result, "result") {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rendered) {
            if parsed.is_object() {
                return Some(parsed);
            }
        }
    }
    if let Some(value) = dict_get(result, "result") {
        let json = vm_to_json(value);
        if json.is_object() {
            return Some(json);
        }
    }
    let tool_name = dict_get(result, "tool_name")
        .or_else(|| dict_get(result, "name"))
        .map(|v| v.display())
        .unwrap_or_default();
    let arguments = dict_get(result, "arguments").map(vm_to_json)?;
    Some(super::plan::normalize_plan_tool_call(
        &tool_name, &arguments,
    ))
}

/// Append per-tool observation messages from a dispatch result.
#[harn_builtin(
    sig = "__host_agent_session_record_tool_results(session_id: string, dispatch: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_tool_results_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let dispatch = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let (provider, model) =
        with_session(&session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
            Ok((
                session.last_provider.clone().unwrap_or_default(),
                session.last_model.clone().unwrap_or_default(),
            ))
        })
        .unwrap_or_default();
    let tool_format = crate::agent_sessions::tool_format(&session_id).unwrap_or_default();
    // dispatch may be either a flat list of results (as returned by
    // agent_dispatch_tool_batch) or a dict with a `results` key (legacy
    // shape some callers still synthesize). Handle both.
    let results_value = match &dispatch {
        VmValue::List(_) => dispatch.clone(),
        _ => dict_get(&dispatch, "results")
            .cloned()
            .unwrap_or(VmValue::Nil),
    };
    let mut successful = Vec::new();
    let mut rejected = Vec::new();
    for result in list_items(&results_value).iter() {
        let name = dict_get(result, "tool_name")
            .or_else(|| dict_get(result, "name"))
            .map(|v| v.display())
            .unwrap_or_default();
        let observation = dict_get(result, "observation")
            .or_else(|| dict_get(result, "rendered_result"))
            .or_else(|| dict_get(result, "output"))
            .or_else(|| dict_get(result, "content"))
            .map(|v| v.display())
            .unwrap_or_default();
        let tool_call_id = dict_get(result, "tool_call_id")
            .or_else(|| dict_get(result, "tool_use_id"))
            .map(|v| v.display())
            .unwrap_or_default();
        let ok = match dict_get(result, "ok") {
            Some(VmValue::Bool(value)) => *value,
            _ => match dict_get(result, "success") {
                Some(VmValue::Bool(value)) => *value,
                _ => match dict_get(result, "status") {
                    Some(VmValue::String(s)) => s.as_ref() == "ok",
                    _ => true,
                },
            },
        };
        if ok {
            successful.push(name.clone());
        } else {
            rejected.push(name.clone());
        }
        if ok && super::plan::is_plan_tool(&name) {
            if let Some(plan_value) = plan_artifact_from_result(result) {
                let plan_metadata = serde_json::json!({"plan": plan_value});
                let event = super::helpers::transcript_event(
                    "plan",
                    "tool",
                    "public",
                    "",
                    Some(plan_metadata.clone()),
                );
                crate::agent_sessions::append_event(&session_id, event)
                    .map_err(VmError::Runtime)?;
                super::agent_runtime::emit_agent_event_sync(&AgentEvent::Plan {
                    session_id: session_id.clone(),
                    plan: plan_value,
                });
            }
        }
        crate::agent_sessions::inject_message(
            &session_id,
            tool_result_message_for_provider(
                &provider,
                &model,
                &tool_format,
                &name,
                &tool_call_id,
                &observation,
            ),
        )
        .map_err(VmError::Runtime)?;
    }
    let _ = with_session(&session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
        session.successful_tools.extend(successful);
        session.rejected_tools.extend(rejected);
        Ok(())
    });
    Ok(VmValue::Nil)
}

/// Accumulate token + cost usage from an llm_call result, return totals.
#[harn_builtin(
    sig = "__host_agent_session_record_usage(session_id: string, llm_result: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_usage_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let llm_block = dict_get(&llm_result, "llm")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let input_tokens = dict_get(&llm_block, "input_tokens")
        .or_else(|| dict_get(&llm_result, "input_tokens"))
        .and_then(|v| match v {
            VmValue::Int(i) => Some(*i),
            VmValue::Float(f) => Some(*f as i64),
            _ => None,
        })
        .unwrap_or(0);
    let output_tokens = dict_get(&llm_block, "output_tokens")
        .or_else(|| dict_get(&llm_result, "output_tokens"))
        .and_then(|v| match v {
            VmValue::Int(i) => Some(*i),
            VmValue::Float(f) => Some(*f as i64),
            _ => None,
        })
        .unwrap_or(0);
    let provider = dict_get(&llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let model = dict_get(&llm_result, "model")
        .map(|v| v.display())
        .unwrap_or_default();
    let cost = calculate_cost_for_provider(&provider, &model, input_tokens, output_tokens);
    let stop_reason = match dict_get(&llm_result, "stop_reason") {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };

    let totals = with_session(&session_id, HOST_SESSION_RECORD_USAGE, |session| {
        session.tokens_used = session
            .tokens_used
            .saturating_add(input_tokens)
            .saturating_add(output_tokens);
        session.input_tokens = session.input_tokens.saturating_add(input_tokens);
        session.output_tokens = session.output_tokens.saturating_add(output_tokens);
        session.cost_used += cost;
        if stop_reason.is_some() {
            session.last_llm_stop_reason = stop_reason.clone();
        }
        Ok((session.tokens_used, session.cost_used))
    })?;
    crate::agent_sessions::append_event(
        &session_id,
        crate::llm::helpers::transcript_event(
            "llm_call",
            "assistant",
            "internal",
            "LLM call completed",
            Some(serde_json::json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "provider": provider,
                "model": model,
                "cost_usd": cost,
                "provider_stop_reason": stop_reason,
                "canonical_stop_reason": canonical_provider_stop_reason(stop_reason.as_deref()),
            })),
        ),
    )
    .map_err(VmError::Runtime)?;
    let mut out = BTreeMap::new();
    out.insert("tokens_used".to_string(), VmValue::Int(totals.0));
    out.insert("cost_usd".to_string(), VmValue::Float(totals.1));
    Ok(VmValue::Dict(Rc::new(out)))
}

/// Drain pending runtime-feedback notes for a session (no-op shim).
#[harn_builtin(
    sig = "__host_agent_session_drain_feedback(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_drain_feedback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_DRAIN_FEEDBACK}: session_id must be a non-empty string"
            )))
        }
    };
    let drained = crate::orchestration::agent_inbox::drain(&session_id)
        .into_iter()
        .map(|entry| {
            let mut item = BTreeMap::new();
            item.insert("kind".to_string(), VmValue::String(Rc::from(entry.kind)));
            item.insert(
                "content".to_string(),
                VmValue::String(Rc::from(entry.content)),
            );
            item.insert(
                "source".to_string(),
                VmValue::String(Rc::from(entry.source)),
            );
            item.insert("sequence".to_string(), VmValue::Int(entry.sequence as i64));
            item.insert("ts_ms".to_string(), VmValue::Int(entry.ts_ms));
            VmValue::Dict(Rc::new(item))
        })
        .collect::<Vec<_>>();
    Ok(VmValue::List(Rc::new(drained)))
}

/// Read accumulated token + cost totals for a session.
#[harn_builtin(
    sig = "__host_agent_session_totals(session_id: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_totals_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let totals = with_session(&session_id, HOST_SESSION_TOTALS, |session| {
        Ok((session.tokens_used, session.cost_used))
    })?;
    let mut out = BTreeMap::new();
    out.insert("tokens_used".to_string(), VmValue::Int(totals.0));
    out.insert("cost_usd".to_string(), VmValue::Float(totals.1));
    Ok(VmValue::Dict(Rc::new(out)))
}

/// Append a runtime-feedback note to the session as a synthetic user turn.
#[harn_builtin(
    sig = "__host_agent_session_inject_feedback(session_id: string, kind: string, content: string) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_inject_feedback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
    let content = args.get(2).map(|v| v.display()).unwrap_or_default();
    crate::agent_sessions::inject_message(
        &session_id,
        super::agent_config::agent_feedback_message(&kind, &content),
    )
    .map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Post an event into a running session's `agent_inbox`. Surface for
/// triggers, connectors, and external host integrations that want to
/// nudge a session that's already mid-loop without bypassing the
/// canonical drain-at-turn-boundary path (e.g. "the GitHub PR you
/// were waiting on just merged").
/// Post an event into a running session's agent_inbox. Used by triggers,
/// connectors, and external host integrations to nudge a mid-loop session.
#[harn_builtin(
    sig = "__host_agent_session_post_event(session_id: string, kind: string, content: string, source?: string|nil) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_post_event_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_POST_EVENT}: session_id must be a non-empty string"
            )))
        }
    };
    let kind = match args.get(1) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_POST_EVENT}: kind must be a non-empty string"
            )))
        }
    };
    let content = match args.get(2) {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => serde_json::to_string(&vm_to_json(other)).unwrap_or_default(),
        None => String::new(),
    };
    let source = match args.get(3) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => "host.post_event".to_string(),
    };
    crate::orchestration::agent_inbox::push(&session_id, &kind, &content, &source);
    Ok(VmValue::Nil)
}

/// Apply reminder TTL lifecycle after an agent turn.
#[harn_builtin(
    sig = "__host_agent_session_apply_reminder_post_turn(session_id: string, turn?: dict|nil) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_apply_reminder_post_turn_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_APPLY_REMINDER_POST_TURN}: session_id must be a non-empty string"
            )))
        }
    };
    let turn = args.get(1).and_then(VmValue::as_int).unwrap_or(0);
    let report = crate::agent_sessions::apply_reminder_post_turn(&session_id, turn)
        .map_err(VmError::Runtime)?;
    Ok(crate::stdlib::json_to_vm_value(&report))
}

/// Replace the session's active skill list.
#[harn_builtin(
    sig = "__host_agent_session_set_active_skills(session_id: string, skills: list) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_set_active_skills_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let skills_value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let ids: Vec<String> = list_items(&skills_value)
        .iter()
        .filter_map(|v| dict_get(v, "id").map(|v| v.display()))
        .collect();
    with_session(&session_id, HOST_SESSION_SET_ACTIVE_SKILLS, |session| {
        session.active_skills = ids.clone();
        Ok(())
    })?;
    crate::agent_sessions::set_active_skills(&session_id, ids);
    Ok(VmValue::Nil)
}

/// Return the session's active skill list.
#[harn_builtin(
    sig = "__host_agent_session_active_skills(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_active_skills_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let ids = with_session(&session_id, HOST_SESSION_ACTIVE_SKILLS, |session| {
        Ok(session.active_skills.clone())
    })?;
    let list = ids
        .into_iter()
        .map(|id| {
            let mut entry = BTreeMap::new();
            entry.insert("id".to_string(), VmValue::String(Rc::from(id)));
            VmValue::Dict(Rc::new(entry))
        })
        .collect();
    Ok(VmValue::List(Rc::new(list)))
}

/// Append a skill lifecycle event and notify live agent-event sinks.
#[harn_builtin(
    sig = "__host_agent_session_record_skill_event(session_id: string, kind: string, metadata: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_skill_event_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
    let metadata = args.get(2).cloned().unwrap_or(VmValue::Nil);
    if session_id.is_empty() || kind.is_empty() {
        return Ok(VmValue::Nil);
    }
    let metadata_json = vm_to_json(&metadata);
    let text = metadata_json
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let event = super::helpers::transcript_event(
        &kind,
        "system",
        "internal",
        &text,
        Some(metadata_json.clone()),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;

    let name = metadata_json
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let iteration = metadata_json
        .get("iteration")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    match kind.as_str() {
        "skill_activated" if !name.is_empty() => {
            let reason = metadata_json
                .get("trigger")
                .or_else(|| metadata_json.get("reason"))
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            super::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillActivated {
                session_id,
                skill_name: name,
                iteration,
                reason,
            });
        }
        "skill_deactivated" if !name.is_empty() => {
            super::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillDeactivated {
                session_id,
                skill_name: name,
                iteration,
            });
        }
        "skill_scope_tools" if !name.is_empty() => {
            let allowed_tools = metadata_json
                .get("allowed_tools")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            super::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillScopeTools {
                session_id,
                skill_name: name,
                allowed_tools,
            });
        }
        "skill_narrow" => {
            let string_list = |key: &str| {
                metadata_json
                    .get(key)
                    .and_then(|value| value.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let reason = metadata_json
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            super::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillNarrow {
                session_id,
                reason,
                removed_tools: string_list("removed_tools"),
                remaining_tools: string_list("remaining_tools"),
            });
        }
        _ => {}
    }
    Ok(VmValue::Nil)
}

/// No-op compaction hook; Harn implements compaction via llm_call.
#[harn_builtin(
    sig = "__host_agent_session_compact_if_needed(session_id: string, options: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_compact_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

/// Replace the session's transcript message list (used by Harn-driven auto-compact).
#[harn_builtin(
    sig = "__host_agent_session_replace_messages(session_id: string, messages: list, summary?: any) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_replace_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_REPLACE_MESSAGES}: session_id must be a non-empty string"
            )))
        }
    };
    let messages_json: Vec<serde_json::Value> = match args.get(1) {
        Some(VmValue::List(list)) => list.iter().map(vm_to_json).collect(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_REPLACE_MESSAGES}: messages must be a list"
            )))
        }
    };
    let summary = match args.get(2) {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    crate::agent_sessions::replace_messages_with_summary(
        &session_id,
        &messages_json,
        summary.as_deref(),
    )
    .map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Score skills against the current task context.
#[harn_builtin(
    sig = "__host_skill_score(context: dict, registry: dict, options: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_skill_score(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let context = args.first().cloned().unwrap_or(VmValue::Nil);
    let registry = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let options = args.get(2).cloned().unwrap_or(VmValue::Nil);
    super::skill_score::score_skill_registry(
        &context,
        &registry,
        &options,
        super::current_host_bridge(),
    )
    .await
}

/// Pre-call budget projection hook (returns false for now).
#[harn_builtin(
    sig = "__host_agent_budget_pre_call_blocked(session_id: string, envelope: dict) -> bool",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_budget_pre_call_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(false))
}

/// Emit an agent event and record transcript-backed event types.
#[harn_builtin(
    sig = "__host_agent_emit_event(session_id: string, event_type: string, payload: dict) -> nil",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_emit_event(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_EMIT_EVENT}: session_id must be a non-empty string"
            )))
        }
    };
    let event_type = match args.get(1) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_EMIT_EVENT}: event_type must be a non-empty string"
            )))
        }
    };
    let payload_value = args.get(2).cloned().unwrap_or(VmValue::Nil);
    let payload = vm_to_json(&payload_value);
    let event = build_agent_event(&session_id, &event_type, &payload)?;
    if matches!(
        event_type.as_str(),
        "tool_search_query"
            | "tool_search_result"
            | "typed_checkpoint"
            | "skill_narrow"
            | "agent_loop_stall_warning"
            | "tool_format_override"
            | "tool_call_audit"
            | "budget_exhausted"
            | "budget_circuit_breaker"
            | "loop_checkpoint"
    ) {
        let role = if matches!(
            event_type.as_str(),
            "tool_search_result" | "tool_call_audit"
        ) {
            "tool"
        } else {
            "assistant"
        };
        let transcript_event =
            super::helpers::transcript_event(&event_type, role, "internal", "", Some(payload));
        if crate::agent_sessions::exists(&session_id) {
            crate::agent_sessions::append_event(&session_id, transcript_event)
                .map_err(VmError::Runtime)?;
        }
    }
    crate::llm::agent_runtime::emit_agent_event_with_ctx(Some(&ctx), &event).await;
    Ok(VmValue::Nil)
}

fn build_agent_event(
    session_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<crate::agent_events::AgentEvent, VmError> {
    use crate::agent_events::AgentEvent;
    use crate::llm::receipts::ToolCallReceipt;
    let payload_obj = payload.as_object();
    let get_usize = |key: &str| -> usize {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
    };
    let get_u64 = |key: &str| -> u64 {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    let get_opt_u64 = |key: &str| -> Option<u64> {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
    };
    let get_opt_f64 = |key: &str| -> Option<f64> {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_f64())
    };
    let get_string = |key: &str| -> String {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_opt_string = |key: &str| -> Option<String> {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    match event_type {
        "iteration_start" => Ok(AgentEvent::IterationStart {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            provider: get_string("provider"),
            model: get_string("model"),
        }),
        "iteration_end" => {
            let iteration_info = payload_obj
                .and_then(|m| m.get("iteration_info"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(AgentEvent::IterationEnd {
                session_id: session_id.to_string(),
                iteration: get_usize("iteration"),
                iteration_info,
            })
        }
        "judge_decision" => Ok(AgentEvent::JudgeDecision {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            verdict: get_string("verdict"),
            reasoning: get_string("reasoning"),
            next_step: get_opt_string("next_step"),
            judge_duration_ms: payload_obj
                .and_then(|m| m.get("judge_duration_ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            trigger: get_opt_string("trigger"),
        }),
        "step_judge_decision" => Ok(AgentEvent::StepJudgeDecision {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            verdict: get_string("verdict"),
            reasoning: get_string("reasoning"),
            critique: get_string("critique"),
            confidence: payload_obj
                .and_then(|m| m.get("confidence"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            judge_duration_ms: payload_obj
                .and_then(|m| m.get("judge_duration_ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            vetoed: payload_obj
                .and_then(|m| m.get("vetoed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            skipped: payload_obj
                .and_then(|m| m.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            reason: get_opt_string("reason"),
            on_veto: get_string("on_veto"),
            input_tokens: payload_obj
                .and_then(|m| m.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: payload_obj
                .and_then(|m| m.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cost_usd: payload_obj
                .and_then(|m| m.get("cost_usd"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            provider: get_string("provider"),
            model: get_string("model"),
        }),
        "structural_validator_decision" => Ok(AgentEvent::StructuralValidatorDecision {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            rule: get_string("rule"),
            diagnostic: get_string("diagnostic"),
            recommended_action: get_string("recommended_action"),
            vetoed: payload_obj
                .and_then(|m| m.get("vetoed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            skipped: payload_obj
                .and_then(|m| m.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            reason: get_opt_string("reason"),
            on_failure: get_string("on_failure"),
            attempts: get_usize("attempts"),
            max_attempts: get_usize("max_attempts"),
        }),
        "scope_classifier_verdict" => Ok(AgentEvent::ScopeClassifierVerdict {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            label: get_string("label"),
            original_label: get_string("original_label"),
            confidence: payload_obj
                .and_then(|m| m.get("confidence"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            confidence_threshold: payload_obj
                .and_then(|m| m.get("confidence_threshold"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.65),
            evidence: get_string("evidence"),
            skip_main_turn: payload_obj
                .and_then(|m| m.get("skip_main_turn"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            classifier_kind: get_opt_string("classifier_kind"),
            model: get_opt_string("model"),
            error: get_opt_string("error"),
        }),
        "typed_checkpoint" => Ok(AgentEvent::TypedCheckpoint {
            session_id: session_id.to_string(),
            checkpoint: payload.clone(),
        }),
        "progress_reported" => Ok(AgentEvent::ProgressReported {
            session_id: session_id.to_string(),
            message: get_opt_string("message"),
            entries: payload_obj
                .and_then(|m| m.get("entries"))
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
            replace: payload_obj
                .and_then(|m| m.get("replace"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            metadata: payload_obj
                .and_then(|m| m.get("metadata"))
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        }),
        "budget_exhausted" => Ok(AgentEvent::BudgetExhausted {
            session_id: session_id.to_string(),
            max_iterations: get_usize("max_iterations"),
            kind: get_opt_string("kind"),
            cost_usd: get_opt_f64("cost_usd"),
            wall_clock_ms: get_opt_u64("wall_clock_ms"),
        }),
        "budget_circuit_breaker" => Ok(AgentEvent::BudgetCircuitBreaker {
            session_id: session_id.to_string(),
            kind: get_string("kind"),
            consecutive_count: get_usize("consecutive_count"),
            paused_for_ms: get_u64("paused_for_ms"),
        }),
        "tool_search_query" => Ok(AgentEvent::ToolSearchQuery {
            session_id: session_id.to_string(),
            tool_use_id: get_string("tool_use_id"),
            name: get_string("name"),
            query: payload_obj
                .and_then(|m| m.get("query"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            strategy: get_string("strategy"),
            mode: get_string("mode"),
        }),
        "tool_search_result" => {
            let promoted = payload_obj
                .and_then(|m| m.get("promoted"))
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok(AgentEvent::ToolSearchResult {
                session_id: session_id.to_string(),
                tool_use_id: get_string("tool_use_id"),
                promoted,
                strategy: get_string("strategy"),
                mode: get_string("mode"),
            })
        }
        "skill_narrow" => {
            let string_list = |key: &str| {
                payload_obj
                    .and_then(|m| m.get(key))
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Ok(AgentEvent::SkillNarrow {
                session_id: session_id.to_string(),
                reason: get_string("reason"),
                removed_tools: string_list("removed_tools"),
                remaining_tools: string_list("remaining_tools"),
            })
        }
        "loop_control_decision" => Ok(AgentEvent::LoopControlDecision {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            action: get_string("action"),
            old_limit: get_usize("old_limit"),
            new_limit: get_usize("new_limit"),
            reason: get_string("reason"),
            status: get_string("status"),
        }),
        "agent_loop_stall_warning" => Ok(AgentEvent::AgentLoopStallWarning {
            session_id: session_id.to_string(),
            warning: payload.clone(),
        }),
        "capability_gap" => Ok(AgentEvent::CapabilityGap {
            session_id: session_id.to_string(),
            level: get_string("level"),
            capability: get_string("capability"),
            provider: get_string("provider"),
            model: get_string("model"),
            fallback_tool_format: get_string("fallback_tool_format"),
            requested_tool_format: get_opt_string("requested_tool_format"),
            message: get_string("message"),
        }),
        "tool_format_override" => Ok(AgentEvent::ToolFormatOverride {
            session_id: session_id.to_string(),
            provider: get_string("provider"),
            model: get_string("model"),
            requested_format: get_string("requested_format"),
            recommended_format: get_string("recommended_format"),
            catalog_parity: get_string("catalog_parity"),
            override_reason: get_opt_string("override_reason"),
        }),
        "tool_call_audit" => {
            let receipt = payload_obj
                .and_then(|m| m.get("receipt"))
                .cloned()
                .map(serde_json::from_value::<ToolCallReceipt>)
                .transpose()
                .map_err(|error| {
                    VmError::Runtime(format!(
                        "{HOST_AGENT_EMIT_EVENT}: invalid tool_call_audit.receipt: {error}"
                    ))
                })?;
            Ok(AgentEvent::ToolCallAudit {
                session_id: session_id.to_string(),
                tool_call_id: get_string("tool_call_id"),
                tool_name: get_string("tool_name"),
                audit: payload_obj
                    .and_then(|m| m.get("audit"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                receipt,
            })
        }
        "loop_checkpoint" => Ok(AgentEvent::LoopCheckpoint {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            kind: get_string("kind"),
            delivered: get_usize("delivered"),
            inbox_delivered: get_usize("inbox_delivered"),
            dispatch_skipped: payload_obj
                .and_then(|m| m.get("dispatch_skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }),
        "cache_hit" => Ok(AgentEvent::CacheHit {
            session_id: session_id.to_string(),
            key: get_string("key"),
            backend: get_string("backend"),
            namespace: get_string("namespace"),
            payload: payload.clone(),
        }),
        "cache_miss" => Ok(AgentEvent::CacheMiss {
            session_id: session_id.to_string(),
            key: get_string("key"),
            backend: get_string("backend"),
            namespace: get_string("namespace"),
            payload: payload.clone(),
        }),
        // `completion_confirmation_nudge` is the engine-side recovery
        // signal for the QwenLM/Qwen3.6 #89 class of regressions where a
        // thinking-enabled native-tool model narrates tool intent in its
        // reasoning trace but emits no `tool_calls`. The engine asks the
        // model to either call a tool or restate its final answer; the
        // event surfaces that nudge to operators alongside the standard
        // FeedbackInjected stream (engine-injected, not user-injected).
        "completion_confirmation_nudge" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "completion_confirmation_nudge".to_string(),
            content: get_string("visible_text_prefix"),
        }),
        other => Err(VmError::Runtime(format!(
            "{HOST_AGENT_EMIT_EVENT}: unsupported event type `{other}`"
        ))),
    }
}

/// Record a native→text tool-call fallback as a transcript event and trace counter.
#[harn_builtin(
    sig = "__host_agent_record_native_tool_fallback(session_id: string, payload: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_record_native_tool_fallback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK}: session_id must be a non-empty string"
            )))
        }
    };
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let payload_json = vm_to_json(&payload);
    let accepted = payload_json
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let policy = payload_json
        .get("policy")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let fallback_index = payload_json
        .get("fallback_index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let tool_call_count = payload_json
        .get("tool_call_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let iteration = payload_json
        .get("iteration")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    super::trace::emit_agent_event(super::trace::AgentTraceEvent::NativeToolFallback {
        iteration,
        accepted,
        policy,
        fallback_index,
        tool_call_count,
    });
    let event = super::helpers::transcript_event(
        "native_tool_fallback",
        "assistant",
        "internal",
        "",
        Some(payload_json),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Record a transcript compaction as a transcript event and trace counter.
#[harn_builtin(
    sig = "__host_agent_record_compaction(session_id: string, payload: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_record_compaction_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_RECORD_COMPACTION}: session_id must be a non-empty string"
            )))
        }
    };
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let payload_json = vm_to_json(&payload);
    let archived_messages = payload_json
        .get("archived_messages")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let new_summary_len = payload_json
        .get("new_summary_len")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let iteration = payload_json
        .get("iteration")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let mode = payload_json
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("auto")
        .to_string();
    let reason = payload_json
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("threshold")
        .to_string();
    let strategy = payload_json
        .get("strategy")
        .or_else(|| payload_json.get("engine_strategy"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let estimated_tokens_before = payload_json
        .get("estimated_tokens_before")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let estimated_tokens_after = payload_json
        .get("estimated_tokens_after")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let snapshot_asset_id = payload_json
        .get("snapshot_asset_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let instruction_mode = payload_json
        .get("instruction_mode")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let instruction_source = payload_json
        .get("instruction_source")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let compaction_policy = payload_json.get("compaction_policy").cloned();
    super::trace::emit_agent_event(super::trace::AgentTraceEvent::ContextCompaction {
        archived_messages,
        new_summary_len,
        iteration,
    });
    let event = super::helpers::transcript_event(
        "compaction",
        "system",
        "internal",
        "",
        Some(payload_json),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;
    crate::llm::emit_live_agent_event_sync(&crate::agent_events::AgentEvent::TranscriptCompacted {
        session_id,
        mode,
        reason,
        strategy,
        archived_messages,
        estimated_tokens_before,
        estimated_tokens_after,
        snapshot_asset_id,
        instruction_mode,
        instruction_source,
        compaction_policy,
    });
    Ok(VmValue::Nil)
}

/// Project the session transcript through a policy, append a
/// transcript.projection event, and return the projected messages
/// with metadata.
#[harn_builtin(
    sig = "__host_agent_session_project_turn(session_id: string, options?: dict|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_project_turn(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_PROJECT_TURN}: session_id must be a non-empty string"
            )))
        }
    };
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let policy = crate::stdlib::transcript_project::parse_projection_options(&options)?;
    let Some(transcript) = crate::agent_sessions::transcript(&session_id) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PROJECT_TURN}: unknown agent session `{session_id}`"
        )));
    };
    let transcript_dict = transcript.as_dict().cloned().unwrap_or_default();
    let result = crate::stdlib::transcript_project::project_transcript(
        Some(&ctx),
        &transcript_dict,
        &policy,
    )
    .await?;
    let event = crate::stdlib::transcript_project::projection_event_value(&result, &policy);
    let _ = crate::agent_sessions::append_event(&session_id, event.clone());
    crate::llm::emit_live_agent_event_with_ctx(
        Some(&ctx),
        &AgentEvent::TranscriptProjected {
            session_id: session_id.clone(),
            policy: policy.kind.as_str().to_string(),
            reason: result.reason.clone(),
            prefix_hash: result.prefix_hash.clone(),
            kept_count: result.kept_indices.len(),
            dropped_count: result.dropped_indices.len(),
            provider_safety_blocked: result.provider_safety_blocked,
        },
    )
    .await;
    Ok(crate::stdlib::transcript_project::result_to_vm(
        &result, &policy,
    ))
}

/// Claim the session's tool_format contract; rejects mid-session changes.
#[harn_builtin(
    sig = "__host_agent_session_claim_tool_format(session_id: string, tool_format: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_claim_tool_format_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_CLAIM_TOOL_FORMAT}: session_id must be a non-empty string"
            )))
        }
    };
    let tool_format = match args.get(1) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_CLAIM_TOOL_FORMAT}: tool_format must be a non-empty string"
            )))
        }
    };
    crate::agent_sessions::claim_tool_format(&session_id, &tool_format)
        .map_err(VmError::Runtime)?;
    with_session(&session_id, HOST_SESSION_CLAIM_TOOL_FORMAT, |session| {
        session.tool_mode = tool_format.clone();
        Ok(())
    })?;
    Ok(VmValue::Nil)
}

/// Persist a daemon snapshot for a Harn-driven agent session.
#[harn_builtin(
    sig = "__host_agent_daemon_snapshot(session_id: string, options: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_daemon_snapshot_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let opts_map = opts_dict(args.get(1));
    let config = super::daemon::parse_daemon_loop_config(Some(&opts_map));
    let daemon_state = opt_str(&opts_map, "daemon_state").unwrap_or_else(|| "idle".to_string());
    let total_iterations = opt_int(&opts_map, "total_iterations").unwrap_or(0).max(0) as usize;
    let transcript_summary_override = opt_str(&opts_map, "transcript_summary");
    let transcript = crate::agent_sessions::transcript(&session_id).unwrap_or(VmValue::Nil);
    let transcript_json = vm_to_json(&transcript);
    let visible_messages = transcript_json
        .get("messages")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let recorded_messages = visible_messages.clone();
    let transcript_events = transcript_json
        .get("events")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let transcript_summary = transcript_summary_override.or_else(|| {
        transcript_json
            .get("summary")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
    });
    let total_text = visible_messages
        .iter()
        .filter_map(|message| message.get("content").and_then(|value| value.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let last_iteration_text = visible_messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(|value| value.as_str()) == Some("assistant"))
        .and_then(|message| message.get("content").and_then(|value| value.as_str()))
        .unwrap_or_default()
        .to_string();

    let mut snapshot = super::daemon::DaemonSnapshot {
        daemon_state: daemon_state.clone(),
        visible_messages,
        recorded_messages,
        transcript_summary,
        transcript_events,
        total_text,
        last_iteration_text,
        total_iterations,
        ..Default::default()
    }
    .normalize();

    let (snapshot_path, idle_backoff_ms) =
        with_session(&session_id, HOST_DAEMON_SNAPSHOT, |session| {
            if session.daemon_watch_state.is_empty() && !config.watch_paths.is_empty() {
                session.daemon_watch_state = super::daemon::watch_state(
                    &super::daemon::RealMtimeProvider,
                    &config.watch_paths,
                );
            }
            snapshot.all_tools_used = session.successful_tools.clone();
            snapshot.rejected_tools = session.rejected_tools.clone();
            snapshot.total_iterations = snapshot
                .total_iterations
                .saturating_add(session.resumed_iterations);
            snapshot.idle_backoff_ms = session.daemon_idle_backoff_ms;
            snapshot.watch_state = session.daemon_watch_state.clone();
            let snapshot_path = if let Some(path) = config.effective_persist_path() {
                Some(super::daemon::persist_snapshot(path, &snapshot)?)
            } else {
                None
            };
            session.daemon_state = Some(daemon_state.clone());
            if let Some(path) = snapshot_path.as_ref() {
                session.daemon_snapshot_path = Some(path.clone());
            }
            Ok((snapshot_path, session.daemon_idle_backoff_ms))
        })?;

    let mut value = serde_json::to_value(&snapshot).unwrap_or_default();
    value["daemon_snapshot_path"] = snapshot_path
        .as_ref()
        .map(|path| serde_json::json!(path))
        .unwrap_or(serde_json::Value::Null);
    value["idle_backoff_ms"] = serde_json::json!(idle_backoff_ms);
    Ok(json_to_vm(&value))
}

fn host_bridge_for_session(
    session_id: &str,
    builtin_name: &str,
) -> Option<Rc<crate::bridge::HostBridge>> {
    with_session(session_id, builtin_name, |session| {
        Ok(session.host_bridge.clone())
    })
    .ok()
    .flatten()
    .or_else(super::agent_runtime::current_host_bridge)
}

fn bridge_delivery_checkpoint(value: &str) -> Result<crate::bridge::DeliveryCheckpoint, VmError> {
    match value {
        "interrupt_immediate" | "interrupt" => {
            Ok(crate::bridge::DeliveryCheckpoint::InterruptImmediate)
        }
        "finish_step" | "after_current_operation" => {
            Ok(crate::bridge::DeliveryCheckpoint::AfterCurrentOperation)
        }
        "audit_only" | "end_of_interaction" => {
            Ok(crate::bridge::DeliveryCheckpoint::EndOfInteraction)
        }
        other => Err(VmError::Runtime(format!(
            "{HOST_SESSION_DRAIN_BRIDGE_INJECTIONS}: unsupported checkpoint `{other}`"
        ))),
    }
}

async fn drain_bridge_injections_for_checkpoint(
    session_id: &str,
    bridge: &crate::bridge::HostBridge,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<(usize, Option<&'static str>), VmError> {
    let queued = bridge
        .take_queued_transcript_injections_for(checkpoint)
        .await;
    if queued.is_empty() {
        return Ok((0, None));
    }
    let mut saw_user_message = false;
    let mut saw_reminder = false;
    let mut delivered = 0;
    for injection in queued {
        match injection {
            crate::bridge::QueuedTranscriptInjection::User(message) => {
                crate::agent_sessions::inject_message(
                    session_id,
                    json_to_vm(&serde_json::json!({
                        "role": "user",
                        "content": message.transcript_content,
                        "messageId": message.message_id,
                    })),
                )
                .map_err(VmError::Runtime)?;
                saw_user_message = true;
                delivered += 1;
            }
            crate::bridge::QueuedTranscriptInjection::Reminder(reminder) => {
                crate::agent_sessions::inject_reminder(session_id, reminder.reminder)
                    .map_err(VmError::Runtime)?;
                saw_reminder = true;
                delivered += 1;
            }
        }
    }
    if saw_user_message {
        Ok((delivered, Some("message")))
    } else if saw_reminder {
        Ok((delivered, Some("reminder")))
    } else {
        Ok((delivered, None))
    }
}

/// Drain `interrupt_immediate` injections on behalf of the daemon idle
/// path and emit a `LoopCheckpoint` so the rest of the seam catalog
/// (Harn-side `__agent_loop_checkpoint`, ACP `loop_checkpoint`
/// notifications, debugger views) sees daemon-side activity through the
/// same surface. The actual drain reuses the bridge primitive; this
/// wrapper adds daemon-specific observability while preserving the
/// caller's Harn callback context for live subscribers.
async fn daemon_checkpoint_drain(
    ctx: &crate::vm::AsyncBuiltinCtx,
    session_id: &str,
    bridge: &crate::bridge::HostBridge,
    kind: &'static str,
) -> Result<(usize, Option<&'static str>), VmError> {
    let (delivered, reason) = drain_bridge_injections_for_checkpoint(
        session_id,
        bridge,
        crate::bridge::DeliveryCheckpoint::InterruptImmediate,
    )
    .await?;
    let event = crate::agent_events::AgentEvent::LoopCheckpoint {
        session_id: session_id.to_string(),
        iteration: 0,
        kind: kind.to_string(),
        delivered,
        inbox_delivered: 0,
        dispatch_skipped: false,
    };
    super::agent_runtime::emit_agent_event_with_ctx(Some(ctx), &event).await;
    Ok((delivered, reason))
}

/// Push a system-reminder onto the session's host bridge queue. The
/// inverse of `__host_agent_session_drain_bridge_injections` — exposed
/// so a Harn script driving the loop (custom CLI host, conformance
/// test, etc.) can queue an injection that lands at the next eligible
/// checkpoint without going through ACP.
///
/// Expects `options` shaped like the `session/remind` JSON-RPC params
/// (`body`, `mode`, optional `tags`, `dedupe_key`, `ttl_turns`,
/// `role_hint`, `propagate`, `preserve_on_compact`). Returns the
/// reminder id so callers can correlate with later
/// `ReminderEmitted` events.
/// Push a system-reminder onto the session's host bridge queue;
/// returns the reminder id. Inverse of drain_bridge_injections.
#[harn_builtin(
    sig = "__host_agent_session_push_bridge_injection(session_id: string, options: dict) -> string",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_push_bridge_injection(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: session_id must be a non-empty string"
        )));
    }
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let params = vm_to_json(&options);
    if !params.is_object() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: options must be a dict"
        )));
    }
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PUSH_BRIDGE_INJECTION)
    else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_BRIDGE_INJECTION}: no host bridge attached to session `{session_id}`"
        )));
    };
    let reminder_id = bridge
        .push_queued_session_remind_from_params(&params)
        .await
        .map_err(|message| {
            VmError::Runtime(format!("{HOST_SESSION_PUSH_BRIDGE_INJECTION}: {message}"))
        })?;
    Ok(VmValue::String(Rc::from(reminder_id)))
}

/// Return a FIFO snapshot of pending bridge user-message and reminder injections.
#[harn_builtin(
    sig = "__host_agent_session_pending_injections(session_id: string) -> list",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_pending_injections(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PENDING_INJECTIONS}: session_id must be a non-empty string"
        )));
    }
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PENDING_INJECTIONS) else {
        return Ok(json_to_vm(&serde_json::json!({
            "pendingCount": 0,
            "injections": [],
        })));
    };
    Ok(json_to_vm(&bridge.pending_injections_json().await))
}

/// Revoke a queued bridge reminder before an agent checkpoint drains it.
#[harn_builtin(
    sig = "__host_agent_session_revoke_reminder(session_id: string, reminder_id: string) -> bool",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_revoke_reminder(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_REVOKE_REMINDER}: session_id must be a non-empty string"
        )));
    }
    let reminder_id = args.get(1).map(|v| v.display()).unwrap_or_default();
    if reminder_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_REVOKE_REMINDER}: reminder_id must be a non-empty string"
        )));
    }
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_REVOKE_REMINDER) else {
        return Ok(json_to_vm(&serde_json::json!({
            "status": "unknown_reminder_id",
            "reminderId": reminder_id,
        })));
    };
    let status = match bridge.revoke_pending_reminder(&reminder_id).await {
        crate::bridge::PendingReminderMutationResult::Mutated => "revoked",
        crate::bridge::PendingReminderMutationResult::AlreadyRevoked => "already_revoked",
        crate::bridge::PendingReminderMutationResult::AlreadyDelivered => "already_delivered",
        crate::bridge::PendingReminderMutationResult::UnknownReminderId => "unknown_reminder_id",
    };
    Ok(json_to_vm(&serde_json::json!({
        "status": status,
        "reminderId": reminder_id,
    })))
}

/// Drain queued bridge transcript injections for a delivery checkpoint.
#[harn_builtin(
    sig = "__host_agent_session_drain_bridge_injections(session_id: string, checkpoint: dict) -> list",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_drain_bridge_injections(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_DRAIN_BRIDGE_INJECTIONS}: session_id must be a non-empty string"
        )));
    }
    let checkpoint_arg = args
        .get(1)
        .map(|value| value.display())
        .unwrap_or_else(|| "finish_step".to_string());
    let checkpoint = bridge_delivery_checkpoint(&checkpoint_arg)?;
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_DRAIN_BRIDGE_INJECTIONS)
    else {
        return Ok(json_to_vm(&serde_json::json!({
            "delivered": 0,
            "reason": "none",
        })));
    };
    let (delivered, reason) =
        drain_bridge_injections_for_checkpoint(&session_id, &bridge, checkpoint).await?;
    Ok(json_to_vm(&serde_json::json!({
        "delivered": delivered,
        "reason": reason.unwrap_or("none"),
    })))
}

/// Wait for daemon wake input or a timeout.
#[harn_builtin(
    sig = "__host_agent_daemon_wait(session_id: string, timeout_ms: int) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_daemon_wait(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let timeout_ms = args
        .get(1)
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64)
        .unwrap_or(0);
    let bridge = host_bridge_for_session(&session_id, HOST_DAEMON_WAIT);
    let has_bridge = bridge.is_some();
    if let Some(bridge) = bridge.as_ref() {
        bridge.set_daemon_idle(true);
        bridge.notify(
            "agent/idle",
            serde_json::json!({"session_id": session_id, "timeout_ms": timeout_ms}),
        );
        if bridge.take_resume_signal() {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": "resume"})));
        }
        let (_, reason) =
            daemon_checkpoint_drain(&ctx, &session_id, bridge, "daemon_idle_pre").await?;
        if let Some(reason) = reason {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": reason})));
        }
    }

    if timeout_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
    }

    if let Some(bridge) = bridge.as_ref() {
        let (_, reason) =
            daemon_checkpoint_drain(&ctx, &session_id, bridge, "daemon_idle_post").await?;
        if let Some(reason) = reason {
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": reason})));
        }
        bridge.set_daemon_idle(false);
    }

    if timeout_ms > 0 && !has_bridge {
        Ok(json_to_vm(&serde_json::json!({
            "reason": "timer",
            "feedback_kind": "timer",
            "feedback": "Daemon wake interval elapsed.",
        })))
    } else {
        Ok(json_to_vm(&serde_json::json!({"reason": nil_json()})))
    }
}

fn nil_json() -> serde_json::Value {
    serde_json::Value::Null
}

/// Check per-agent autonomy budget and return an approval-shaped denial.
#[harn_builtin(
    sig = "__host_autonomy_budget_check(session_id: string, budget_config: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_autonomy_budget_check(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("agent_session_{}", now_id()));
    let mut opts = BTreeMap::new();
    if let Some(config) = args.get(1) {
        opts.insert("autonomy_budget".to_string(), config.clone());
    }
    match check_autonomy_budget(&opts, &session_id).await? {
        AutonomyCheck::Denied(result) => {
            let mut out = BTreeMap::new();
            out.insert("approved".to_string(), VmValue::Bool(false));
            out.insert("denial_result".to_string(), result);
            Ok(VmValue::Dict(Rc::new(out)))
        }
        AutonomyCheck::Approved(_) | AutonomyCheck::NoBudget => {
            let mut out = BTreeMap::new();
            out.insert("approved".to_string(), VmValue::Bool(true));
            Ok(VmValue::Dict(Rc::new(out)))
        }
    }
}

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
    opts_map: &BTreeMap<String, VmValue>,
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

fn install_session_policies_inner(
    opts_map: &BTreeMap<String, VmValue>,
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

    if let Some(permissions) = permissions::parse_dynamic_permission_policy(
        opts_map.get("permissions"),
        "agent_loop.permissions",
    )? {
        permissions::push_dynamic_permission_policy(permissions);
        installed.pushed_permissions = true;
    }

    Ok(())
}

fn release_session_policies(installed: &InstalledPolicies) {
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
fn install_session_nested_budget(
    opts_map: &BTreeMap<String, VmValue>,
    session_id: &str,
) -> Result<NestedExecutionGuard, VmError> {
    let requested = parse_capability_policy(opts_map.get("policy"))?;
    let kind =
        NestedExecutionKind::parse_or_default(opts_map.get(NESTED_KIND_OPTION_KEY).and_then(|v| {
            match v {
                VmValue::String(text) => Some(text.as_ref()),
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
fn build_nested_budget_denial(session_id: &str, prompt: &str, error: &VmError) -> VmValue {
    let (message, category) = match error {
        VmError::CategorizedError { message, category } => (message.clone(), category.as_str()),
        other => (other.to_string(), "tool_rejected"),
    };
    let _ = crate::agent_sessions::append_event(
        session_id,
        super::helpers::transcript_event(
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
        "final_status": "blocked",
        "stop_reason": "nested_execution_budget_exhausted",
        "error": {
            "category": category,
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

const HOST_SESSION_BUILTINS: &[&VmBuiltinDef] = &[
    // sync
    &HOST_AGENT_SESSION_MESSAGES_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_ASSISTANT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_POP_LAST_ASSISTANT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_TOOL_RESULTS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_USAGE_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_TOTALS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_INJECT_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_POST_EVENT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_APPLY_REMINDER_POST_TURN_BUILTIN_DEF,
    &HOST_AGENT_SESSION_SET_ACTIVE_SKILLS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_ACTIVE_SKILLS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_SKILL_EVENT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_COMPACT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_REPLACE_MESSAGES_BUILTIN_DEF,
    &HOST_AGENT_BUDGET_PRE_CALL_BUILTIN_DEF,
    &HOST_AGENT_DAEMON_SNAPSHOT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_CLAIM_TOOL_FORMAT_BUILTIN_DEF,
    &HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK_BUILTIN_DEF,
    &HOST_AGENT_RECORD_COMPACTION_BUILTIN_DEF,
    // async
    &HOST_AGENT_SESSION_INIT_DEF,
    &HOST_AGENT_SESSION_FINALIZE_DEF,
    &HOST_AGENT_EMIT_EVENT_DEF,
    &HOST_SKILL_SCORE_DEF,
    &HOST_AUTONOMY_BUDGET_CHECK_DEF,
    &HOST_AGENT_SESSION_DRAIN_BRIDGE_INJECTIONS_DEF,
    &HOST_AGENT_SESSION_PUSH_BRIDGE_INJECTION_DEF,
    &HOST_AGENT_SESSION_PENDING_INJECTIONS_DEF,
    &HOST_AGENT_SESSION_REVOKE_REMINDER_DEF,
    &HOST_AGENT_DAEMON_WAIT_DEF,
    &HOST_AGENT_SESSION_PROJECT_TURN_DEF,
];

pub fn register_agent_session_host_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, HOST_SESSION_BUILTINS);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        assistant_message_from_llm_result, canonical_acp_stop_reason,
        canonical_provider_stop_reason, initial_user_content, last_assistant_text,
        tool_result_message_for_provider, vm_to_json,
    };
    use std::collections::BTreeMap;

    #[test]
    fn native_tool_calls_replay_with_openai_wire_shape() {
        let result = crate::stdlib::json_to_vm_value(&json!({
            "provider": "local",
            "text": "",
            "native_tool_calls": [{
                "id": "call_001",
                "name": "release_run",
                "arguments": {"command": "git status --short"}
            }],
        }));
        let message = vm_to_json(&assistant_message_from_llm_result(&result));

        assert_eq!(message["role"], "assistant");
        assert_eq!(message["tool_calls"][0]["id"], "call_001");
        assert_eq!(message["tool_calls"][0]["type"], "function");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "release_run");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            r#"{"command":"git status --short"}"#
        );
    }

    #[test]
    fn initial_user_content_preserves_multimodal_blocks() {
        let mut opts = BTreeMap::new();
        opts.insert(
            "initial_user_content".to_string(),
            crate::stdlib::json_to_vm_value(&json!([
                {"type": "text", "text": "Describe this image."},
                {
                    "type": "image",
                    "media_type": "image/png",
                    "base64": "aGVsbG8="
                }
            ])),
        );

        let content = initial_user_content(&opts, "Describe this image.");

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["base64"], "aGVsbG8=");
    }

    #[test]
    fn initial_user_content_falls_back_to_text_message() {
        let opts = BTreeMap::new();

        assert_eq!(
            initial_user_content(&opts, "hello"),
            serde_json::Value::String("hello".to_string())
        );
    }

    #[test]
    fn tool_results_replay_with_provider_appropriate_ids() {
        let local = vm_to_json(&tool_result_message_for_provider(
            "local",
            "Qwen/Qwen3.6-35B-A3B",
            "native",
            "release_run",
            "call_001",
            "ok",
        ));
        assert_eq!(local["role"], "tool");
        assert_eq!(local["name"], "release_run");
        assert_eq!(local["tool_call_id"], "call_001");

        let anthropic = vm_to_json(&tool_result_message_for_provider(
            "anthropic",
            "claude-opus-4-7",
            "native",
            "release_run",
            "call_002",
            "ok",
        ));
        assert_eq!(anthropic["role"], "tool_result");
        assert_eq!(anthropic["tool_use_id"], "call_002");

        let bedrock_claude = vm_to_json(&tool_result_message_for_provider(
            "bedrock",
            "anthropic.claude-3-5-sonnet-20240620-v1:0",
            "native",
            "release_run",
            "call_003",
            "ok",
        ));
        assert_eq!(bedrock_claude["role"], "tool_result");
        assert_eq!(bedrock_claude["tool_use_id"], "call_003");

        let gemini = vm_to_json(&tool_result_message_for_provider(
            "gemini",
            "gemini-2.5-flash",
            "native",
            "release_run",
            "call_004",
            "ok",
        ));
        assert_eq!(gemini["role"], "tool");
        assert_eq!(gemini["name"], "release_run");
        assert_eq!(gemini["tool_call_id"], "call_004");

        let text_mode = vm_to_json(&tool_result_message_for_provider(
            "ollama",
            "devstral-small-2:24b",
            "text",
            "release_run",
            "call_005",
            "ok",
        ));
        assert_eq!(text_mode["role"], "user");
        assert!(text_mode.get("tool_call_id").is_none());
        assert!(text_mode.get("tool_use_id").is_none());
    }

    #[test]
    fn final_visible_text_skips_control_only_assistant_turns() {
        let snapshot = crate::stdlib::json_to_vm_value(&json!({
            "messages": [
                {"role": "assistant", "content": "Final answer before sentinel."},
                {"role": "assistant", "content": "\n\n##DONE##"}
            ]
        }));

        assert_eq!(
            last_assistant_text(&snapshot).as_deref(),
            Some("Final answer before sentinel.")
        );
    }

    #[test]
    fn iteration_cap_maps_to_max_turn_requests() {
        assert_eq!(
            canonical_acp_stop_reason("budget_exhausted", 5, 5, None),
            "max_turn_requests"
        );
        assert_eq!(
            canonical_acp_stop_reason("budget_exhausted", 6, 5, Some("end_turn")),
            "max_turn_requests"
        );
    }

    #[test]
    fn other_budget_paths_also_map_to_max_turn_requests() {
        // Token / cost / autonomy budgets all stop the loop short, so
        // they share the canonical ACP reason even when iterations are
        // below the cap.
        assert_eq!(
            canonical_acp_stop_reason("budget_exhausted", 2, 50, Some("end_turn")),
            "max_turn_requests"
        );
    }

    #[test]
    fn provider_max_tokens_promoted_when_loop_clean() {
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("max_tokens")),
            "max_tokens"
        );
        // OpenAI flavor.
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("length")),
            "max_tokens"
        );
        // Case-insensitive on the provider value.
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("MAX_TOKENS")),
            "max_tokens"
        );
    }

    #[test]
    fn provider_stop_reason_normalization_is_shared_with_transcripts() {
        assert_eq!(canonical_provider_stop_reason(Some("length")), "max_tokens");
        assert_eq!(canonical_provider_stop_reason(Some("refusal")), "refusal");
        assert_eq!(canonical_provider_stop_reason(Some("tool_use")), "end_turn");
        assert_eq!(canonical_provider_stop_reason(None), "end_turn");
    }

    #[test]
    fn anthropic_refusal_stop_reason_maps_to_refusal() {
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("refusal")),
            "refusal"
        );
    }

    #[test]
    fn natural_completion_maps_to_end_turn() {
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("end_turn")),
            "end_turn"
        );
        assert_eq!(canonical_acp_stop_reason("", 1, 50, None), "end_turn");
        // Anthropic `tool_use` is normal mid-turn behavior; if it
        // somehow surfaced as the last call's stop_reason (loop ended
        // before the next turn ran), it still represents a clean stop.
        assert_eq!(
            canonical_acp_stop_reason("done", 1, 50, Some("tool_use")),
            "end_turn"
        );
    }

    #[test]
    fn budget_exhausted_overrides_provider_signal() {
        // The loop ran out of budget before the model could refuse or
        // truncate again, so loop-level cap wins.
        assert_eq!(
            canonical_acp_stop_reason("budget_exhausted", 50, 50, Some("max_tokens")),
            "max_turn_requests"
        );
        assert_eq!(
            canonical_acp_stop_reason("budget_exhausted", 50, 50, Some("refusal")),
            "max_turn_requests"
        );
    }
}

#[cfg(test)]
mod nested_budget_tests {
    use super::*;
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, CapabilityPolicy,
    };
    use std::rc::Rc;

    fn policy_value(policy: &CapabilityPolicy) -> VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::to_value(policy).unwrap())
    }

    fn empty_session_id() -> String {
        format!("test_session_{}", uuid::Uuid::now_v7())
    }

    #[test]
    fn install_session_nested_budget_rejects_when_parent_is_zero() {
        clear_execution_policy_stacks();
        let parent = CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        };
        push_execution_policy(parent);

        let opts_map = BTreeMap::new();
        let session_id = empty_session_id();
        let error = install_session_nested_budget(&opts_map, &session_id).unwrap_err();
        match error {
            VmError::CategorizedError { message, category } => {
                assert_eq!(category.as_str(), "budget_exceeded");
                assert!(message.contains("agent_loop"), "missing kind: {message}");
                assert!(message.contains(&session_id), "missing label: {message}");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_decrements_when_parent_has_room() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(3),
            ..Default::default()
        });

        let opts_map = BTreeMap::new();
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        assert_eq!(guard.parent_limit, Some(3));
        assert_eq!(guard.child_limit, Some(2));
        assert_eq!(current_execution_policy().unwrap().recursion_limit, Some(2));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_reads_kind_and_label_from_options() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(0),
            ..Default::default()
        });

        let mut opts_map = BTreeMap::new();
        opts_map.insert(
            "_nested_kind".to_string(),
            VmValue::String(Rc::from("sub_agent_run")),
        );
        opts_map.insert(
            "_nested_label".to_string(),
            VmValue::String(Rc::from("research-worker")),
        );
        let error = install_session_nested_budget(&opts_map, "ignored").unwrap_err();
        match error {
            VmError::CategorizedError { message, .. } => {
                assert!(
                    message.contains("sub_agent_run"),
                    "kind not surfaced: {message}"
                );
                assert!(
                    message.contains("research-worker"),
                    "label not surfaced: {message}"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn install_session_nested_budget_intersects_requested_policy() {
        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(10),
            ..Default::default()
        });

        let mut opts_map = BTreeMap::new();
        opts_map.insert(
            "policy".to_string(),
            policy_value(&CapabilityPolicy {
                recursion_limit: Some(1),
                ..Default::default()
            }),
        );
        let guard = install_session_nested_budget(&opts_map, "child").unwrap();
        // Parent had Some(10); decremented to Some(9). Intersected with
        // the requested ceiling Some(1) yields the tighter Some(1).
        assert_eq!(guard.child_limit, Some(1));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn build_nested_budget_denial_carries_budget_exceeded_category() {
        let error = VmError::CategorizedError {
            message: "nested execution budget exhausted before sub_agent_run: research-worker"
                .to_string(),
            category: crate::value::ErrorCategory::BudgetExceeded,
        };
        let result = build_nested_budget_denial("session-x", "go", &error);
        let json = vm_to_json(&result);
        assert_eq!(json["final_status"], "blocked");
        assert_eq!(json["stop_reason"], "nested_execution_budget_exhausted");
        assert_eq!(json["error"]["category"], "budget_exceeded");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("research-worker"));
        assert_eq!(json["session_id"], "session-x");
        assert_eq!(json["task"], "go");
    }
}
