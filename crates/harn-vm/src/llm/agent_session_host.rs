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

use crate::stdlib::registration::{register_builtin_group, BuiltinGroup, SyncBuiltin};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

use super::cost::calculate_cost_for_provider;

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
    successful_tools: Vec<serde_json::Value>,
    rejected_tools: Vec<serde_json::Value>,
    tool_mode: String,
    started_at: String,
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
    };

    AGENT_HOST_SESSIONS.with(|sessions| {
        sessions.borrow_mut().insert(resolved.clone(), session);
    });

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
    let result = serde_json::json!({
        "status": if final_status.is_empty() { "done" } else { final_status.as_str() },
        "final_status": final_status,
        "stop_reason": stop_reason,
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
    let results_value = dict_get(&dispatch, "results")
        .cloned()
        .unwrap_or(VmValue::Nil);
    let mut successful = Vec::new();
    let mut rejected = Vec::new();
    for result in list_items(&results_value).iter() {
        let name = dict_get(result, "name")
            .map(|v| v.display())
            .unwrap_or_default();
        let observation = dict_get(result, "observation")
            .or_else(|| dict_get(result, "output"))
            .or_else(|| dict_get(result, "content"))
            .map(|v| v.display())
            .unwrap_or_default();
        let success = dict_get(result, "success")
            .map(|v| matches!(v, VmValue::Bool(true)))
            .unwrap_or(true);
        let summary = serde_json::json!({"name": name, "observation": observation});
        if success {
            successful.push(summary);
        } else {
            rejected.push(summary);
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

    let totals = with_session(&session_id, HOST_SESSION_RECORD_USAGE, |session| {
        session.tokens_used = session
            .tokens_used
            .saturating_add(input_tokens)
            .saturating_add(output_tokens);
        session.input_tokens = session.input_tokens.saturating_add(input_tokens);
        session.output_tokens = session.output_tokens.saturating_add(output_tokens);
        session.cost_used += cost;
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
