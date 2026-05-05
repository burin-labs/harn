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
    pop_approval_policy, pop_execution_policy, push_approval_policy, push_command_policy,
    push_execution_policy, CapabilityPolicy, ToolApprovalPolicy,
};
use crate::stdlib::registration::{register_builtin_group, BuiltinGroup, SyncBuiltin};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

use super::cost::calculate_cost_for_provider;
use super::helpers::ResolvedProvider;
use super::permissions;
use super::tools::build_assistant_response_message;

const HOST_SESSION_INIT: &str = "__host_agent_session_init";
const HOST_SESSION_FINALIZE: &str = "__host_agent_session_finalize";
const HOST_SESSION_MESSAGES: &str = "__host_agent_session_messages";
const HOST_SESSION_RECORD_ASSISTANT: &str = "__host_agent_session_record_assistant";
const HOST_SESSION_RECORD_TOOL_RESULTS: &str = "__host_agent_session_record_tool_results";
const HOST_SESSION_RECORD_USAGE: &str = "__host_agent_session_record_usage";
const HOST_SESSION_DRAIN_FEEDBACK: &str = "__host_agent_session_drain_feedback";
const HOST_SESSION_TOTALS: &str = "__host_agent_session_totals";
const HOST_SESSION_INJECT_FEEDBACK: &str = "__host_agent_session_inject_feedback";
const HOST_SESSION_SET_ACTIVE_SKILLS: &str = "__host_agent_session_set_active_skills";
const HOST_SESSION_ACTIVE_SKILLS: &str = "__host_agent_session_active_skills";
const HOST_SESSION_RECORD_SKILL_EVENT: &str = "__host_agent_session_record_skill_event";
const HOST_SESSION_COMPACT: &str = "__host_agent_session_compact_if_needed";
const HOST_SESSION_REPLACE_MESSAGES: &str = "__host_agent_session_replace_messages";
const HOST_SESSION_CLAIM_TOOL_FORMAT: &str = "__host_agent_session_claim_tool_format";
const HOST_SKILL_SCORE: &str = "__host_skill_score";
const HOST_BUDGET_PRE_CALL: &str = "__host_agent_budget_pre_call_blocked";
const HOST_BUILD_TURN_SYSTEM: &str = "__host_agent_build_turn_system";
const HOST_AUTONOMY_BUDGET_CHECK: &str = "__host_autonomy_budget_check";
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
    installed_policies: InstalledPolicies,
}

/// Tracks which scoped policy stacks were pushed during session init so
/// finalize can pop them in reverse order. The agent loop honours
/// per-agent ceilings by intersecting outer policies with the requested
/// per-agent ones before pushing — so child sub-agents never widen
/// permissions beyond their parents.
#[derive(Default)]
struct InstalledPolicies {
    pushed_execution: bool,
    pushed_approval: bool,
    pushed_command: bool,
    pushed_permissions: bool,
}

thread_local! {
    static AGENT_HOST_SESSIONS: RefCell<BTreeMap<String, AgentHostSession>> =
        const { RefCell::new(BTreeMap::new()) };
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

fn now_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

async fn host_agent_session_init(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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

    let resolved = crate::agent_sessions::open_or_create(Some(session_id));

    let user_msg = serde_json::json!({"role": "user", "content": message});
    let _ = crate::agent_sessions::inject_message(&resolved, json_to_vm(&user_msg));

    let max_iterations = opt_int(&opts_map, "max_iterations").unwrap_or(50).max(1);
    let max_verify_attempts = opt_int(&opts_map, "max_verify_attempts")
        .unwrap_or(20)
        .max(0);
    let daemon_config = super::daemon::parse_daemon_loop_config(Some(&opts_map));
    let resumed_iterations = match daemon_config.resume_path.as_deref() {
        Some(path) => super::daemon::load_snapshot(path)?.total_iterations,
        None => 0,
    };

    let installed_policies = install_session_policies(&opts_map)?;
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
        installed_policies,
    };

    AGENT_HOST_SESSIONS.with(|sessions| {
        sessions.borrow_mut().insert(resolved.clone(), session);
    });
    // Push the session id onto the thread-local current-session stack so
    // tool handlers + nested calls inside the loop see it via
    // `agent_session_current_id()`. Paired with the pop in finalize.
    crate::agent_sessions::push_current_session(resolved.clone());

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

async fn host_agent_session_finalize(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = args
        .first()
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{HOST_SESSION_FINALIZE}: missing session_id")))?;
    let status_dict = opts_dict(args.get(1));
    let final_status = opt_str(&status_dict, "final_status").unwrap_or_default();
    let stop_reason = opt_str(&status_dict, "stop_reason").unwrap_or_default();

    let session = AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow_mut().remove(&session_id))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{HOST_SESSION_FINALIZE}: unknown session `{session_id}`"
            ))
        })?;
    // Unwind the per-agent policy stacks pushed in init, in reverse
    // order — outer scopes (workflow stage, parent agent) survive intact.
    release_session_policies(&session.installed_policies);
    permissions::clear_session_grants(&session_id);
    if session.pushed_transcript_dir {
        super::agent_observe::pop_llm_transcript_dir();
    }
    // Pair with the push in init so subsequent loops see the right stack.
    crate::agent_sessions::pop_current_session();
    // Fire registered session-end hooks (e.g. cancelling orphaned
    // long-running handles) after the session has been removed from
    // the active map so hooks observe a fully-quiesced session.
    super::agent_runtime::fire_session_end_hooks(&session_id);

    let snapshot = crate::agent_sessions::snapshot(&session_id);
    let transcript_json = snapshot
        .as_ref()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let visible_text = snapshot
        .as_ref()
        .and_then(last_assistant_text)
        .unwrap_or_default();

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
    let trace_summary = super::trace::agent_trace_summary();
    let result = serde_json::json!({
        "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
        "final_status": final_status,
        "stop_reason": stop_reason,
        "acp_stop_reason": acp_stop_reason,
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

fn host_agent_session_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let snapshot = crate::agent_sessions::snapshot(&session_id);
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
    );
    json_to_vm(&msg)
}

fn host_agent_session_record_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let provider = dict_get(&llm_result, "provider")
        .map(|v| v.display())
        .unwrap_or_default();
    let raw_tool_calls = dict_get(&llm_result, "tool_calls")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let calls_json = list_items(&raw_tool_calls)
        .iter()
        .map(vm_to_json)
        .collect::<Vec<_>>();
    let _ = with_session(&session_id, HOST_SESSION_RECORD_ASSISTANT, |session| {
        session.tool_calls.extend(calls_json);
        if !provider.is_empty() {
            session.last_provider = Some(provider);
        }
        Ok(())
    });
    let _ = crate::agent_sessions::inject_message(
        &session_id,
        assistant_message_from_llm_result(&llm_result),
    );
    Ok(VmValue::Nil)
}

fn tool_result_message_for_provider(
    provider: &str,
    name: &str,
    tool_call_id: &str,
    observation: &str,
) -> VmValue {
    let mut msg = BTreeMap::new();
    if ResolvedProvider::resolve(provider).is_anthropic_style {
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

fn host_agent_session_record_tool_results_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let dispatch = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let provider = with_session(&session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
        Ok(session.last_provider.clone().unwrap_or_default())
    })
    .unwrap_or_default();
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
                let _ = crate::agent_sessions::append_event(&session_id, event);
                super::agent_runtime::emit_agent_event_sync(&AgentEvent::Plan {
                    session_id: session_id.clone(),
                    plan: plan_value,
                });
            }
        }
        let _ = crate::agent_sessions::inject_message(
            &session_id,
            tool_result_message_for_provider(&provider, &name, &tool_call_id, &observation),
        );
    }
    let _ = with_session(&session_id, HOST_SESSION_RECORD_TOOL_RESULTS, |session| {
        session.successful_tools.extend(successful);
        session.rejected_tools.extend(rejected);
        Ok(())
    });
    Ok(VmValue::Nil)
}

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
    let _ = crate::agent_sessions::append_event(
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
            })),
        ),
    );
    let mut out = BTreeMap::new();
    out.insert("tokens_used".to_string(), VmValue::Int(totals.0));
    out.insert("cost_usd".to_string(), VmValue::Float(totals.1));
    Ok(VmValue::Dict(Rc::new(out)))
}

fn host_agent_session_drain_feedback_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    // Drained per-session feedback is no longer plumbed through Rust;
    // the new Harn loop drives `agent_inject_feedback` in-process. Keep
    // the primitive registered as a no-op for source compatibility.
    Ok(VmValue::List(Rc::new(Vec::new())))
}

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

fn host_agent_session_inject_feedback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
    let content = args.get(2).map(|v| v.display()).unwrap_or_default();
    let body = format!("<runtime_feedback kind=\"{kind}\">\n{content}\n</runtime_feedback>");
    let mut msg = BTreeMap::new();
    msg.insert("role".to_string(), VmValue::String(Rc::from("user")));
    msg.insert("content".to_string(), VmValue::String(Rc::from(body)));
    let _ = crate::agent_sessions::inject_message(&session_id, VmValue::Dict(Rc::new(msg)));
    Ok(VmValue::Nil)
}

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
    let _ = crate::agent_sessions::append_event(&session_id, event);

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
        _ => {}
    }
    Ok(VmValue::Nil)
}

fn host_agent_session_compact_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

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
    );
    Ok(VmValue::Nil)
}

async fn host_skill_score(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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

fn host_agent_budget_pre_call_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(false))
}

async fn host_agent_emit_event(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
        "tool_search_query" | "tool_search_result" | "typed_checkpoint"
    ) {
        let role = if event_type == "tool_search_result" {
            "tool"
        } else {
            "assistant"
        };
        let transcript_event =
            super::helpers::transcript_event(&event_type, role, "internal", "", Some(payload));
        let _ = crate::agent_sessions::append_event(&session_id, transcript_event);
    }
    crate::llm::agent_runtime::emit_agent_event(&event).await;
    Ok(VmValue::Nil)
}

fn build_agent_event(
    session_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<crate::agent_events::AgentEvent, VmError> {
    use crate::agent_events::AgentEvent;
    let payload_obj = payload.as_object();
    let get_usize = |key: &str| -> usize {
        payload_obj
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
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
        "turn_start" => Ok(AgentEvent::TurnStart {
            session_id: session_id.to_string(),
            iteration: get_usize("iteration"),
        }),
        "turn_end" => {
            let turn_info = payload_obj
                .and_then(|m| m.get("turn_info"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(AgentEvent::TurnEnd {
                session_id: session_id.to_string(),
                iteration: get_usize("iteration"),
                turn_info,
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
        }),
        "typed_checkpoint" => Ok(AgentEvent::TypedCheckpoint {
            session_id: session_id.to_string(),
            checkpoint: payload.clone(),
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
        other => Err(VmError::Runtime(format!(
            "{HOST_AGENT_EMIT_EVENT}: unsupported event type `{other}`"
        ))),
    }
}

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
        policy: policy.clone(),
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
    let _ = crate::agent_sessions::append_event(&session_id, event);
    Ok(VmValue::Nil)
}

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
    let _ = crate::agent_sessions::append_event(&session_id, event);
    Ok(VmValue::Nil)
}

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

fn host_agent_build_turn_system_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let _session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let opts_map = opts_dict(args.get(1));
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = opt_str(&opts_map, "system") {
        if !system.is_empty() {
            parts.push(system);
        }
    }
    if let Some(skill_prompt) = render_active_skill_prompt(opts_map.get("active_skills")) {
        parts.push(skill_prompt);
    }
    if opts_map.contains_key("tools") {
        let tool_format = opt_str(&opts_map, "tool_format").unwrap_or_else(|| "text".to_string());
        if tool_format != "native" {
            parts.push(
                include_str!(
                    "../../../harn-stdlib/src/stdlib/agent/prompts/tool_contract_text.harn.prompt"
                )
                .to_string(),
            );
        }
    }
    Ok(VmValue::String(Rc::from(parts.join("\n\n"))))
}

fn render_active_skill_prompt(value: Option<&VmValue>) -> Option<String> {
    let active = match value {
        Some(VmValue::List(list)) => list,
        _ => return None,
    };
    if active.is_empty() {
        return None;
    }
    let mut out = String::from("## Active skills\n");
    for skill in active.iter() {
        let Some(dict) = skill.as_dict() else {
            continue;
        };
        let name = dict
            .get("id")
            .or_else(|| dict.get("name"))
            .map(VmValue::display);
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            continue;
        };
        out.push_str(&format!("\n### {name}\n"));
        if let Some(description) = dict
            .get("description")
            .map(VmValue::display)
            .filter(|text| !text.trim().is_empty())
        {
            out.push_str(description.trim());
            out.push('\n');
        }
        if let Some(when_to_use) = dict
            .get("when_to_use")
            .map(VmValue::display)
            .filter(|text| !text.trim().is_empty())
        {
            out.push_str("When to use: ");
            out.push_str(when_to_use.trim());
            out.push('\n');
        }
        let allowed_tools = match dict.get("allowed_tools") {
            Some(VmValue::List(list)) => list
                .iter()
                .map(VmValue::display)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        if !allowed_tools.is_empty() {
            out.push_str("Scoped tools: ");
            out.push_str(&allowed_tools.join(", "));
            out.push('\n');
        }
        if let Some(prompt) = dict
            .get("prompt")
            .map(VmValue::display)
            .filter(|text| !text.trim().is_empty())
        {
            out.push('\n');
            out.push_str(prompt.trim());
            out.push('\n');
        }
    }
    Some(out)
}

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
    let transcript = crate::agent_sessions::snapshot(&session_id).unwrap_or(VmValue::Nil);
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

async fn host_agent_daemon_wait(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let timeout_ms = args
        .get(1)
        .and_then(VmValue::as_int)
        .map(|value| value.max(0) as u64)
        .unwrap_or(0);
    let bridge = with_session(&session_id, HOST_DAEMON_WAIT, |session| {
        Ok(session.host_bridge.clone())
    })
    .ok()
    .flatten()
    .or_else(super::agent_runtime::current_host_bridge);
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
        let queued = bridge
            .take_queued_user_messages_for(crate::bridge::DeliveryCheckpoint::InterruptImmediate)
            .await;
        if !queued.is_empty() {
            for message in queued {
                let _ = crate::agent_sessions::inject_message(
                    &session_id,
                    json_to_vm(&serde_json::json!({
                        "role": "user",
                        "content": message.content,
                    })),
                );
            }
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": "message"})));
        }
    }

    if timeout_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
    }

    if let Some(bridge) = bridge.as_ref() {
        let queued = bridge
            .take_queued_user_messages_for(crate::bridge::DeliveryCheckpoint::InterruptImmediate)
            .await;
        if !queued.is_empty() {
            for message in queued {
                let _ = crate::agent_sessions::inject_message(
                    &session_id,
                    json_to_vm(&serde_json::json!({
                        "role": "user",
                        "content": message.content,
                    })),
                );
            }
            bridge.set_daemon_idle(false);
            return Ok(json_to_vm(&serde_json::json!({"reason": "message"})));
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

async fn host_autonomy_budget_check(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
/// policies onto the thread-local stacks for the lifetime of this agent
/// session. Each scope intersects with the currently-active outer policy
/// (when any) so a sub-agent cannot widen its parent's ceiling — only
/// narrow it. Dynamic permissions are stack-checked, so push as-is and
/// rely on the dispatch path to honour every active scope.
///
/// On any failure the partially-pushed stacks are unwound before
/// returning, so the caller never has to worry about leaked policy
/// state.
fn install_session_policies(
    opts_map: &BTreeMap<String, VmValue>,
) -> Result<InstalledPolicies, VmError> {
    let mut installed = InstalledPolicies::default();
    match install_session_policies_inner(opts_map, &mut installed) {
        Ok(()) => Ok(installed),
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

const HOST_SESSION_PRIMITIVES_SYNC: &[SyncBuiltin] = &[
    SyncBuiltin::new(HOST_SESSION_MESSAGES, host_agent_session_messages_builtin)
        .signature("__host_agent_session_messages(session_id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return the visible message list for an agent session."),
    SyncBuiltin::new(
        HOST_SESSION_RECORD_ASSISTANT,
        host_agent_session_record_assistant_builtin,
    )
    .signature("__host_agent_session_record_assistant(session_id, llm_result)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Append the assistant turn from an llm_call result to the session log."),
    SyncBuiltin::new(
        HOST_SESSION_RECORD_TOOL_RESULTS,
        host_agent_session_record_tool_results_builtin,
    )
    .signature("__host_agent_session_record_tool_results(session_id, dispatch)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Append per-tool observation messages from a dispatch result."),
    SyncBuiltin::new(
        HOST_SESSION_RECORD_USAGE,
        host_agent_session_record_usage_builtin,
    )
    .signature("__host_agent_session_record_usage(session_id, llm_result)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Accumulate token + cost usage from an llm_call result, return totals."),
    SyncBuiltin::new(
        HOST_SESSION_DRAIN_FEEDBACK,
        host_agent_session_drain_feedback_builtin,
    )
    .signature("__host_agent_session_drain_feedback(session_id)")
    .arity(VmBuiltinArity::Exact(1))
    .doc("Drain pending runtime-feedback notes for a session (no-op shim)."),
    SyncBuiltin::new(HOST_SESSION_TOTALS, host_agent_session_totals_builtin)
        .signature("__host_agent_session_totals(session_id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Read accumulated token + cost totals for a session."),
    SyncBuiltin::new(
        HOST_SESSION_INJECT_FEEDBACK,
        host_agent_session_inject_feedback_builtin,
    )
    .signature("__host_agent_session_inject_feedback(session_id, kind, content)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Append a runtime-feedback note to the session as a synthetic user turn."),
    SyncBuiltin::new(
        HOST_SESSION_SET_ACTIVE_SKILLS,
        host_agent_session_set_active_skills_builtin,
    )
    .signature("__host_agent_session_set_active_skills(session_id, skills)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Replace the session's active skill list."),
    SyncBuiltin::new(
        HOST_SESSION_ACTIVE_SKILLS,
        host_agent_session_active_skills_builtin,
    )
    .signature("__host_agent_session_active_skills(session_id)")
    .arity(VmBuiltinArity::Exact(1))
    .doc("Return the session's active skill list."),
    SyncBuiltin::new(
        HOST_SESSION_RECORD_SKILL_EVENT,
        host_agent_session_record_skill_event_builtin,
    )
    .signature("__host_agent_session_record_skill_event(session_id, kind, metadata)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Append a skill lifecycle event and notify live agent-event sinks."),
    SyncBuiltin::new(HOST_SESSION_COMPACT, host_agent_session_compact_builtin)
        .signature("__host_agent_session_compact_if_needed(session_id, options)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("No-op compaction hook; Harn implements compaction via llm_call."),
    SyncBuiltin::new(
        HOST_SESSION_REPLACE_MESSAGES,
        host_agent_session_replace_messages_builtin,
    )
    .signature("__host_agent_session_replace_messages(session_id, messages, summary?)")
    .arity(VmBuiltinArity::Range { min: 2, max: 3 })
    .doc("Replace the session's transcript message list (used by Harn-driven auto-compact)."),
    SyncBuiltin::new(HOST_BUDGET_PRE_CALL, host_agent_budget_pre_call_builtin)
        .signature("__host_agent_budget_pre_call_blocked(session_id, envelope)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Pre-call budget projection hook (returns false for now)."),
    SyncBuiltin::new(HOST_BUILD_TURN_SYSTEM, host_agent_build_turn_system_builtin)
        .signature("__host_agent_build_turn_system(session_id, options, iteration)")
        .arity(VmBuiltinArity::Exact(3))
        .doc("Compose the per-turn system prompt from system + tool contract."),
    SyncBuiltin::new(HOST_DAEMON_SNAPSHOT, host_agent_daemon_snapshot_builtin)
        .signature("__host_agent_daemon_snapshot(session_id, options)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Persist a daemon snapshot for a Harn-driven agent session."),
    SyncBuiltin::new(
        HOST_SESSION_CLAIM_TOOL_FORMAT,
        host_agent_session_claim_tool_format_builtin,
    )
    .signature("__host_agent_session_claim_tool_format(session_id, tool_format)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Claim the session's tool_format contract; rejects mid-session changes."),
    SyncBuiltin::new(
        HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK,
        host_agent_record_native_tool_fallback_builtin,
    )
    .signature("__host_agent_record_native_tool_fallback(session_id, payload)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Record a native→text tool-call fallback as a transcript event and trace counter."),
    SyncBuiltin::new(
        HOST_AGENT_RECORD_COMPACTION,
        host_agent_record_compaction_builtin,
    )
    .signature("__host_agent_record_compaction(session_id, payload)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Record a transcript compaction as a transcript event and trace counter."),
];

const HOST_SESSION_PRIMITIVES_GROUP: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("agent.host")
    .sync(HOST_SESSION_PRIMITIVES_SYNC);

pub fn register_agent_session_host_primitives(vm: &mut Vm) {
    register_builtin_group(vm, HOST_SESSION_PRIMITIVES_GROUP);

    let init = VmBuiltinMetadata::async_static(HOST_SESSION_INIT)
        .signature_static("__host_agent_session_init(message, system?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .category_static("agent.host")
        .doc_static("Initialize a Harn-driven agent session: open transcript, seed user message.");
    vm.register_async_builtin_with_metadata(init, |args| {
        Box::pin(async move { host_agent_session_init(args).await })
    });

    let finalize = VmBuiltinMetadata::async_static(HOST_SESSION_FINALIZE)
        .signature_static("__host_agent_session_finalize(session_id, status)")
        .arity(VmBuiltinArity::Exact(2))
        .category_static("agent.host")
        .doc_static("Tear down a Harn-driven agent session and emit the final result dict.");
    vm.register_async_builtin_with_metadata(finalize, |args| {
        Box::pin(async move { host_agent_session_finalize(args).await })
    });

    let emit_event = VmBuiltinMetadata::async_static(HOST_AGENT_EMIT_EVENT)
        .signature_static("__host_agent_emit_event(session_id, event_type, payload)")
        .arity(VmBuiltinArity::Exact(3))
        .category_static("agent.host")
        .doc_static("Emit an agent event and record transcript-backed event types.");
    vm.register_async_builtin_with_metadata(emit_event, |args| {
        Box::pin(async move { host_agent_emit_event(args).await })
    });

    let skill_score = VmBuiltinMetadata::async_static(HOST_SKILL_SCORE)
        .signature_static("__host_skill_score(context, registry, options)")
        .arity(VmBuiltinArity::Exact(3))
        .category_static("agent.host")
        .doc_static("Score skills against the current task context.");
    vm.register_async_builtin_with_metadata(skill_score, |args| {
        Box::pin(async move { host_skill_score(args).await })
    });

    let budget_check = VmBuiltinMetadata::async_static(HOST_AUTONOMY_BUDGET_CHECK)
        .signature_static("__host_autonomy_budget_check(session_id, budget_config)")
        .arity(VmBuiltinArity::Exact(2))
        .category_static("agent.host")
        .doc_static("Check per-agent autonomy budget and return an approval-shaped denial.");
    vm.register_async_builtin_with_metadata(budget_check, |args| {
        Box::pin(async move { host_autonomy_budget_check(args).await })
    });

    let daemon_wait = VmBuiltinMetadata::async_static(HOST_DAEMON_WAIT)
        .signature_static("__host_agent_daemon_wait(session_id, timeout_ms)")
        .arity(VmBuiltinArity::Exact(2))
        .category_static("agent.host")
        .doc_static("Wait for daemon wake input or a timeout.");
    vm.register_async_builtin_with_metadata(daemon_wait, |args| {
        Box::pin(async move { host_agent_daemon_wait(args).await })
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        assistant_message_from_llm_result, canonical_acp_stop_reason, last_assistant_text,
        tool_result_message_for_provider, vm_to_json,
    };

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
    fn tool_results_replay_with_provider_appropriate_ids() {
        let local = vm_to_json(&tool_result_message_for_provider(
            "local",
            "release_run",
            "call_001",
            "ok",
        ));
        assert_eq!(local["role"], "tool");
        assert_eq!(local["name"], "release_run");
        assert_eq!(local["tool_call_id"], "call_001");

        let anthropic = vm_to_json(&tool_result_message_for_provider(
            "anthropic",
            "release_run",
            "call_002",
            "ok",
        ));
        assert_eq!(anthropic["role"], "tool_result");
        assert_eq!(anthropic["tool_use_id"], "call_002");
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
