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

use crate::orchestration::{
    pop_approval_policy, pop_execution_policy, push_approval_policy, push_command_policy,
    push_execution_policy, CapabilityPolicy, ToolApprovalPolicy,
};
use crate::stdlib::registration::{register_builtin_group, BuiltinGroup, SyncBuiltin};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

use super::cost::calculate_cost_for_provider;
use super::permissions;

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
const HOST_SESSION_COMPACT: &str = "__host_agent_session_compact_if_needed";
const HOST_SKILL_SCORE: &str = "__host_skill_score";
const HOST_BUDGET_PRE_CALL: &str = "__host_agent_budget_pre_call_blocked";
const HOST_BUILD_TURN_SYSTEM: &str = "__host_agent_build_turn_system";

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
    started_at: String,
    /// Iteration cap from `agent_loop(options.max_iterations)`. Captured
    /// here so finalize can disambiguate `final_status == "budget_exhausted"`
    /// caused by hitting the cap (→ ACP `max_turn_requests`) from other
    /// budget paths.
    max_iterations: i64,
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
    let session_id = opt_str(&opts_map, "session_id")
        .or_else(crate::agent_sessions::current_session_id)
        .unwrap_or_else(|| format!("agent_session_{}", now_id()));
    let resolved = crate::agent_sessions::open_or_create(Some(session_id));

    let user_msg = serde_json::json!({"role": "user", "content": message});
    let _ = crate::agent_sessions::inject_message(&resolved, json_to_vm(&user_msg));

    let max_iterations = opt_int(&opts_map, "max_iterations").unwrap_or(50).max(1);
    let max_verify_attempts = opt_int(&opts_map, "max_verify_attempts")
        .unwrap_or(20)
        .max(0);

    let installed_policies = install_session_policies(&opts_map)?;

    let session = AgentHostSession {
        session_id: resolved.clone(),
        task: message.clone(),
        tokens_used: 0,
        cost_used: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        active_skills: Vec::new(),
        tool_calls: Vec::new(),
        successful_tools: Vec::new(),
        rejected_tools: Vec::new(),
        tool_mode: String::new(),
        started_at: now_id(),
        max_iterations,
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
        "tokens_used": session.tokens_used,
        "cost_usd": session.cost_used,
        "session_id": session.session_id,
        "started_at": session.started_at,
        "task": session.task,
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
            return dict_get(msg, "content").map(|v| v.display());
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

fn host_agent_session_record_assistant_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let llm_result = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let text = dict_get(&llm_result, "text")
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
        Ok(())
    });
    let mut msg = BTreeMap::new();
    msg.insert("role".to_string(), VmValue::String(Rc::from("assistant")));
    msg.insert("content".to_string(), VmValue::String(Rc::from(text)));
    let _ = crate::agent_sessions::inject_message(&session_id, VmValue::Dict(Rc::new(msg)));
    Ok(VmValue::Nil)
}

fn host_agent_session_record_tool_results_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let dispatch = args.get(1).cloned().unwrap_or(VmValue::Nil);
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
        let ok = dict_get(result, "ok")
            .or_else(|| dict_get(result, "success"))
            .map(|v| matches!(v, VmValue::Bool(true)))
            .unwrap_or(true);
        if ok {
            successful.push(name.clone());
        } else {
            rejected.push(name.clone());
        }
        let mut msg = BTreeMap::new();
        msg.insert("role".to_string(), VmValue::String(Rc::from("tool")));
        msg.insert(
            "content".to_string(),
            VmValue::String(Rc::from(observation)),
        );
        msg.insert("name".to_string(), VmValue::String(Rc::from(name)));
        let _ = crate::agent_sessions::inject_message(&session_id, VmValue::Dict(Rc::new(msg)));
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
        .and_then(|v| match v {
            VmValue::Int(i) => Some(*i),
            VmValue::Float(f) => Some(*f as i64),
            _ => None,
        })
        .unwrap_or(0);
    let output_tokens = dict_get(&llm_block, "output_tokens")
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
        session.active_skills = ids;
        Ok(())
    })?;
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

fn host_agent_session_compact_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

async fn host_skill_score(_args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let mut out = BTreeMap::new();
    out.insert("scored".to_string(), VmValue::List(Rc::new(Vec::new())));
    out.insert("active".to_string(), VmValue::List(Rc::new(Vec::new())));
    Ok(VmValue::Dict(Rc::new(out)))
}

fn host_agent_budget_pre_call_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(false))
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
    SyncBuiltin::new(HOST_SESSION_COMPACT, host_agent_session_compact_builtin)
        .signature("__host_agent_session_compact_if_needed(session_id, options)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("No-op compaction hook; Harn implements compaction via llm_call."),
    SyncBuiltin::new(HOST_BUDGET_PRE_CALL, host_agent_budget_pre_call_builtin)
        .signature("__host_agent_budget_pre_call_blocked(session_id, envelope)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Pre-call budget projection hook (returns false for now)."),
    SyncBuiltin::new(HOST_BUILD_TURN_SYSTEM, host_agent_build_turn_system_builtin)
        .signature("__host_agent_build_turn_system(session_id, options, iteration)")
        .arity(VmBuiltinArity::Exact(3))
        .doc("Compose the per-turn system prompt from system + tool contract."),
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

    let skill_score = VmBuiltinMetadata::async_static(HOST_SKILL_SCORE)
        .signature_static("__host_skill_score(context, registry, options)")
        .arity(VmBuiltinArity::Exact(3))
        .category_static("agent.host")
        .doc_static("Score skills against the current task context (stub returning empty).");
    vm.register_async_builtin_with_metadata(skill_score, |args| {
        Box::pin(async move { host_skill_score(args).await })
    });
}

#[cfg(test)]
mod tests {
    use super::canonical_acp_stop_reason;

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
