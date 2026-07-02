//! Host primitives backing the Harn-driven agent loop in
//! `std/agent/loop.harn`.
//!
//! These are CRUD-shaped primitives over per-session host state. The
//! decision logic (iterate, sentinel-check, dispatch tools, judge,
//! continue/break) lives in Harn; Rust is reduced to data plumbing,
//! provider/tool capability surfaces, and resource lifecycle.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

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
const HOST_SESSION_PUSH_USER_MESSAGE: &str = "__host_agent_session_push_user_message";
const HOST_SESSION_PENDING_INJECTIONS: &str = "__host_agent_session_pending_injections";
const HOST_SESSION_REVOKE_REMINDER: &str = "__host_agent_session_revoke_reminder";
const HOST_SESSION_INJECT_REMINDER: &str = "__host_agent_session_inject_reminder";
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
    host_bridge: Option<Arc<crate::bridge::HostBridge>>,
    /// Provider-reported `stop_reason` from the most recent `llm_call`
    /// in this loop. Used by finalize to detect ACP `max_tokens` (when
    /// the last call truncated due to its `max_tokens` parameter) and
    /// `refusal` (Anthropic refusal stop_reason).
    last_llm_stop_reason: Option<String>,
    /// Lethal-trifecta taint ledger: untrusted external content (MCP servers,
    /// `Fetch`-kind tools) that entered this session's context. Owned here so
    /// it drops with the session — no cross-session leak. Read by the dispatch
    /// gate to decide whether exfiltration-capable tools need confirmation.
    taint: Vec<crate::security::TaintRecord>,
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
        const { RefCell::new(std::collections::BTreeMap::new()) };
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

/// Append a taint record to the session's lethal-trifecta ledger. No-op when
/// the session is unknown (e.g. tool results recorded outside a host session).
pub(crate) fn push_session_taint(session_id: &str, record: crate::security::TaintRecord) {
    let _ = with_session(session_id, "record_session_taint", |session| {
        session.taint.push(record);
        Ok(())
    });
}

/// Snapshot the session's taint ledger for the dispatch gate. Empty when the
/// session is unknown or no untrusted content has entered context.
pub(crate) fn session_taint_snapshot(session_id: &str) -> Vec<crate::security::TaintRecord> {
    with_session(session_id, "snapshot_session_taint", |session| {
        Ok(session.taint.clone())
    })
    .unwrap_or_default()
}

fn opts_dict(value: Option<&VmValue>) -> crate::value::DictMap {
    match value {
        Some(VmValue::Dict(d)) => (**d).clone(),
        _ => crate::value::DictMap::new(),
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

fn opt_str(map: &crate::value::DictMap, key: &str) -> Option<String> {
    map.get(key).and_then(|v| match v {
        VmValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

fn opt_int(map: &crate::value::DictMap, key: &str) -> Option<i64> {
    map.get(key).and_then(|v| match v {
        VmValue::Int(i) => Some(*i),
        VmValue::Float(f) => Some(*f as i64),
        _ => None,
    })
}

fn opt_json(map: &crate::value::DictMap, key: &str) -> Option<serde_json::Value> {
    map.get(key)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(vm_to_json)
}

fn initial_user_content(
    opts_map: &crate::value::DictMap,
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
        daemon_watch_state: std::collections::BTreeMap::new(),
        daemon_idle_backoff_ms: 100,
        host_bridge,
        last_llm_stop_reason: None,
        taint: Vec::new(),
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

    let mut control = crate::value::DictMap::new();
    control.put_str("session_id", resolved);
    control.put_str("task", message);
    control.insert(
        crate::value::intern_key("system"),
        system
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    control.insert(
        crate::value::intern_key("max_iterations"),
        VmValue::Int(max_iterations),
    );
    control.insert(
        crate::value::intern_key("max_verify_attempts"),
        VmValue::Int(max_verify_attempts),
    );
    control.insert(crate::value::intern_key("done"), VmValue::Bool(false));
    Ok(VmValue::dict(control))
}

enum AutonomyCheck {
    NoBudget,
    Approved(super::autonomy_budget::AgentAutonomyBudget),
    Denied(VmValue),
}

async fn check_autonomy_budget(
    opts_map: &crate::value::DictMap,
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
        "error" | "failed" | "provider_error" | "verify_exhausted" | "verify_capped" | "stuck"
    )
}

/// Detect a model-less agent turn: the loop finalized a *completed* turn
/// (empty status or `done`) but never actually called the provider. We
/// treat "no iterations AND no tokens recorded for this session" as the
/// signal, since any real provider round-trip increments iterations and
/// records token usage.
///
/// Only the success-completion statuses qualify. Intentional non-terminal
/// states — `suspended`, `blocked`, `paused`, `cancelled`, waitpoints — and
/// already-errored turns legitimately finalize with zero iterations and must
/// be left alone; otherwise we would turn an intentional pause into a
/// spurious failure.
fn agent_turn_made_no_llm_call(
    final_status: &str,
    has_terminal_error: bool,
    iterations: i64,
    input_tokens: i64,
    output_tokens: i64,
) -> bool {
    let is_success_completion = final_status.is_empty() || final_status == "done";
    !has_terminal_error
        && is_success_completion
        && iterations == 0
        && input_tokens == 0
        && output_tokens == 0
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
    let mut control = crate::value::DictMap::new();
    control.put_str("session_id", session_id);
    control.put_str("task", task);
    control.insert(
        crate::value::intern_key("system"),
        system
            .map(|s| VmValue::String(arcstr::ArcStr::from(s.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    control.insert(crate::value::intern_key("max_iterations"), VmValue::Int(0));
    control.insert(
        crate::value::intern_key("max_verify_attempts"),
        VmValue::Int(0),
    );
    control.insert(crate::value::intern_key("done"), VmValue::Bool(true));
    control.insert(crate::value::intern_key("result"), result);
    VmValue::dict(control)
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
    let mut final_status = opt_str(&status_dict, "final_status").unwrap_or_default();
    let mut stop_reason = opt_str(&status_dict, "stop_reason").unwrap_or_default();
    let mut terminal_error = opt_json(&status_dict, "error");
    let iterations = opt_int(&status_dict, "iterations").unwrap_or(0);

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

    // Fail loud on a model-less turn. If the loop finalized a non-error,
    // success-shaped result but never actually called the provider (zero
    // iterations and zero input/output tokens recorded for this session),
    // the turn silently short-circuited — typically because no model
    // resolved or the input was empty. Returning success-with-empty-text
    // here masks a configuration failure and costs hours of forensics, so
    // promote it to a terminal error that flows through the normal
    // SessionError path below.
    if agent_turn_made_no_llm_call(
        &final_status,
        terminal_error.is_some(),
        iterations,
        session.input_tokens,
        session.output_tokens,
    ) {
        terminal_error = Some(serde_json::json!({
            "category": "no_llm_call",
            "message": "agent turn made no LLM call: no model resolved / empty input. \
                        The agent loop completed without ever calling the provider \
                        (0 iterations, 0 tokens). Check that a model is configured and \
                        the prompt is non-empty.",
        }));
        final_status = "error".to_string();
        if stop_reason.is_empty() {
            stop_reason = "no_llm_call".to_string();
        }
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

/// True when a provider stop_reason means "I ran out of output-token budget
/// mid-emit", i.e. the response was cut off rather than completed.
///
/// Keys on the normalized condition, not one wire format: OpenAI / OpenRouter /
/// Ollama (`/v1` finish_reason + native `done_reason`) report `length`;
/// Anthropic reports `max_tokens`. Both canonicalize to `max_tokens` via
/// [`canonical_provider_stop_reason`], so we reuse that mapping as the single
/// source of truth — a new provider that adopts either spelling is covered for
/// free.
pub(crate) fn is_length_truncation(stop_reason: Option<&str>) -> bool {
    canonical_provider_stop_reason(stop_reason) == "max_tokens"
}

/// True when the model's text looks like it was mid-tool-call when the stream
/// was cut off: it contains a text-tool-call opener (the `<tool_call>` tag or a
/// bare `name(` shape) but the turn resolved ZERO usable tool calls. This is the
/// "truncated, unparseable tool call" fingerprint — distinct from a model that
/// simply ran long on prose with no tool intent.
///
/// Deliberately permissive on the *prefix* side (any opener) but strict on the
/// *outcome* side (zero calls dispatched): a turn that landed even one tool call
/// made real progress and is not a truncation casualty.
fn text_has_tool_call_prefix(text: &str) -> bool {
    if text.contains(super::tools::TEXT_TOOL_CALL_OPEN)
        || text.contains(super::tools::TEXT_TOOL_CALL_OPEN_COMPACT)
    {
        return true;
    }
    // Bare `name(` shape at the start of any line — the text-tool wire format
    // the agent loop reads back. We only need a cheap structural sniff here;
    // the authoritative parse already ran and produced zero calls, so this just
    // decides whether continuing is worthwhile.
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(ident) = super::tools::ident_length(trimmed.as_bytes()) {
            if trimmed.as_bytes().get(ident) == Some(&b'(') {
                return true;
            }
        }
    }
    false
}

/// Decide whether the agent loop should AUTO-CONTINUE (re-issue the completion
/// with a raised output cap) instead of burning the turn on parse-guidance.
///
/// Fires only when ALL hold:
///   1. `stop_reason` is a length truncation (the response was cut off), AND
///   2. the turn resolved ZERO usable tool calls, AND
///   3. there is a partial tool-call signal — either the parser emitted a
///      diagnostic (e.g. "unterminated heredoc") or the raw text carries a
///      tool-call opener prefix.
///
/// A clean stop with a genuinely malformed call returns `false` here: that is
/// the parse-tolerance / narration-as-prose domain (#3137) and the
/// reasoning-leak domain (#3142), which this must NOT double-handle. The
/// length-truncation gate is what keeps the two from colliding — those cases
/// stop with `end_turn`/`stop`, never `length`/`max_tokens`.
pub(crate) fn truncated_tool_call_should_continue(
    stop_reason: Option<&str>,
    text: &str,
    tool_call_count: i64,
    has_parse_errors: bool,
) -> bool {
    if !is_length_truncation(stop_reason) {
        return false;
    }
    if tool_call_count > 0 {
        return false;
    }
    has_parse_errors || text_has_tool_call_prefix(text)
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
        .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new())));
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
    let thinking = dict_get(llm_result, "thinking").map(|v| v.display());
    let agent_tool_format = dict_get(llm_result, "_agent_tool_format").map(|v| v.display());
    if native_calls_json.is_empty() {
        // gpt-oss / harmony channel-leak backstop. A native-tools model is
        // supposed to split its harmony channels at the wire: analysis ->
        // `reasoning`, commentary/tool -> `tool_calls`, final -> `content`. On
        // ~23% of gpt-oss-120b turns the provider FAILS to split and collapses
        // the analysis reasoning AND the inline tool-call JSON into a single
        // `content` blob (empty `reasoning` field, empty `tool_calls`). The
        // tagged-parser merge in `vm_build_llm_result` recovers the call into
        // the unified `tool_calls` (the `tool`-key dialect now recovers too,
        // see native_json.rs), but the recovered prose is discarded, so the
        // dirty `text` would still be persisted verbatim. Replaying that raw
        // blob back into history wastes input tokens AND re-feeds the model its
        // own private chain-of-thought (incl. "game the verifier" plans) on
        // every later turn.
        //
        // For a native-tools model the canonical persisted shape is structured
        // `tool_calls` + a private `reasoning` trace + a clean `content` (this
        // is exactly what a NON-leaked gpt-oss turn produces, and what the
        // native-calls-present branch below builds). So we reconstruct that
        // shape: move the leaked blob into the private `reasoning` field (it is
        // analysis CoT, not a committed answer — clean tool-call turns carry no
        // `content`), attach the recovered call to `tool_calls`, and leave
        // `content` empty. The next request's openai-compat wire already strips
        // prior-turn `reasoning` (harn#3319), so nothing dirty is re-fed.
        //
        // Pure text-format models (`native_tools == false`, e.g. local
        // llamacpp) legitimately keep their calls inline in `content` for the
        // NEXT turn's text parser to re-read, so those keep the verbatim-text
        // path below.
        let caps = crate::llm::capabilities::lookup(&provider, &model);
        let native_history_requested = agent_tool_format.as_deref().unwrap_or("native") == "native";
        if native_history_requested && caps.native_tools {
            let recovered_calls = list_items(
                &dict_get(llm_result, "tool_calls")
                    .cloned()
                    .unwrap_or(VmValue::Nil),
            )
            .iter()
            .map(vm_to_json)
            .collect::<Vec<_>>();
            if !recovered_calls.is_empty() {
                // A call was recovered from the dirty content. The accompanying
                // prose is private analysis CoT — route it to `reasoning`
                // (preferring the wire `thinking` field when the provider did
                // populate it) and keep `content` empty, matching a clean turn.
                let reasoning = thinking
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or(text.as_str());
                let msg = build_assistant_response_message(
                    "",
                    &[],
                    &recovered_calls,
                    Some(reasoning),
                    &provider,
                    &model,
                );
                return json_to_vm(&msg);
            }
        }
        let mut msg = crate::value::DictMap::new();
        msg.put_str("role", "assistant");
        msg.put_str("content", text);
        return VmValue::dict(msg);
    }

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
    let mut msg = crate::value::DictMap::new();
    // A text-channel tool_format (`text` or `json`) carries tool results back
    // as an ordinary `user` message — there is no provider tool-result role on
    // the text channel. `native` uses the provider's tool_result/tool role.
    let is_text_channel = matches!(
        crate::llm_config::tool_format_channel(tool_format),
        Some(crate::llm_config::ToolFormatChannel::Text)
    );
    if is_text_channel {
        msg.put_str("role", "user");
    } else if crate::llm::provider::provider_uses_anthropic_messages(provider, model) {
        msg.put_str("role", "tool_result");
        msg.put_str("tool_use_id", tool_call_id);
    } else {
        msg.put_str("role", "tool");
        msg.put_str("name", name);
        if !tool_call_id.is_empty() {
            msg.put_str("tool_call_id", tool_call_id);
        }
    }
    msg.put_str("content", observation);
    VmValue::dict(msg)
}

/// The `(id, name)` of one provider-native tool-call block carried on an
/// assistant message, recovered across the three wire shapes the transcript
/// builder emits (`build_assistant_tool_message`):
///
///   - Anthropic: `content` is a list of blocks; `{type: "tool_use", id, name}`.
///   - OpenAI / Ollama: a top-level `tool_calls` list of
///     `{id, function: {name}}`.
///   - Gemini: `content` list of `{functionCall: {name, id?}}` (id optional).
struct AssistantToolUse {
    id: String,
    name: String,
}

/// Extract every provider-native tool-call block declared on an assistant
/// message, regardless of the provider wire shape it was persisted in. Text-
/// channel turns keep their calls inline in `content` (a plain string), so they
/// carry no structured blocks and yield an empty list — which is exactly why the
/// repair below is a no-op for homogeneous text-format runs.
fn assistant_tool_use_blocks(message: &VmValue) -> Vec<AssistantToolUse> {
    let mut blocks = Vec::new();
    // OpenAI / Ollama: top-level `tool_calls`.
    for call in list_items(
        &dict_get(message, "tool_calls")
            .cloned()
            .unwrap_or(VmValue::Nil),
    ) {
        let id = dict_get(&call, "id")
            .map(|v| v.display())
            .unwrap_or_default();
        let name = dict_get(&call, "name")
            .map(|v| v.display())
            .or_else(|| {
                dict_get(&call, "function").and_then(|f| dict_get(f, "name").map(|v| v.display()))
            })
            .unwrap_or_default();
        blocks.push(AssistantToolUse { id, name });
    }
    // Anthropic / Gemini: structured `content` blocks.
    if let Some(content) = dict_get(message, "content") {
        for block in list_items(content) {
            // Anthropic `tool_use`.
            let block_type = dict_get(&block, "type")
                .map(|v| v.display())
                .unwrap_or_default();
            if block_type == "tool_use" {
                let id = dict_get(&block, "id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let name = dict_get(&block, "name")
                    .map(|v| v.display())
                    .unwrap_or_default();
                blocks.push(AssistantToolUse { id, name });
                continue;
            }
            // Gemini `functionCall` part (id is optional on this wire).
            if let Some(function_call) = dict_get(&block, "functionCall") {
                let id = dict_get(function_call, "id")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let name = dict_get(function_call, "name")
                    .map(|v| v.display())
                    .unwrap_or_default();
                blocks.push(AssistantToolUse { id, name });
            }
        }
    }
    blocks
}

/// The set of `tool_use`/`tool_call` ids that ALREADY have a paired tool-result
/// message somewhere in the transcript. Used so the repair only synthesizes a
/// result for a genuinely orphaned block and stays a no-op when the loop already
/// dispatched (and recorded) the call. Covers both provider tool-result roles
/// (`tool_result`/`tool_use_id`, `tool`/`tool_call_id`) and the text-channel
/// `user` echo (which carries no id, so it never satisfies a native id — correct,
/// since a native tool_use ALWAYS needs a real tool-result role).
fn paired_tool_result_ids(messages: &[VmValue]) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for message in messages {
        let role = dict_get(message, "role")
            .map(|v| v.display())
            .unwrap_or_default();
        if role != "tool_result" && role != "tool" {
            continue;
        }
        let id = dict_get(message, "tool_use_id")
            .or_else(|| dict_get(message, "tool_call_id"))
            .map(|v| v.display())
            .unwrap_or_default();
        if !id.is_empty() {
            ids.insert(id);
        }
    }
    ids
}

/// Synthesize a provider-valid tool-result message for each orphaned
/// `tool_use`/`tool_call` block on `last_assistant`, carrying `feedback` as the
/// observation. Blocks whose id already has a paired result (`already_paired`)
/// are skipped. Returns the messages to append, in block order. Pure over its
/// inputs so the invariant is unit-testable without a live session.
fn synthesize_orphan_tool_results(
    last_assistant: &VmValue,
    provider: &str,
    model: &str,
    tool_format: &str,
    feedback: &str,
    already_paired: &std::collections::BTreeSet<String>,
) -> Vec<VmValue> {
    let mut out = Vec::new();
    for block in assistant_tool_use_blocks(last_assistant) {
        if !block.id.is_empty() && already_paired.contains(&block.id) {
            continue;
        }
        out.push(tool_result_message_for_provider(
            provider,
            model,
            tool_format,
            &block.name,
            &block.id,
            feedback,
        ));
    }
    out
}

/// Repair the transcript invariant that every assistant `tool_use`/`tool_call`
/// block is immediately followed by a matching `tool_result` before the next
/// provider request. The agent loop calls this at every inject site that
/// DECLINES to dispatch an assistant turn's tool calls (native-format fallback
/// reject, all-blank-name drop, parse-error, no-progress nudge) and would
/// otherwise append a bare user-feedback message after an orphaned `tool_use` —
/// which Anthropic rejects with a non-retryable HTTP 400 ("tool_use ids were
/// found without tool_result blocks immediately after"), killing the run.
///
/// The synthesized tool-result carries `feedback` as its observation, so the
/// model still sees the same corrective steering it would have gotten from the
/// user message — just delivered in a provider-valid tool-result envelope that
/// keeps pairing intact.
///
/// Returns the number of orphaned blocks repaired. `0` when the trailing message
/// is not an assistant turn, carries no structured tool_use (e.g. a homogeneous
/// text-format run keeps calls inline in `content`), or every block already has
/// a paired result — so this is a strict no-op for runs that already converge.
pub(crate) fn pair_orphaned_tool_use(session_id: &str, feedback: &str) -> usize {
    let Some(transcript) = crate::agent_sessions::transcript(session_id) else {
        return 0;
    };
    let messages = list_items(
        &dict_get(&transcript, "messages")
            .cloned()
            .unwrap_or(VmValue::Nil),
    );
    let Some(last) = messages.last() else {
        return 0;
    };
    let role = dict_get(last, "role")
        .map(|v| v.display())
        .unwrap_or_default();
    if role != "assistant" {
        return 0;
    }
    let (provider, model) = with_session(session_id, "pair_orphaned_tool_use", |session| {
        Ok((
            session.last_provider.clone().unwrap_or_default(),
            session.last_model.clone().unwrap_or_default(),
        ))
    })
    .unwrap_or_default();
    let tool_format = crate::agent_sessions::tool_format(session_id).unwrap_or_default();
    let already_paired = paired_tool_result_ids(&messages);
    let synthetic = synthesize_orphan_tool_results(
        last,
        &provider,
        &model,
        &tool_format,
        feedback,
        &already_paired,
    );
    let mut repaired = 0;
    for message in synthetic {
        if crate::agent_sessions::inject_message(session_id, message).is_ok() {
            repaired += 1;
        }
    }
    repaired
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
    let security_policy = crate::security::current_policy();
    let mut successful = Vec::new();
    let mut rejected = Vec::new();
    for result in list_items(&results_value).iter() {
        let name = dict_get(result, "tool_name")
            .or_else(|| dict_get(result, "name"))
            .map(|v| v.display())
            .unwrap_or_default();
        let raw_observation = dict_get(result, "observation")
            .or_else(|| dict_get(result, "rendered_result"))
            .or_else(|| dict_get(result, "output"))
            .or_else(|| dict_get(result, "content"))
            .map(|v| v.display())
            .unwrap_or_default();
        // Provenance / spotlighting (Layer 0): tag content that crossed a trust
        // boundary (external MCP server, internet fetch) and frame it as data,
        // not instructions, before it reaches the model's context. Skipped
        // entirely when security is disabled.
        let provenance = if security_policy.is_off() {
            None
        } else {
            crate::security::classify_result_trust(
                dict_get(result, "executor"),
                crate::orchestration::current_tool_annotations(&name).as_ref(),
                &name,
                &security_policy,
            )
        };
        let observation = match &provenance {
            Some((trust, origin)) if security_policy.spotlight_external => {
                crate::security::spotlight_wrap(
                    &raw_observation,
                    origin,
                    *trust,
                    security_policy.mode,
                )
            }
            _ => raw_observation.clone(),
        };
        let tool_call_id = dict_get(result, "tool_call_id")
            .or_else(|| dict_get(result, "tool_use_id"))
            .map(|v| v.display())
            .unwrap_or_default();
        let ok = match dict_get(result, "ok") {
            Some(VmValue::Bool(value)) => *value,
            _ => match dict_get(result, "success") {
                Some(VmValue::Bool(value)) => *value,
                _ => match dict_get(result, "status") {
                    Some(VmValue::String(s)) => s.as_str() == "ok",
                    _ => true,
                },
            },
        };
        if ok {
            successful.push(name.clone());
        } else {
            rejected.push(name.clone());
        }
        // Lethal-trifecta ledger (Layer 1): note that untrusted content entered
        // this session's context so the dispatch gate can require confirmation
        // before an exfiltration-capable tool runs.
        if let Some((trust, origin)) = &provenance {
            if trust.is_untrusted() && !raw_observation.is_empty() {
                push_session_taint(
                    &session_id,
                    crate::security::TaintRecord {
                        origin: origin.clone(),
                        trust: *trust,
                        introduced_by: if tool_call_id.is_empty() {
                            name.clone()
                        } else {
                            tool_call_id.clone()
                        },
                        // Layer 2: score the untrusted content with the active
                        // injection classifier when detection is enabled
                        // (`local-ml` mode, or an explicit opt-in). The neural
                        // backend (if the host installed a loader) is materialized
                        // lazily on this first scored span; otherwise the
                        // dependency-free heuristic runs.
                        detector: if security_policy.detect_injection {
                            crate::security::ensure_neural_classifier(&security_policy.guard_model);
                            Some(crate::security::classify_injection(
                                &raw_observation,
                                security_policy.guard_threshold_percent,
                            ))
                        } else {
                            None
                        },
                        labels: crate::security::content_labels(&raw_observation),
                    },
                );
            }
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

/// Synthesize a matching tool-result for each orphaned `tool_use`/`tool_call`
/// block on the trailing assistant turn, carrying `feedback` as the observation,
/// so a subsequent user-feedback inject never leaves the block unpaired. Returns
/// the number of blocks repaired (`0` = no-op: not an assistant turn, no
/// structured tool calls, or already paired). See `pair_orphaned_tool_use`.
#[harn_builtin(
    sig = "__host_agent_session_pair_orphaned_tool_use(session_id: string, feedback: string) -> int",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_pair_orphaned_tool_use_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let feedback = args.get(1).map(|v| v.display()).unwrap_or_default();
    let repaired = pair_orphaned_tool_use(&session_id, &feedback);
    Ok(VmValue::Int(repaired as i64))
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
    let mut out = crate::value::DictMap::new();
    out.insert(
        crate::value::intern_key("tokens_used"),
        VmValue::Int(totals.0),
    );
    out.insert(
        crate::value::intern_key("cost_usd"),
        VmValue::Float(totals.1),
    );
    Ok(VmValue::dict(out))
}

/// Deterministic "should the loop auto-continue this truncated turn?" gate.
///
/// Returns `true` only when the provider cut the response off mid-emit
/// (`stop_reason` is a length truncation), the turn resolved zero usable tool
/// calls, and there is a partial tool-call signal (a parser diagnostic or a
/// tool-call opener in the text). Returns `false` on clean stops — including a
/// cleanly-finished-but-malformed call — so it never overlaps the
/// parse-tolerance (#3137) or reasoning-leak (#3142) paths.
#[harn_builtin(
    sig = "__host_agent_truncated_tool_call(stop_reason: string|nil, text: string, tool_call_count: int, has_parse_errors: bool) -> bool",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_truncated_tool_call_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let stop_reason = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    let text = args.get(1).map(|v| v.display()).unwrap_or_default();
    let tool_call_count = match args.get(2) {
        Some(VmValue::Int(i)) => *i,
        _ => 0,
    };
    let has_parse_errors = matches!(args.get(3), Some(VmValue::Bool(true)));
    Ok(VmValue::Bool(truncated_tool_call_should_continue(
        stop_reason.as_deref(),
        &text,
        tool_call_count,
        has_parse_errors,
    )))
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
            let mut item = crate::value::DictMap::new();
            item.put_str("kind", entry.kind);
            item.put_str("content", entry.content);
            item.put_str("source", entry.source);
            item.insert(
                crate::value::intern_key("sequence"),
                VmValue::Int(entry.sequence as i64),
            );
            item.insert(crate::value::intern_key("ts_ms"), VmValue::Int(entry.ts_ms));
            VmValue::dict(item)
        })
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(drained)))
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
    let mut out = crate::value::DictMap::new();
    out.insert(
        crate::value::intern_key("tokens_used"),
        VmValue::Int(totals.0),
    );
    out.insert(
        crate::value::intern_key("cost_usd"),
        VmValue::Float(totals.1),
    );
    Ok(VmValue::dict(out))
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

/// Inject a single typed system reminder directly into a live session's
/// transcript event stream, bridge-free. The in-process sibling of the
/// `push_bridge_injection` -> `drain_bridge_injections` reminder path: a host
/// that drives the agent loop in-process (no ACP `HostBridge` attached, e.g.
/// Burin's headless/TUI surfaces) can queue a single-turn ephemeral reminder
/// synchronously, the same way `transcript.inject_reminder` does for a
/// transcript value. `options` mirrors that shape — `body` required; optional
/// `tags`, `dedupe_key`, `ttl_turns`, `preserve_on_compact`, `propagate`,
/// `role_hint`. Same-`dedupe_key` reminders are replaced. The reminder renders
/// into the next model prompt and the loop's existing `apply_reminder_post_turn`
/// pass evicts it once its `ttl_turns` reaches zero. Returns the reminder id.
#[harn_builtin(
    sig = "__host_agent_session_inject_reminder(session_id: string, options: dict) -> string",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_inject_reminder_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_INJECT_REMINDER}: session_id must be a non-empty string"
            )))
        }
    };
    let Some(options) = args.get(1).and_then(|value| value.as_dict()) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_INJECT_REMINDER}: options must be a dict with a string `body`"
        )));
    };
    super::conversation::ensure_known_reminder_keys(
        HOST_SESSION_INJECT_REMINDER,
        options,
        super::conversation::INJECT_REMINDER_KEYS,
    )?;
    let reminder =
        super::conversation::parse_inject_reminder_options(options, HOST_SESSION_INJECT_REMINDER)?;
    let report =
        crate::agent_sessions::inject_reminder(&session_id, reminder).map_err(VmError::Runtime)?;
    Ok(VmValue::String(arcstr::ArcStr::from(report.reminder_id)))
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
            let mut entry = crate::value::DictMap::new();
            entry.put_str("id", id);
            VmValue::dict(entry)
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(list)))
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
                policy: metadata_json
                    .get("policy")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                removed_tool_details: metadata_json
                    .get("removed_tool_details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                kept_tool_details: metadata_json
                    .get("kept_tool_details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
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
            | "loop_stuck"
            | "reserved_terminal_verify"
            | "context_overflow_recovery"
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
    use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus, ToolExecutor};
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
    let tool_status = |default: ToolCallStatus| -> Result<ToolCallStatus, VmError> {
        let Some(raw) = get_opt_string("status") else {
            return Ok(default);
        };
        serde_json::from_value::<ToolCallStatus>(serde_json::Value::String(raw.clone())).map_err(
            |error| {
                VmError::Runtime(format!(
                    "{HOST_AGENT_EMIT_EVENT}: invalid tool status `{raw}`: {error}"
                ))
            },
        )
    };
    let tool_error_category = || -> Result<Option<ToolCallErrorCategory>, VmError> {
        let Some(raw) = get_opt_string("error_category") else {
            return Ok(None);
        };
        serde_json::from_value::<ToolCallErrorCategory>(serde_json::Value::String(raw.clone()))
            .map(Some)
            .map_err(|error| {
                VmError::Runtime(format!(
                    "{HOST_AGENT_EMIT_EVENT}: invalid tool error_category `{raw}`: {error}"
                ))
            })
    };
    let tool_executor = || -> Result<Option<ToolExecutor>, VmError> {
        let Some(value) = payload_obj.and_then(|m| m.get("executor")).cloned() else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        if let Some(raw) = value.as_str() {
            let normalized = raw.trim();
            return match normalized {
                "" => Ok(None),
                "harn" | "harn_builtin" => Ok(Some(ToolExecutor::HarnBuiltin)),
                "host" | "host_bridge" => Ok(Some(ToolExecutor::HostBridge)),
                "provider" | "provider_native" => Ok(Some(ToolExecutor::ProviderNative)),
                other => Err(VmError::Runtime(format!(
                    "{HOST_AGENT_EMIT_EVENT}: invalid tool executor `{other}`"
                ))),
            };
        }
        serde_json::from_value::<ToolExecutor>(value)
            .map(Some)
            .map_err(|error| {
                VmError::Runtime(format!(
                    "{HOST_AGENT_EMIT_EVENT}: invalid tool executor: {error}"
                ))
            })
    };
    match event_type {
        "tool_call" => Ok(AgentEvent::ToolCall {
            session_id: session_id.to_string(),
            tool_call_id: get_string("tool_call_id"),
            tool_name: get_string("tool_name"),
            kind: payload_obj
                .and_then(|m| m.get("kind"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| {
                    VmError::Runtime(format!(
                        "{HOST_AGENT_EMIT_EVENT}: invalid tool kind: {error}"
                    ))
                })?,
            status: tool_status(ToolCallStatus::Pending)?,
            raw_input: payload_obj
                .and_then(|m| m.get("raw_input"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            parsing: payload_obj
                .and_then(|m| m.get("parsing"))
                .and_then(|v| v.as_bool()),
            audit: crate::orchestration::current_mutation_session(),
        }),
        "tool_call_update" => Ok(AgentEvent::ToolCallUpdate {
            session_id: session_id.to_string(),
            tool_call_id: get_string("tool_call_id"),
            tool_name: get_string("tool_name"),
            status: tool_status(ToolCallStatus::InProgress)?,
            raw_output: payload_obj.and_then(|m| m.get("raw_output")).cloned(),
            error: get_opt_string("error"),
            duration_ms: get_opt_u64("duration_ms"),
            execution_duration_ms: get_opt_u64("execution_duration_ms"),
            error_category: tool_error_category()?,
            executor: tool_executor()?,
            parsing: payload_obj
                .and_then(|m| m.get("parsing"))
                .and_then(|v| v.as_bool()),
            raw_input: payload_obj.and_then(|m| m.get("raw_input")).cloned(),
            raw_input_partial: get_opt_string("raw_input_partial"),
            audit: crate::orchestration::current_mutation_session(),
        }),
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
            judge_error: payload_obj
                .and_then(|m| m.get("judge_error"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
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
        "missing_tool_call_verdict" => Ok(AgentEvent::MissingToolCallVerdict {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
            action: get_string("action"),
            original_action: get_string("original_action"),
            tool_name: get_string("tool_name"),
            confidence: payload_obj
                .and_then(|m| m.get("confidence"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            confidence_threshold: payload_obj
                .and_then(|m| m.get("confidence_threshold"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.65),
            evidence: get_string("evidence"),
            language: get_opt_string("language"),
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
        "agent_scratchpad_reorganization" => {
            let mut details = payload.clone();
            if let Some(object) = details.as_object_mut() {
                object.remove("iteration");
                object.remove("status");
            }
            Ok(AgentEvent::AgentScratchpadReorganization {
                session_id: session_id.to_string(),
                iteration: get_usize("iteration"),
                status: get_string("status"),
                details,
            })
        }
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
        "loop_stuck" => Ok(AgentEvent::LoopStuckSignal {
            session_id: session_id.to_string(),
            payload: payload.clone(),
        }),
        "reserved_terminal_verify" => Ok(AgentEvent::ReservedTerminalVerify {
            session_id: session_id.to_string(),
            payload: payload.clone(),
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
                policy: payload_obj
                    .and_then(|m| m.get("policy"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                removed_tool_details: payload_obj
                    .and_then(|m| m.get("removed_tool_details"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                kept_tool_details: payload_obj
                    .and_then(|m| m.get("kept_tool_details"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
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
        // Text-mode corrective nudges (cheap/local-model behavioral tips).
        // `fenced_call_attempt_nudge` fires when the model wrapped a tool
        // call in a Markdown fence (```tool_code/call/edit/python/…) that the
        // parser ignores; `missing_tool_call_nudge` fires when the model
        // likely meant to call a bound tool without emitting any call.
        // `no_progress_streak_nudge` is the escalating fallback for pure-prose
        // churn turns that made no successful tool progress for >= 2 turns.
        // All three surface to operators on the FeedbackInjected stream.
        "fenced_call_attempt_nudge" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "fenced_call_attempt_nudge".to_string(),
            content: get_string("fence"),
        }),
        "missing_tool_call_nudge" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "missing_tool_call_nudge".to_string(),
            content: get_string("tool"),
        }),
        "no_progress_streak_nudge" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "no_progress_streak_nudge".to_string(),
            content: get_usize("turns_since_progress").to_string(),
        }),
        // `tool_parse_error_feedback` fires when a turn's tool calls were ALL
        // dropped by the parser (>= 1 parse error, 0 dispatched). The engine
        // injects the purpose-built parse-guidance prompt instead of the
        // generic no-progress nudge; this surfaces that correction to operators
        // on the FeedbackInjected stream (engine-injected, not user-injected),
        // exactly like the sibling text-mode nudges above. `content` carries the
        // first parser diagnostic that drove the correction.
        "tool_parse_error_feedback" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "tool_parse_error_feedback".to_string(),
            content: get_string("error_summary"),
        }),
        // `tool_call_blank_name_dropped` fires when a provider emits one or more
        // tool calls with an empty/whitespace name (JSON malformation). The
        // engine drops ONLY the nameless calls, keeps valid siblings, and
        // injects parse-guidance so the loop self-corrects instead of
        // terminating silently on the malformed call. Surfaced on the
        // FeedbackInjected stream like the sibling corrections above; `content`
        // carries the dropped-call count.
        "tool_call_blank_name_dropped" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "tool_call_blank_name_dropped".to_string(),
            content: get_usize("dropped_count").to_string(),
        }),
        // `llm_auto_continue` fires when a length-truncated turn with an
        // incomplete tool call is re-issued with a raised output cap instead of
        // burning the turn on parse-guidance. Engine-emitted (not user
        // feedback), surfaced on the FeedbackInjected stream like the sibling
        // corrections above so operators can see the recovery. `content`
        // summarizes the cap raise: "<previous>->-<raised> (attempt N/max)".
        "llm_auto_continue" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "llm_auto_continue".to_string(),
            content: format!(
                "{}->{} (attempt {}/{})",
                get_usize("previous_max_tokens"),
                get_usize("raised_max_tokens"),
                get_usize("attempt"),
                get_usize("max_continuations"),
            ),
        }),
        // `context_overflow_recovery` fires when a provider rejected the turn
        // with a context_overflow error and the loop emergency-compacted the
        // transcript (deterministic observation masking) before re-issuing the
        // turn — the recovery that keeps the agent working on a large repo
        // instead of dying. Engine-emitted; surfaced on the FeedbackInjected
        // stream like the sibling recoveries. `content` summarizes the attempt
        // and how many messages were archived to fit under the window.
        "context_overflow_recovery" => Ok(AgentEvent::FeedbackInjected {
            session_id: session_id.to_string(),
            kind: "context_overflow_recovery".to_string(),
            content: format!(
                "attempt {}/{} archived {} messages",
                get_usize("attempt"),
                get_usize("max_recoveries"),
                get_usize("archived_messages"),
            ),
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
            redacted_count: result.redaction_pointers.len(),
            reclaimed_tokens: result.reclaimed_tokens,
            roots_consulted: result.roots_consulted.clone(),
            redaction_pointers: result.redaction_pointers.clone(),
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
) -> Option<Arc<crate::bridge::HostBridge>> {
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
    Ok(VmValue::String(arcstr::ArcStr::from(reminder_id)))
}

/// Push a user-role message onto the session's host bridge queue. The
/// in-VM equivalent of the ACP `session/inject` JSON-RPC method (the
/// user-message sibling of `__host_agent_session_push_bridge_injection`,
/// which is the in-VM equivalent of `session/remind`). Exposed so a Harn
/// script driving the loop (custom CLI host, conformance test, etc.) can
/// enqueue a steer mid-turn without going through ACP.
///
/// Expects `options.content` (string, required, non-empty) and an
/// optional `options.mode` (string, default `"finish_step"`). The mode
/// follows `QueuedUserMessageMode::from_str`: `"finish_step"` /
/// `"after_current_operation"` / `"steer"` deliver at the next loop
/// checkpoint (tool boundary / iteration boundary); `"interrupt_immediate"`
/// / `"interrupt"` preempt the next tool batch; `"audit_only"` / `"queue"`
/// land in the transcript at `loop_exit` only. Returns the message id so
/// callers can correlate with later events / revoke the message.
#[harn_builtin(
    sig = "__host_agent_session_push_user_message(session_id: string, options: dict) -> string",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_push_user_message(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    if session_id.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: session_id must be a non-empty string"
        )));
    }
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let params = vm_to_json(&options);
    if !params.is_object() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: options must be a dict"
        )));
    }
    let content = params
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if content.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: options.content must be a non-empty string"
        )));
    }
    let mode = params
        .get("mode")
        .and_then(|value| value.as_str())
        .unwrap_or("finish_step")
        .to_string();
    let Some(bridge) = host_bridge_for_session(&session_id, HOST_SESSION_PUSH_USER_MESSAGE) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PUSH_USER_MESSAGE}: no host bridge attached to session `{session_id}`"
        )));
    };
    let message_id = bridge.push_queued_user_message(content, &mode).await;
    Ok(VmValue::String(arcstr::ArcStr::from(message_id)))
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
    let mut opts = crate::value::DictMap::new();
    if let Some(config) = args.get(1) {
        opts.insert(crate::value::intern_key("autonomy_budget"), config.clone());
    }
    match check_autonomy_budget(&opts, &session_id).await? {
        AutonomyCheck::Denied(result) => {
            let mut out = crate::value::DictMap::new();
            out.insert(crate::value::intern_key("approved"), VmValue::Bool(false));
            out.insert(crate::value::intern_key("denial_result"), result);
            Ok(VmValue::dict(out))
        }
        AutonomyCheck::Approved(_) | AutonomyCheck::NoBudget => {
            let mut out = crate::value::DictMap::new();
            out.insert(crate::value::intern_key("approved"), VmValue::Bool(true));
            Ok(VmValue::dict(out))
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
const SESSION_POLICY_OPTION_KEYS: [&str; 4] =
    ["policy", "approval_policy", "command_policy", "permissions"];

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
    &HOST_AGENT_SESSION_PAIR_ORPHANED_TOOL_USE_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_USAGE_BUILTIN_DEF,
    &HOST_AGENT_SESSION_DRAIN_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_TOTALS_BUILTIN_DEF,
    &HOST_AGENT_TRUNCATED_TOOL_CALL_BUILTIN_DEF,
    &HOST_AGENT_SESSION_INJECT_FEEDBACK_BUILTIN_DEF,
    &HOST_AGENT_SESSION_INJECT_REMINDER_BUILTIN_DEF,
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
    &HOST_AGENT_SESSION_PUSH_USER_MESSAGE_DEF,
    &HOST_AGENT_SESSION_PENDING_INJECTIONS_DEF,
    &HOST_AGENT_SESSION_REVOKE_REMINDER_DEF,
    &HOST_AGENT_DAEMON_WAIT_DEF,
    &HOST_AGENT_SESSION_PROJECT_TURN_DEF,
];

pub fn register_agent_session_host_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, HOST_SESSION_BUILTINS);
}

#[cfg(test)]
#[path = "agent_session_host_tests.rs"]
mod tests;
