//! Tool dispatch helpers used by the agent-loop host primitives.

use std::sync::Arc;

use crate::agent_events::ToolExecutor;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};

pub(super) mod approval_denials;
pub(super) mod denials;
pub(super) mod handler_result;
pub(super) mod hash;

use handler_result::agent_tool_handler_result_text;

pub(super) fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    denials::denied_tool_result(tool_name, reason)
}

/// Run a Harn-side tool handler in a child VM. The bridge is trusted for the
/// call, `host_call` is rejected inside the handler, and captured output is
/// forwarded to the parent context. A `ToolRejected` error becomes a denied
/// tool result rather than a dispatch failure.
async fn run_tool_handler(
    mut vm: crate::vm::Vm,
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    handler: &VmValue,
    tool_name: &str,
    tool_args: &serde_json::Value,
    declared_failure: &mut Option<&'static str>,
) -> Result<serde_json::Value, VmError> {
    let args_vm = crate::stdlib::json_to_vm_value(tool_args);
    let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
    let outcome = crate::tool_handler_scope::scope(vm.call_closure_pub(handler, &[args_vm])).await;
    let captured = vm.take_output();
    if let Some(ctx) = ctx {
        ctx.forward_output(&captured);
    }
    match outcome {
        Ok(val) => {
            let (payload, failure) = handler_result::coerce_and_classify_handler_result(&val);
            *declared_failure = failure;
            Ok(payload)
        }
        Err(VmError::CategorizedError {
            message,
            category: ErrorCategory::ToolRejected,
        }) => Ok(denied_tool_result(tool_name, message)),
        Err(e) => Err(e),
    }
}

pub(super) fn side_effect_ceiling_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
    details: &crate::agent_events::SideEffectCeilingDetails,
) -> serde_json::Value {
    denials::side_effect_ceiling_tool_result(tool_name, reason, details)
}

/// Build the tool-result body for a NAME-RESOLUTION failure: the call was
/// refused because its name is not in the session's available tool set
/// (`DenialGate::ToolCeiling`), not because a granted capability is missing.
/// Permission framing is actively harmful here — on a headless run the model
/// reads "tell the user what you need permission for", starts petitioning a
/// user that does not exist, and stalls. Keep the wording action-oriented:
/// name the failure class, steer off a re-send, list the callable tools, and
/// show the call shape. Genuinely permission-gated denials (capability /
/// side-effect ceilings, approval and host rejections) keep
/// [`denied_tool_result`].
pub(super) fn unavailable_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
) -> serde_json::Value {
    let reason = reason.into();
    let allowed = crate::orchestration::current_allowed_tool_names();
    let available_clause = if allowed.is_empty() {
        String::new()
    } else {
        format!(" Available tools: {}.", allowed.join(", "))
    };
    let next_step = format!(
        "`{tool_name}` is not one of the available tools, so re-sending this call will \
         fail the same way. This is a tool-name mistake to correct yourself, not \
         something to ask the user about: pick the available tool that does what you \
         intended and call it directly as `name({{ ... }})`.{available_clause}"
    );
    serde_json::json!({
        "error": "unknown_tool",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}

/// Build corrective feedback when a native call contains one valid embedded
/// text-format call. Never dispatches the repaired call, which would bypass
/// the original call's approval flow.
pub(super) async fn embedded_call_repair_result(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    tool_name: &str,
    tool_args: &serde_json::Value,
) -> Option<serde_json::Value> {
    let tag = crate::llm::tools::TEXT_TOOL_CALL_TAG;
    let wrapper_named = crate::llm::tools::is_generic_wrapper_name(tool_name)
        || tool_name == crate::llm::tools::TEXT_TOOL_CALL_TAG_COMPACT;
    // The text parser only recognizes calls to KNOWN tools; project the active
    // policy's allowlist into a minimal tool catalog so the embedded call is
    // validated against exactly the set the model may use. A ToolCeiling
    // denial implies a non-empty allowlist (an empty list means "no ceiling"),
    // so an empty list here means there is nothing safe to coach.
    let allowed = crate::orchestration::current_allowed_tool_names();
    if allowed.is_empty() {
        return None;
    }
    let name_field_payload = call_shaped_tool_name_payload(tool_name, &allowed);
    if !wrapper_named && name_field_payload.is_none() {
        return None;
    }
    let payload = if let Some(payload) = name_field_payload.as_ref() {
        payload.clone()
    } else {
        embedded_call_payload_text(tool_args)?
    };
    let tools_json = serde_json::json!({
        "tools": allowed
            .iter()
            .map(|name| serde_json::json!({ "name": name }))
            .collect::<Vec<_>>(),
    });
    let tools_val = crate::stdlib::json_to_vm_value(&tools_json);
    let parsed = crate::llm::api::parse_text_tools_with_harn(ctx, &payload, Some(&tools_val), "")
        .await
        .ok()?;
    if !parsed.errors.is_empty() || parsed.calls.len() != 1 {
        if name_field_payload.is_some() {
            return Some(call_shaped_tool_name_repair_result(
                tool_name,
                parsed.errors.first().map(String::as_str),
            ));
        }
        return None;
    }
    let call = &parsed.calls[0];
    let inner_name = call.get("name")?.as_str()?.trim().to_string();
    if inner_name.is_empty()
        || crate::llm::tools::is_generic_wrapper_name(&inner_name)
        || !allowed.iter().any(|name| name == &inner_name)
    {
        return None;
    }
    let inner_args = call
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let rendered_args = serde_json::to_string(&inner_args).unwrap_or_else(|_| "{ ... }".into());
    // Echo the corrected invocation only when it is short enough to repeat
    // verbatim; a long payload (e.g. an edit body) is coached by reference so
    // the feedback itself does not balloon the turn.
    let corrected_invocation = if rendered_args.chars().count() <= 400 {
        format!("{inner_name}({rendered_args})")
    } else {
        format!("{inner_name}({{ ...the same arguments you already wrote... }})")
    };
    let reason = if name_field_payload.is_some() {
        format!(
            "`{tool_name}` is not a tool name — it contains a complete text-format call \
             to `{inner_name}` in the tool-name field."
        )
    } else {
        format!(
            "`{tool_name}` is not a tool name — `<{tag}>` is the wrapper tag of the text \
             tool-call format, and this call's arguments contain a complete call to \
             `{inner_name}`."
        )
    };
    let next_step = format!(
        "Your call was understood but mis-addressed. Re-issue the embedded call directly, \
         using `{inner_name}` as the tool name and no wrapper tags: {corrected_invocation}"
    );
    Some(serde_json::json!({
        "error": "invalid_arguments",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    }))
}

fn call_shaped_tool_name_payload(tool_name: &str, allowed: &[String]) -> Option<String> {
    let trimmed = tool_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let unwrapped = trimmed.strip_prefix('<').unwrap_or(trimmed).trim_start();
    for name in allowed {
        let Some(rest) = unwrapped.strip_prefix(name) else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            return Some(unwrapped.to_string());
        }
    }
    None
}

fn call_shaped_tool_name_repair_result(
    tool_name: &str,
    parser_error: Option<&str>,
) -> serde_json::Value {
    let reason = if let Some(error) = parser_error {
        format!(
            "`{tool_name}` is not a tool name — it is a malformed text-format call in \
             the tool-name field. Parser diagnostic: {error}"
        )
    } else {
        format!(
            "`{tool_name}` is not a tool name — it looks like a text-format call in \
             the tool-name field."
        )
    };
    serde_json::json!({
        "error": "invalid_arguments",
        "tool": tool_name,
        "reason": reason,
        "next_step": "Move the call expression out of the tool-name field. Re-emit a real tool name with its arguments object, or emit one complete text-format `<tool_call>name({ ... })</tool_call>` block.",
    })
}

/// Extract the text a wrapper-named call most plausibly smuggled its real
/// call through: a bare string argument, the streamed-arguments fallback's
/// `{"__parse_error": "... Raw input: <raw>"}` carrier, or a single
/// string-valued field (e.g. `{"input": "<tool_call>..."}`).
fn embedded_call_payload_text(tool_args: &serde_json::Value) -> Option<String> {
    match tool_args {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) => {
            if let Some(parse_error) = map.get("__parse_error").and_then(|v| v.as_str()) {
                return parse_error
                    .split_once("Raw input: ")
                    .map(|(_, raw)| raw.to_string());
            }
            if map.len() == 1 {
                if let Some(text) = map.values().next().and_then(|v| v.as_str()) {
                    return Some(text.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Build the tool-result body for a RECOVERABLE rejection — a schema /
/// argument-validation failure or a malformed (empty) tool name. Unlike
/// [`denied_tool_result`], this is explicitly retry-POSITIVE: the model made a
/// fixable slip, so the guidance tells it to re-call the same tool *with the
/// correction*, naming the specific missing/invalid parameter(s) when the
/// `reason` carries them.
///
/// Using `error: "invalid_arguments"` (NOT `permission_denied`) is load-bearing
/// — it keeps `is_denied_tool_result` from misclassifying a fixable mistake as a
/// hard denial, and it stops cheap models from giving up after one correctable
/// error (observed live: a model called `edit` without `path`, read
/// `permission_denied / do not retry`, then made zero further edits and timed
/// out into a false FAIL; ~26 recent eval transcripts show this pattern).
pub(super) fn recoverable_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
) -> serde_json::Value {
    let reason = reason.into();
    // The validator phrases missing params as
    // "... missing required parameter(s): path, mode. ...". Pull the named
    // params out so the next_step can be concretely actionable instead of
    // generic. Falls back to a generic-but-still-retry-positive nudge when no
    // parameter list is present (e.g. the empty-tool-name slip).
    let missing_params = extract_missing_params(&reason);
    let next_step = match (tool_name, missing_params.as_deref()) {
        ("<unnamed>", _) => "This was a malformed tool call (no tool name). It is fixable — \
             emit exactly one tool call this turn as `name({ ... })` using a non-empty tool \
             name from the allowed list, with all required parameters."
            .to_string(),
        (name, Some(params)) => format!(
            "This is a fixable argument error, not a permission denial. \
             Re-call `{name}` with the missing required parameter(s): {params}."
        ),
        (name, None) => format!(
            "This is a fixable argument error, not a permission denial. \
             Re-call `{name}` with corrected arguments per the reason above."
        ),
    };
    serde_json::json!({
        "error": "invalid_arguments",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}

/// Extract the named missing parameters from a validator `reason` of the form
/// `"Tool 'x' is missing required parameter(s): a, b. ..."`. Returns the
/// comma-separated parameter list (e.g. `"a, b"`) when present, else `None`.
#[expect(
    clippy::string_slice,
    reason = "start is find() of the ASCII marker plus its length; end is find() on the tail"
)]
fn extract_missing_params(reason: &str) -> Option<String> {
    let marker = "missing required parameter(s):";
    let start = reason.find(marker)? + marker.len();
    let tail = reason[start..].trim_start();
    // The list runs up to the sentence-ending period the validator appends.
    let end = tail.find('.').unwrap_or(tail.len());
    let params = tail[..end].trim();
    if params.is_empty() {
        None
    } else {
        Some(params.to_string())
    }
}

pub(super) fn render_tool_result(value: &serde_json::Value) -> String {
    if let Some(text) = agent_tool_handler_result_text(value) {
        text.to_string()
    } else if let Some(text) = value.as_str() {
        text.to_string()
    } else if value.is_null() {
        "(no output)".to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }
}

/// A base64 image payload longer than this is replaced by a `<... N bytes>`
/// marker in the rendered transcript text. A 1024x768 PNG screenshot is ~1MB of
/// base64, which would swamp the transcript — but the full payload still travels
/// to the model as an image content block (see the tool-result recording path),
/// so eliding it from the *text* rendering is pure hygiene, not data loss.
const RENDERED_IMAGE_BASE64_ELIDE_THRESHOLD: usize = 512;

/// Return a copy of a tool result with any large `base64` image payload replaced
/// by a compact `<screenshot base64 elided: N bytes>` marker, so the transcript
/// text stays small. Recurses through objects and arrays so a screenshot nested
/// under `screenshot`/`image` (the computer tool's `{ok, text, screenshot:{...}}`
/// shape) is elided wherever it sits. Non-image results are returned unchanged
/// (structurally identical), so this is a no-op for every existing tool.
pub(super) fn elide_image_base64(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, child) in map {
                if key == "base64" {
                    if let Some(data) = child.as_str() {
                        if data.len() > RENDERED_IMAGE_BASE64_ELIDE_THRESHOLD {
                            out.insert(
                                key.clone(),
                                serde_json::json!(format!("<base64 elided: {} bytes>", data.len())),
                            );
                            continue;
                        }
                    }
                }
                out.insert(key.clone(), elide_image_base64(child));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(elide_image_base64).collect())
        }
        other => other.clone(),
    }
}

pub(super) fn is_denied_tool_result(value: &serde_json::Value) -> bool {
    if is_denied_tool_result_object(value) {
        return true;
    }
    value
        .as_str()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .is_some_and(|parsed| is_denied_tool_result_object(&parsed))
}

fn is_denied_tool_result_object(value: &serde_json::Value) -> bool {
    value
        .get("error")
        .and_then(|error| error.as_str())
        .is_some_and(|error| error == "permission_denied")
        || value
            .get("blocked")
            .and_then(|blocked| blocked.as_bool())
            .unwrap_or(false)
        || value
            .get("status")
            .and_then(|status| status.as_str())
            .is_some_and(|status| status == "blocked")
}

/// Classify a tool result that came back as `Ok(value)` (no Rust-level error).
///
/// A tool/host primitive can complete the dispatch without throwing yet still
/// signal a *failure* in the result body — e.g. the host bridge returns a
/// structured `{"ok": false, ...}` / `{"status": "error", ...}` / `{"error":
/// "..."}` envelope, or an MCP-shaped `{"isError": true}` body that wasn't
/// already converted to a thrown error. Returning `None` means the result is a
/// genuine success; `Some(error_category)` means it represents a failure that
/// must be surfaced as `ok: false` to the agent loop, not laundered into a
/// success. Denials are classified by [`is_denied_tool_result`] upstream; this
/// covers the broader failure shapes.
pub(super) fn ok_result_failure_category(value: &serde_json::Value) -> Option<&'static str> {
    // The body may be a JSON string carrying the real object (host bridges that
    // stringify their envelope) — inspect the parsed form in that case.
    if let Some(parsed) = value
        .as_str()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    {
        return ok_result_failure_category_object(&parsed);
    }
    ok_result_failure_category_object(value)
}

fn ok_result_failure_category_object(value: &serde_json::Value) -> Option<&'static str> {
    // Only structured objects carry these signals; scalars/strings/arrays are
    // ordinary successful output.
    let obj = value.as_object()?;

    // Explicit boolean failure flags win first.
    if obj.get("ok").and_then(serde_json::Value::as_bool) == Some(false)
        || obj.get("success").and_then(serde_json::Value::as_bool) == Some(false)
        || obj.get("isError").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return Some("tool_error");
    }

    // Failure status strings.
    if let Some(status) = obj.get("status").and_then(serde_json::Value::as_str) {
        let status = status.trim().to_ascii_lowercase();
        if matches!(status.as_str(), "error" | "failed" | "failure") {
            return Some("tool_error");
        }
    }

    // A non-empty `error` string with no contradicting success signal. Guard
    // against false positives: `{"ok": true, "error": null}` and an empty error
    // are successes; only a populated error with no positive ok/status counts.
    if let Some(error) = obj.get("error").and_then(serde_json::Value::as_str) {
        if !error.trim().is_empty()
            && obj.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
            && obj.get("success").and_then(serde_json::Value::as_bool) != Some(true)
            && obj
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                != Some("ok")
        {
            return Some("tool_error");
        }
    }

    None
}

/// Outcome of a single tool dispatch — pairs the result with the
/// backend that ran it for projection through `AgentEvent::ToolCallUpdate`.
pub(super) struct ToolDispatchOutcome {
    pub result: Result<serde_json::Value, VmError>,
    pub executor: Option<ToolExecutor>,
    /// A failure the handler declared in its return value, read while that
    /// value was still structured (harn#7884).
    pub declared_failure: Option<&'static str>,
}

/// Dispatch a single tool invocation to its execution backend, recording
/// which backend actually answered. The returned `executor` is `None`
/// only when no backend could handle the call (no script handler, no
/// bridge, not handled locally) — i.e. the categorized "tool not
/// available" error. Retries don't change the executor: a tool that
/// resolves via the bridge stays a `HostBridge` call across attempts.
#[cfg(test)]
pub(super) async fn dispatch_tool_execution(
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> ToolDispatchOutcome {
    dispatch_tool_execution_with_mcp(
        None,
        tool_name,
        tool_args,
        tools_val,
        None,
        bridge,
        tool_retries,
        tool_backoff_ms,
    )
    .await
}

pub(super) async fn dispatch_tool_execution_with_mcp(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    mcp_clients: Option<&std::collections::BTreeMap<String, crate::mcp::VmMcpClientHandle>>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> ToolDispatchOutcome {
    use super::tools::handle_tool_locally;

    if let Some(outcome) =
        super::agent_tool_governance::registry_dispatch_rejection(tools_val, tool_name)
    {
        return outcome;
    }

    // Honor the declared executor (harn#743) ahead of the historic
    // heuristic so a tool defined as `executor: "host_bridge"` always
    // reports `HostBridge` on the wire — even if a stale handler value
    // happens to be on the dict, and even if the host bridge is also
    // capable of serving builtins.
    let declared = declared_executor_for_tool(tools_val, tool_name);
    let mut attempt = 0usize;
    let mut executor: Option<ToolExecutor> = None;
    // Reset per attempt: a retry re-runs the handler and re-reads whatever the
    // new return value declares.
    let mut declared_failure: Option<&'static str>;
    loop {
        declared_failure = None;
        let result = if matches!(declared.as_deref(), Some("provider_native")) {
            // The runtime never dispatches provider-native tools — the
            // model returns the already-executed result inline. Reaching
            // here means the model emitted a tool call against a tool
            // we're not supposed to run; surface that explicitly so the
            // turn doesn't silently swallow it.
            executor = Some(ToolExecutor::ProviderNative);
            Err(VmError::CategorizedError {
                message: format!(
                    "tool '{tool_name}' is declared executor: \"provider_native\" — \
                     the runtime does not dispatch these locally; the provider must \
                     have already executed the call"
                ),
                category: ErrorCategory::ToolRejected,
            })
        } else if matches!(declared.as_deref(), Some("host_bridge")) {
            // Force-route declared host-bridge tools through the bridge
            // even if a stale `handler` value is present. Without a
            // bridge, fail clearly instead of silently falling back.
            let Some(bridge) = bridge else {
                executor = Some(ToolExecutor::HostBridge);
                return ToolDispatchOutcome {
                    declared_failure: None,
                    result: Err(VmError::CategorizedError {
                        message: format!(
                            "tool '{tool_name}' is declared executor: \"host_bridge\" \
                             but no host bridge is connected to this environment"
                        ),
                        category: ErrorCategory::ToolRejected,
                    }),
                    executor,
                };
            };
            executor = Some(ToolExecutor::HostBridge);
            match bridge
                .call(
                    "builtin_call",
                    serde_json::json!({
                        "name": tool_name,
                        "args": [tool_args],
                    }),
                )
                .await
            {
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                other => other,
            }
        } else if matches!(declared.as_deref(), Some("mcp_server")) {
            // Declared MCP-served — prefer the configured `mcp_server`
            // field, fall back to the `_mcp_server` annotation.
            let server_name = declared_mcp_server_for_tool(tools_val, tool_name)
                .or_else(|| mcp_server_for_tool(tools_val, tool_name))
                .unwrap_or_else(|| "mcp".to_string());
            executor = Some(ToolExecutor::McpServer {
                server_name: server_name.clone(),
            });
            if let Some(client) = mcp_clients.and_then(|clients| clients.get(&server_name)) {
                let original_name = declared_mcp_tool_name_for_tool(tools_val, tool_name)
                    .unwrap_or_else(|| tool_name.to_string());
                crate::mcp::call_mcp_tool(client, &original_name, tool_args.clone()).await
            } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
                // MCP-served tools defined by the host are typically served
                // through the host bridge today; preserve that path. A
                // Harn-side `handler` overrides (custom MCP wrappers).
                let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
                    return ToolDispatchOutcome {
                        declared_failure: None,
                        result: Err(VmError::CategorizedError {
                            message: format!(
                                "tool '{tool_name}' is MCP-served but no child VM context was available"
                            ),
                            category: ErrorCategory::ToolRejected,
                        }),
                        executor,
                    };
                };
                run_tool_handler(
                    vm,
                    ctx,
                    &handler,
                    tool_name,
                    tool_args,
                    &mut declared_failure,
                )
                .await
            } else if let Some(bridge) = bridge {
                match bridge
                    .call(
                        "builtin_call",
                        serde_json::json!({
                            "name": tool_name,
                            "args": [tool_args],
                        }),
                    )
                    .await
                {
                    Err(VmError::CategorizedError {
                        message,
                        category: ErrorCategory::ToolRejected,
                    }) => Ok(denied_tool_result(tool_name, message)),
                    other => other,
                }
            } else {
                Err(VmError::CategorizedError {
                    message: format!(
                        "tool '{tool_name}' (mcp_server: \"{server_name}\") cannot be \
                         dispatched: no direct MCP client, bridge, or Harn handler"
                    ),
                    category: ErrorCategory::ToolRejected,
                })
            }
        } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
            // User-registered Harn handler. Runs BEFORE the vm-stdlib
            // short-circuit so user-defined tool semantics always win
            // over the runtime's built-in `read_file`/`list_directory`
            // shortcuts; otherwise a script that registers `read_file`
            // with a custom handler (mock data, sandboxed paths, audit
            // wrappers) would silently get the built-in real-filesystem
            // read instead of the user's intent.
            //
            // If the tool was sourced from `mcp_list_tools`, the dict
            // carries the originating server name as `_mcp_server`, and
            // the call is semantically "served by MCP" even though
            // dispatch goes through a Harn closure that ultimately
            // invokes mcp_call.
            executor = Some(match mcp_server_for_tool(tools_val, tool_name) {
                Some(server_name) => ToolExecutor::McpServer { server_name },
                None => ToolExecutor::HarnBuiltin,
            });
            let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
                return ToolDispatchOutcome {
                    declared_failure: None,
                    result: Err(VmError::CategorizedError {
                        message: format!(
                            "tool '{tool_name}' is Harn-owned but no child VM context was available"
                        ),
                        category: ErrorCategory::ToolRejected,
                    }),
                    executor,
                };
            };
            run_tool_handler(
                vm,
                ctx,
                &handler,
                tool_name,
                tool_args,
                &mut declared_failure,
            )
            .await
        } else if let Some(local_result) = handle_tool_locally(tool_name, tool_args) {
            // VM-stdlib short-circuit (read_file / list_directory) used
            // when no user handler is registered for a tool name harn
            // can service from its own stdlib. Provides the implicit
            // "free" tools without forcing every script to wire them.
            executor = Some(ToolExecutor::HarnBuiltin);
            Ok(serde_json::Value::String(local_result))
        } else if let Some(bridge) = bridge {
            // Same `_mcp_server` discriminator: a host that surfaces an
            // MCP server's tools without a Harn-side closure (e.g. the
            // CLI's eager-connect path) still routes through the bridge,
            // but the executor is the MCP server, not the bridge itself.
            executor = Some(match mcp_server_for_tool(tools_val, tool_name) {
                Some(server_name) => ToolExecutor::McpServer { server_name },
                None => ToolExecutor::HostBridge,
            });
            match bridge
                .call(
                    "builtin_call",
                    serde_json::json!({
                        "name": tool_name,
                        "args": [tool_args],
                    }),
                )
                .await
            {
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                other => other,
            }
        } else {
            // No backend could claim the call — leave executor unset so
            // the caller reports "tool unavailable" rather than blaming
            // a specific backend.
            Err(VmError::CategorizedError {
                message: format!(
                    "Tool '{tool_name}' is not available in the current environment. \
                     Use only the tools listed in the tool-calling contract."
                ),
                category: ErrorCategory::ToolRejected,
            })
        };
        // Retry only a transient dispatch error with budget left. An internal
        // engine/wiring bug (undefined builtin, corrupt bytecode) will never
        // resolve on retry, so it breaks immediately and propagates to the
        // agent loop rather than burning the budget as a tool observation.
        let retryable = matches!(&result, Err(error)
            if !matches!(
                error,
                VmError::CategorizedError {
                    category: ErrorCategory::ToolRejected,
                    ..
                }
            ) && !crate::value::error_to_category(error).is_internal()
            && attempt < tool_retries);
        if !retryable {
            break ToolDispatchOutcome {
                result,
                executor,
                declared_failure,
            };
        }
        attempt += 1;
        let delay = tool_backoff_ms * (1u64 << attempt.min(5));
        crate::clock_mock::sleep(tokio::time::Duration::from_millis(delay)).await;
    }
}

/// Inspect `tools_val` for a `_mcp_server` annotation on the entry
/// matching `tool_name`. Returns the originating server name when the
/// tool was sourced from `mcp_list_tools`, otherwise `None`. The
/// annotation is a free-form dict key (it travels alongside the
/// schema), so we also peek at a `function` sub-dict for OpenAI-shape
/// entries that nest the metadata.
pub(super) fn mcp_server_for_tool(tools_val: Option<&VmValue>, tool_name: &str) -> Option<String> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &crate::value::DictMap = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = match entry.get("name") {
            Some(v) => v.display(),
            None => entry
                .get("function")
                .and_then(|f| f.as_dict())
                .and_then(|f| f.get("name"))
                .map(|v| v.display())
                .unwrap_or_default(),
        };
        if name != tool_name {
            continue;
        }
        if let Some(VmValue::String(s)) = entry.get("_mcp_server") {
            return Some(s.to_string());
        }
        if let Some(VmValue::Dict(func)) = entry.get("function") {
            if let Some(VmValue::String(s)) = func.get("_mcp_server") {
                return Some(s.to_string());
            }
        }
        return None;
    }
    None
}

/// Return the canonical declared executor for `tool_name`, if the
/// registry entry carries one (harn#743). The wire form
/// (`"harn_builtin"`) is canonicalized to `"harn"` on storage; this
/// helper returns whatever is stored, so callers can compare against
/// the documented set without re-aliasing.
///
/// `None` means the entry pre-dates the `executor` field (e.g. an
/// `mcp_list_tools` result the user pushed straight into the
/// registry) — callers fall back to the historic
/// handler/`_mcp_server`/bridge heuristic.
pub(super) fn declared_executor_for_tool(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<String> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &crate::value::DictMap = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = match entry.get("name") {
            Some(v) => v.display(),
            None => continue,
        };
        if name != tool_name {
            continue;
        }
        if let Some(VmValue::String(s)) = entry.get("executor") {
            return Some(s.to_string());
        }
        return None;
    }
    None
}

/// Return the configured `mcp_server` name on `tool_name`'s entry, set
/// either via `tool_define({executor: "mcp_server", mcp_server: "..."})`
/// or via the implicit `_mcp_server` annotation `mcp_list_tools` injects.
fn declared_mcp_server_for_tool(tools_val: Option<&VmValue>, tool_name: &str) -> Option<String> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &crate::value::DictMap = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        if entry.get("name").map(|v| v.display()).as_deref() != Some(tool_name) {
            continue;
        }
        if let Some(VmValue::String(s)) = entry.get("mcp_server") {
            return Some(s.to_string());
        }
        return None;
    }
    None
}

fn declared_mcp_tool_name_for_tool(tools_val: Option<&VmValue>, tool_name: &str) -> Option<String> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &crate::value::DictMap = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        if entry.get("name").map(|v| v.display()).as_deref() != Some(tool_name) {
            continue;
        }
        if let Some(VmValue::String(s)) = entry.get("_mcp_tool_name") {
            return Some(s.to_string());
        }
        return None;
    }
    None
}

/// Look up the Harn-defined handler closure for a tool, if any.
pub(super) fn find_tool_handler(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<std::sync::Arc<VmClosure>> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &crate::value::DictMap = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = match entry.get("name") {
            Some(v) => v.display(),
            None => continue,
        };
        if name == tool_name {
            if let Some(VmValue::Closure(c)) = entry.get("handler") {
                return Some(std::sync::Arc::clone(c));
            }
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests;
