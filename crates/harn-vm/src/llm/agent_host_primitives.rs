use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};

use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::agent_runtime::{current_agent_session_id, current_host_bridge};
use super::{
    agent_runtime, agent_session_host, agent_tools, compass_router, helpers, permissions, tools,
};

mod denial_results;
mod dispatch_policy;
pub(super) mod event_capture;
mod host_permission;
mod primitive_args;
mod side_effect_ceiling;
mod structured_tool_result;
mod tool_catalog;
mod tool_parse_diagnostics;
use denial_results::{
    agent_primitive_denied_tool, deny_tool_call, deny_tool_call_value, DenialEvidence,
};
use dispatch_policy::{tool_denial_from_policy, DispatchPolicy};
use host_permission::{
    emit_permission_event, emit_permission_event_with_policy, emit_runtime_denied_activity,
    emit_runtime_resolved_activity, emit_runtime_unavailable_activity, record_allowed_dispatch,
    request_host_permission, HostPermissionOutcome, HostPermissionRequest,
};
use primitive_args::{
    option_int as agent_primitive_option_int, option_str as agent_primitive_option_str,
    options_value as agent_primitive_options_value_arg, tools as agent_primitive_tools_arg,
    tools_value as agent_primitive_tools_value_arg,
};
use side_effect_ceiling::{request_side_effect_permission, SideEffectPermissionOutcome};
use tool_catalog::{
    annotations_for as tool_annotations_for, descriptor_for as tool_descriptor_for,
    permission_context_for,
};

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

/// Synthesize placeholder tool_results for calls that were persisted as an
/// assistant tool_use turn but will never be dispatched (pre-dispatch
/// interrupt, `agent_await_resumption` suspension). Recording these keeps
/// the transcript well-formed: Anthropic rejects any assistant `tool_use`
/// block without an adjacent `tool_result` on the next call (HTTP 400),
/// which otherwise breaks interrupted or resumed sessions.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
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
/// fenced-JSON parser; everything else — `"text"`, `"auto"`, nil, and the
/// never-text `"native"` (defensively) — uses the canonical tagged grammar.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_parse_tool_calls(text: string, tools?: {_type: \"tool_registry\", tools: list}?, tool_format?: string?) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_parse_tool_calls_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
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
    let mut parsed = crate::stdlib::harn_entry::call_harn_export_json(
        &ctx,
        "std/llm/tool_parse",
        "parse_tool_calls",
        "__host_agent_parse_tool_calls",
        serde_json::json!({
            "text": text,
            "tools": tools.as_ref().map(helpers::vm_value_to_json),
            "tool_format": tool_format,
        }),
    )
    .await?;
    let calls = parsed
        .get_mut("tool_calls")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            VmError::Runtime(
                "__host_agent_parse_tool_calls: std/llm/tool_parse returned no tool_calls list"
                    .to_string(),
            )
        })?;
    tools::stamp_synthetic_tool_call_ids(
        calls,
        crate::agent_sessions::next_text_tool_call_seq_for_parse,
    );
    let stamped_calls = calls.clone();
    tool_parse_diagnostics::append_empty_required_arg_diagnostics(
        &mut parsed,
        &stamped_calls,
        tools.as_ref(),
    )?;
    parsed["calls"] = serde_json::Value::Array(stamped_calls);
    Ok(json_to_vm_value(&parsed))
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
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_dispatch_tool_batch(calls: list, tools?: {_type: \"tool_registry\", tools: list}?, options?: dict?) -> list",
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
    crate::llm::pairing_receipts::emit_tool_call_receipts(&calls);
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
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_dispatch_tool_call(call: dict, tools?: {_type: \"tool_registry\", tools: list}?, options?: dict?) -> dict",
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
    crate::llm::pairing_receipts::emit_tool_call_receipts(std::slice::from_ref(&call));
    host_agent_dispatch_tool_call(ctx, call, tools.as_ref(), &options).await
}

use super::agent_host_tool_dispatch::{pin_scoped_tool_dispatch, ToolDispatchRequest};

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
    // Build argument JSON lazily: normalization takes ownership, while denial
    // and feedback paths only derive it when they fire.
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
    tools::recover_provider_safe_alias(&mut tool_name, tools);
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
    let dispatch_annotations = tool_annotations_for(tools, &tool_name);
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
        || permissions::session_has_grants(&session_id)
        || crate::orchestration::tool_precheck_active();
    let _policy_guard = if policy_machinery_active {
        Some(agent_session_host::install_session_policy_guard(options)?)
    } else {
        None
    };
    let dispatch_policy =
        DispatchPolicy::new(policy_machinery_active, dispatch_annotations.as_ref());

    let mut approval_status = None;
    if let Err(policy_denial) = dispatch_policy.enforce(&tool_name, &tool_args, None) {
        if let Some(violation) = policy_denial.side_effect_ceiling {
            // A side-effect ceiling is the one static policy refusal that can
            // offer an explicit, dispatch-local ACP approval. The grant below
            // is exact (tool + active ceiling + required effect), non-stored,
            // and immediately rechecked before the call can proceed.
            // Offer the ceiling refusal to the reviewer before the host, the
            // same order the approval-policy path uses. Without this a run
            // carrying both a capability policy and a reviewer refuses the
            // call and never asks (harn#7982), which is every product loop.
            let reviewer_decision = crate::orchestration::maybe_grant_side_effect_by_auto_review(
                Some(&ctx),
                &tool_name,
                &tool_args,
                &session_id,
                violation.ceiling.as_str(),
                violation.required_level.as_str(),
                &policy_denial.reason,
            )
            .await;
            let reviewer_granted = reviewer_decision.is_some();
            let ceiling_outcome = match reviewer_decision {
                Some(policy_decision) => SideEffectPermissionOutcome::Allowed { policy_decision },
                None => {
                    request_side_effect_permission(
                        bridge.as_ref(),
                        &session_id,
                        &tool_id,
                        &tool_name,
                        &tool_args,
                        violation,
                        policy_denial.reason.clone(),
                        permission_context_for(tools, &tool_name),
                    )
                    .await
                }
            };
            match ceiling_outcome {
                SideEffectPermissionOutcome::Allowed { policy_decision } => {
                    let Some(grant) = policy_denial.side_effect_grant_for(&tool_name) else {
                        return Err(VmError::Runtime(
                            "side-effect approval missing its policy violation".to_string(),
                        ));
                    };
                    if let Err(recheck_denial) =
                        dispatch_policy.enforce(&tool_name, &tool_args, Some(&grant))
                    {
                        let denial = tool_denial_from_policy(recheck_denial, &tool_name);
                        return Ok(deny_tool_call_value(
                            Some(&ctx),
                            &session_id,
                            &tool_name,
                            &tool_id,
                            &tool_args,
                            denial,
                            false,
                            DenialEvidence::new(Some(policy_decision), None),
                        )
                        .await);
                    }
                    approval_status = Some(if reviewer_granted {
                        "auto_review_granted"
                    } else {
                        "host_granted"
                    });
                    emit_permission_event_with_policy(
                        &session_id,
                        "PermissionGrant",
                        &tool_name,
                        &tool_args,
                        if reviewer_granted {
                            "reviewer approved one-time side-effect ceiling exception"
                        } else {
                            "host approved one-time side-effect ceiling exception"
                        },
                        true,
                        Some(policy_decision),
                    );
                }
                SideEffectPermissionOutcome::Denied {
                    denial,
                    escalated,
                    policy_decision,
                } => {
                    return Ok(deny_tool_call_value(
                        Some(&ctx),
                        &session_id,
                        &tool_name,
                        &tool_id,
                        &tool_args,
                        denial,
                        escalated,
                        DenialEvidence::new(Some(policy_decision), None),
                    )
                    .await);
                }
            }
        } else {
            let denial = tool_denial_from_policy(policy_denial, &tool_name);
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
                Some(&ctx),
                &session_id,
                &tool_name,
                &tool_id,
                &tool_args,
                denial,
                false,
                DenialEvidence::new(None, schema_repair),
            )
            .await);
        }
    }

    // Deterministic pre-approval deny seam: consult an embedder-registered
    // precheck BEFORE the dynamic-permission and approval gates, so a call the
    // embedder has already decided to refuse is denied straight to the model
    // without ever emitting a `session/request_permission` prompt for a
    // predetermined-denied command. The audience-split refusal (model cue plus
    // optional machine reason / human summary) rides the standard denied
    // tool-result path. Fail-open: no precheck (or an unreadable verdict)
    // leaves dispatch byte-identical.
    if crate::orchestration::tool_precheck_active() {
        if let Some(precheck_denial) =
            crate::orchestration::run_tool_precheck(Some(&ctx), &tool_name, &tool_args, &session_id)
                .await?
        {
            let denial = crate::orchestration::precheck_tool_denial(precheck_denial);
            return Ok(deny_tool_call_value(
                Some(&ctx),
                &session_id,
                &tool_name,
                &tool_id,
                &tool_args,
                denial,
                false,
                DenialEvidence::new(None, None),
            )
            .await);
        }
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
                    Some(&ctx),
                    &session_id,
                    &tool_name,
                    &tool_id,
                    &tool_args,
                    denial,
                    escalated,
                    DenialEvidence::new(None, None),
                )
                .await);
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
    // The `AutoReview` seam: a refusal nobody is present to reconsider. Runs
    // AFTER the policy has decided and the trifecta gate has had its say, so
    // the reviewer sees the FINAL refusal and cannot pre-empt a gate that had
    // not run yet. Body lives in the owning module.
    if crate::orchestration::maybe_grant_by_auto_review(
        Some(&ctx),
        approval.as_mut(),
        &tool_name,
        &tool_args,
        &session_id,
    )
    .await
    {
        approval_status = Some("auto_review_granted");
    }

    match approval {
        None => {}
        // One arm for every allowed dispatch; the recorder names whoever
        // decided it. The old audit-signal condition also dropped a reviewer
        // grant on a rule carrying no id and no risk label.
        Some(decision)
            if decision.is_allow() && crate::orchestration::decision_records_a_grant(&decision) =>
        {
            record_allowed_dispatch(&session_id, &tool_id, &tool_name, &tool_args, &decision);
        }
        Some(decision) if decision.is_deny() => {
            emit_runtime_denied_activity(&session_id, &tool_id, &tool_name, &decision);
            let denial = crate::agent_events::ToolDenial::terminal(
                crate::agent_events::DenialGate::ApprovalPolicy,
                None,
                decision.reason,
            );
            return Ok(deny_tool_call_value(
                Some(&ctx),
                &session_id,
                &tool_name,
                &tool_id,
                &tool_args,
                denial,
                false,
                DenialEvidence::new(Some(decision.receipt), None),
            )
            .await);
        }
        Some(decision) if decision.is_ask() => {
            let approval_id = if tool_id.is_empty() {
                format!("tool_call_{}", uuid::Uuid::now_v7())
            } else {
                tool_id.clone()
            };
            let no_host_bridge = bridge.is_none();
            let request = HostPermissionRequest {
                session_id: session_id.clone(),
                tool_call_id: approval_id.clone(),
                tool_name: tool_name.clone(),
                tool_args: tool_args.clone(),
                policy_decision: decision.receipt.clone(),
                request_context: serde_json::json!({"policy_decision": decision.receipt.clone()}),
                requested_capabilities: vec![format!("tool.{tool_name}")],
                tool_descriptor: tool_descriptor_for(tools, &tool_name),
                tool_annotations: tool_annotations_for(tools, &tool_name),
            };
            match request_host_permission(bridge.as_ref(), request).await {
                HostPermissionOutcome::Allowed {
                    response,
                    resolution,
                } => {
                    // Preserve the established static-approval extension.
                    // Side-effect grants intentionally never accept rewritten
                    // arguments: their exact exception was approved for the
                    // original dispatch and is rechecked before execution.
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
                    emit_runtime_resolved_activity(
                        &session_id,
                        &approval_id,
                        &tool_name,
                        &decision,
                        resolution,
                    );
                }
                HostPermissionOutcome::Rejected { reason, resolution } => {
                    emit_runtime_resolved_activity(
                        &session_id,
                        &approval_id,
                        &tool_name,
                        &decision,
                        resolution,
                    );
                    let denial = crate::agent_events::ToolDenial::terminal(
                        crate::agent_events::DenialGate::HostRejected,
                        None,
                        reason,
                    );
                    return Ok(deny_tool_call_value(
                        Some(&ctx),
                        &session_id,
                        &tool_name,
                        &tool_id,
                        &tool_args,
                        denial,
                        true,
                        DenialEvidence::new(Some(decision.receipt.clone()), None),
                    )
                    .await);
                }
                HostPermissionOutcome::Unavailable => {
                    emit_runtime_unavailable_activity(
                        &session_id,
                        &approval_id,
                        &tool_name,
                        &decision,
                    );
                    let (denial_class, repeat_count) =
                        crate::orchestration::next_approval_unavailable_class_repeat_count(
                            &session_id,
                            &decision.risk_labels,
                        );
                    let denial = crate::agent_events::ToolDenial::terminal(
                        crate::agent_events::DenialGate::ApprovalUnavailable,
                        None,
                        if no_host_bridge {
                            "approval required but no host bridge is available"
                        } else {
                            "approval request failed or host does not implement session/request_permission"
                        },
                    )
                    .with_denial_class(denial_class, repeat_count);
                    return Ok(deny_tool_call_value(
                        Some(&ctx),
                        &session_id,
                        &tool_name,
                        &tool_id,
                        &tool_args,
                        denial,
                        true,
                        DenialEvidence::new(Some(decision.receipt.clone()), None),
                    )
                    .await);
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
            Some(&ctx),
            &session_id,
            &tool_name,
            &tool_id,
            &tool_args,
            denial,
            false,
            DenialEvidence::new(None, None),
        )
        .await;
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
    // Heap-pin the dispatch future on a frame that dies immediately. The
    // tool-execution path builds large per-call state (e.g.
    // `LlmCallOptions`/`LlmRequestPayload`), and both await arms below consume
    // the `Pin<Box<_>>` directly, but the box alone is not enough: see
    // `pin_scoped_tool_dispatch`.
    let mut dispatch_future = pin_scoped_tool_dispatch(
        session_id.clone(),
        tool_id.clone(),
        ToolDispatchRequest {
            ctx: &ctx,
            tool_name: &tool_name,
            tool_args: &tool_args,
            tools,
            mcp_clients: mcp_clients_ref,
            bridge: bridge.as_ref(),
            tool_retries,
            tool_backoff_ms,
        },
    );
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
                        declared_failure: None,
                    },
                    true,
                ),
            }
        }
        None => (dispatch_future.await, false),
    };
    let execution_duration_ms = started.elapsed().as_millis() as u64;
    let declared_failure = outcome.declared_failure;
    let executor = outcome
        .executor
        .as_ref()
        .and_then(|e| serde_json::to_value(e).ok());

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
            let mutation_status = structured_tool_result::mutation_status(&raw_result);
            let changed_paths = structured_tool_result::changed_paths(&raw_result);
            let data = structured_tool_result::data(&raw_result);
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
            let hook_result = rendered?;
            let dropped_bytes = hook_result.dropped_bytes;
            let rendered = hook_result.text;
            let output_truncated = dropped_bytes > 0;
            if output_truncated {
                crate::boundary::BoundaryFailure::new(
                    crate::boundary::BoundaryId::PostToolOutput,
                    crate::boundary::BoundaryFailureKind::Truncated,
                    format!("PostToolUse hooks truncated output from tool {tool_name}"),
                )
                .with_dropped_bytes(dropped_bytes)
                .in_session(&session_id)
                .report();
            }
            let reminder_payload = serde_json::json!({
                "event": crate::orchestration::HookEvent::PostToolUse.as_str(),
                "session": {"id": &session_id},
                "iteration": agent_primitive_option_int(options, "_iteration").unwrap_or(0),
                "tool": {"name": &tool_name, "args": &tool_args},
                "tool_name": &tool_name,
                "result": {
                    "text": &rendered,
                    "truncated": output_truncated,
                    "dropped_bytes": dropped_bytes,
                    "original_size": rendered_before_hooks.len(),
                    "final_size": rendered.len(),
                },
                "truncated": output_truncated,
                "dropped_bytes": dropped_bytes,
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
            // A dispatch that returned `Ok(..)` can still carry a failure in its
            // body (`{ok:false}` / `{status:"error"}` / `{error:".."}`, or an
            // MCP-shaped `{isError:true}`). Surface those instead of laundering
            // them into `ok:true`: the agent loop reads `ok`/`status`.
            // Prefer the pre-coercion declaration: a dict-returning handler's
            // coerced payload no longer parses (harn#7884).
            let error_category = if denied {
                Some("tool_rejected")
            } else {
                declared_failure.or_else(|| agent_tools::ok_result_failure_category(&raw_result))
            };
            let is_failure = error_category.is_some();
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
                "data": data,
                "executor": executor,
                "approval": approval_status,
                "execution_duration_ms": execution_duration_ms,
                "tool_output_truncated": output_truncated,
                "dropped_bytes": dropped_bytes,
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
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_mcp_bootstrap(session_id: string, specs?: list|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_mcp_bootstrap_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
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
    let fixtures = ctx
        .child_vm()
        .harness()
        .map(|harness| harness.inner().fixtures_arc());

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
                if let Some(fixtures) = fixtures.as_ref() {
                    handle.set_capability_fixtures(fixtures.clone()).await;
                }
                let discovery = handle
                    .discovery_result
                    .lock()
                    .await
                    .clone()
                    .unwrap_or(serde_json::Value::Null);
                let instructions = discovery
                    .get("instructions")
                    .or_else(|| {
                        discovery
                            .get("serverInfo")
                            .and_then(|value| value.get("instructions"))
                    })
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                server_infos.push(serde_json::json!({
                    "name": server_name.clone(),
                    "discovery": discovery,
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
    exposure = "runtime_internal",
    effects = [],
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
    exposure = "runtime_internal",
    effects = [],
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
mod approval_unavailable_tests;
#[cfg(test)]
mod auto_review_decider_tests;
#[cfg(test)]
mod denied_tool_routing_tests;
#[cfg(test)]
mod mcp_bootstrap_tests;
#[cfg(test)]
mod parse_tool_call_id_tests;
#[cfg(test)]
mod schema_argument_dispatch_tests;
#[cfg(test)]
mod security_gate_tests;
#[cfg(test)]
mod session_scope_tests;
#[cfg(test)]
mod side_effect_ceiling_tests;
#[cfg(test)]
mod tool_failure_recording_tests;
#[cfg(test)]
mod tool_output_truncation_tests;
