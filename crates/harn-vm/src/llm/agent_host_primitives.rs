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
    envelope.insert("result".to_string(), result);
    envelope.insert(
        "events".to_string(),
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
) -> serde_json::Value {
    let reason = reason.into();
    // Split the inner result body by category. A RECOVERABLE rejection
    // (`SchemaValidation` — bad/missing arguments or an empty tool name) must
    // coach a retry *with the correction* via `recoverable_tool_result`
    // (`error: "invalid_arguments"`), never the permission-denial body. Reusing
    // `denied_tool_result` here was the live convergence bug: cheap models read
    // "permission_denied / Do not retry the same call" after one fixable arg
    // mistake and gave up (false FAIL across ~26 recent eval transcripts). True
    // policy/permission denials keep the don't-retry `denied_tool_result` body.
    let mut result = if category.is_recoverable() {
        agent_tools::recoverable_tool_result(tool_name, reason.clone())
    } else {
        agent_tools::denied_tool_result(tool_name, reason.clone())
    };
    // Mirror the structured denial onto the inner tool result so it rides
    // along in the transcript the model sees, and onto the envelope so a
    // host harness reading the dispatch outcome can fail or pivot early
    // without re-parsing the rendered reason (harn#2780).
    if let Some(denial) = denial {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("denial".to_string(), denial.to_json());
        }
    }
    serde_json::json!({
        "ok": false,
        "status": "error",
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": result,
        "rendered_result": agent_tools::render_tool_result(&result),
        "error": reason,
        "error_category": category.as_str(),
        "denial": denial.map(crate::agent_events::ToolDenial::to_json),
        "executor": null,
    })
}

/// Cause-named feedback for a tool call whose arguments arrived EMPTY
/// (`{}` or null) and failed required-parameter validation.
///
/// Observed live on the OpenAI-compatible native tool-call route
/// (burin-code#2121): 13/165 edit calls arrived with literally `{}` arguments
/// while the model generated 549–5,056 output tokens those turns — the model
/// authored content, but the provider boundary delivered an empty-args call.
/// The generic "missing required parameter(s): path" message misdiagnoses
/// that as a model slip and sends it into re-call loops. Name the actual
/// cause instead, keyed off the turn's provider stop reason:
///
/// - length truncation (`length` / `max_tokens` / `MAX_TOKENS`): the
///   arguments were cut off by the output-token limit — coach a smaller
///   re-issue.
/// - anything else: the provider/template dropped the arguments — coach an
///   identical re-issue with the full arguments.
///
/// Returns `None` when the call's arguments were not empty, so ordinary
/// validation failures keep the precise missing-parameter message. The
/// `&'static str` is a machine-readable cause (`empty_arguments_truncated` /
/// `empty_arguments_dropped`) exposed on the dispatch envelope so hosts can
/// distinguish the class without string-matching the reason.
fn empty_args_cause_named_feedback(
    tool_name: &str,
    raw_args: &serde_json::Value,
    stop_reason: Option<&str>,
) -> Option<(String, &'static str)> {
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

/// Emit the `PermissionDeny` event and build the denied `tool_result` for a
/// refused agent tool call. Centralizes the structured-denial plumbing so
/// every gate (capability ceiling, argument allow-list, dynamic permission,
/// approval policy, host rejection, pre-tool hook) produces an identical
/// shape. Fills `denied_paths` from the tool's annotated path arguments when
/// the caller left them empty. Returns the raw `serde_json::Value` so callers
/// that need to enrich it further (e.g. attach hook-reminder audit) can do so
/// before converting to a `VmValue`.
fn deny_tool_call(
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    mut denial: crate::agent_events::ToolDenial,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) -> serde_json::Value {
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
    )
}

/// `deny_tool_call` for the common case where no further enrichment is
/// needed — emits the event, builds the denied result, and converts to a
/// `VmValue` ready to return from the dispatch primitive.
fn deny_tool_call_value(
    session_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    denial: crate::agent_events::ToolDenial,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) -> VmValue {
    json_to_vm_value(&deny_tool_call(
        session_id,
        tool_name,
        tool_call_id,
        tool_args,
        denial,
        escalated,
        policy_decision,
    ))
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
    serde_json::json!({
        "ok": false,
        "status": "cancelled",
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": serde_json::Value::Null,
        "rendered_result": rendered,
        "observation": observation,
        "error": error_message,
        "error_category": crate::agent_events::ToolCallErrorCategory::Cancelled.as_str(),
        "executor": executor,
        "approval": approval_status,
        "execution_duration_ms": execution_duration_ms,
        "cancelled": true,
        "cancellation_reason": reason,
    })
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
    let parsed = tools::parse_text_tool_calls_in_format(&text, tools.as_ref(), format);
    Ok(json_to_vm_value(&serde_json::json!({
        "calls": parsed.calls,
        "tool_calls": parsed.calls,
        "tool_parse_errors": parsed.errors,
        "protocol_violations": parsed.violations,
        "prose": parsed.prose,
        "user_response": parsed.user_response,
        "done_marker": parsed.done_marker,
        "canonical_text": parsed.canonical,
    })))
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
        return Some(GateOutcome {
            reason: format!(
                "Untrusted content from {origins} is in context and `{tool_name}` can send data to an external destination.{injection_note} Confirm this is intended (lethal-trifecta guard)."
            ),
            injection_flagged,
        });
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

async fn host_agent_dispatch_tool_call(
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
    let raw_args = call
        .get("arguments")
        .map(helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let mut tool_name = match call.get("name") {
        Some(VmValue::String(name)) if !name.trim().is_empty() => name.to_string(),
        // An empty/missing/non-string tool name is a recoverable parse slip,
        // not a terminal harness error: the model emitted a malformed call and
        // can fix it on the next turn. Surface it through the same
        // schema-validation denied-tool envelope the loop already re-injects
        // for bad arguments (a hard `Err` here aborts the whole agent_loop,
        // wrongly tanking a run over one fixable slip). The unknown-but-named
        // path is already recoverable this way; align the unnamed path with it.
        _ => {
            let denied = agent_primitive_denied_tool(
                "<unnamed>",
                &tool_id,
                &raw_args,
                "Tool call is missing a name. Emit one tool call per turn as \
                 `name({ ... })` using a non-empty tool name from the allowed \
                 list, then retry.",
                crate::agent_events::ToolCallErrorCategory::SchemaValidation,
                None,
            );
            return Ok(json_to_vm_value(&denied));
        }
    };
    let mut tool_args = tools::normalize_tool_args(&tool_name, &raw_args);
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
    let _policy_guard = agent_session_host::install_session_policy_guard(options)?;

    if let Err(policy_denial) = crate::orchestration::enforce_current_policy_for_tool(&tool_name)
        .and_then(|_| {
            crate::orchestration::enforce_tool_arg_constraints(
                &crate::orchestration::current_execution_policy().unwrap_or_default(),
                &tool_name,
                &tool_args,
            )
        })
    {
        let denial = crate::agent_events::ToolDenial::terminal(
            policy_denial.gate,
            policy_denial.capability,
            policy_denial.reason,
        );
        return Ok(deny_tool_call_value(
            &session_id,
            &tool_name,
            &tool_id,
            &tool_args,
            denial,
            false,
            None,
        ));
    }

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
            permissions::PermissionCheck::Denied { reason, escalated } => {
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
                let denial = crate::agent_events::ToolDenial::terminal(
                    crate::agent_events::DenialGate::DynamicPermission,
                    None,
                    reason,
                );
                return Ok(deny_tool_call_value(
                    &session_id,
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    denial,
                    escalated,
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
                    "session/request_permission",
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
        );
        let denied = attach_hook_reminder_audit(denied, hook_reminder_reports);
        return Ok(json_to_vm_value(&denied));
    }

    // Burin compass tool-rewrite router (B.9, #2612). Observe freeform /
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
        // Empty-args calls (`{}` / null arguments) are a provider-boundary
        // fault class, not a model slip — replace the misdiagnosing
        // missing-parameter message with cause-named feedback keyed off the
        // turn's provider stop reason (threaded in by the agent loop as
        // `_stop_reason`). See `empty_args_cause_named_feedback`.
        let turn_stop_reason = agent_primitive_option_str(options, "_stop_reason");
        let cause_named = empty_args_cause_named_feedback(
            &tool_name,
            &raw_args,
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
            let rendered_before_hooks = agent_tools::render_tool_result(&raw_result);
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
                        for tool in raw_tools {
                            tools_added.push(tag_mcp_tool(tool, &server_name, eager_schemas));
                        }
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

    fn vm_str(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    #[test]
    fn tool_descriptor_extracts_description_and_schema_changed() {
        let mut tool = crate::value::DictMap::new();
        tool.insert("name".to_string(), vm_str("linear__create"));
        tool.insert("description".to_string(), vm_str("Create an issue"));
        tool.insert("_mcp_server".to_string(), vm_str("linear"));
        tool.insert("_schema_changed".to_string(), VmValue::Bool(true));
        let catalog = {
            let mut dict = crate::value::DictMap::new();
            dict.insert(
                "tools".to_string(),
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
    use super::agent_primitive_denied_tool;
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
        );
        // Envelope-level category is still schema_validation for the wire...
        assert_eq!(envelope["error_category"], "schema_validation");
        // ...but the inner model-facing result is retry-positive, NOT a denial.
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        assert_ne!(result["error"], serde_json::json!("permission_denied"));
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
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            !next.contains("Do not retry"),
            "empty-name slip must be retry-positive: {next}"
        );
    }

    use super::empty_args_cause_named_feedback;

    #[test]
    fn empty_args_with_length_truncation_names_the_truncation_cause() {
        let (reason, cause) =
            empty_args_cause_named_feedback("edit", &serde_json::json!({}), Some("length"))
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
            empty_args_cause_named_feedback("edit", &serde_json::Value::Null, Some("MAX_TOKENS"))
                .expect("null args must be cause-named");
        assert_eq!(cause, "empty_arguments_truncated");
    }

    #[test]
    fn empty_args_with_clean_stop_names_the_provider_fault_cause() {
        for stop_reason in [Some("stop"), Some("tool_calls"), None] {
            let (reason, cause) =
                empty_args_cause_named_feedback("edit", &serde_json::json!({}), stop_reason)
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

    #[test]
    fn non_empty_args_keep_the_precise_validator_message() {
        assert!(
            empty_args_cause_named_feedback(
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
