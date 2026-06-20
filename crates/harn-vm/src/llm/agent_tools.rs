//! Tool dispatch helpers used by the agent-loop host primitives.

use std::sync::Arc;

use crate::agent_events::ToolExecutor;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};

/// Hash a serde_json::Value deterministically for dedup purposes.
pub(super) fn stable_hash(val: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let canonical = serde_json::to_string(val).unwrap_or_default();
    canonical.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    let reason = reason.into();
    // A bare `{"error":"permission_denied", ...}` tells the model what was
    // blocked but not what to do instead, so it tends to retry the same denied
    // call. Add a generic, capability-gate-appropriate next step: don't repeat
    // the call, find another way to make progress, or ask for the permission.
    //
    // RESERVED FOR TRUE POLICY/PERMISSION DENIALS (capability gate, sandbox /
    // command policy, host rejection). The "Do not retry the same call" wording
    // is *correct* there — the call is blocked and re-issuing it identically
    // will be blocked again. Recoverable rejections (bad/missing arguments, an
    // empty tool name) must NOT use this body: see `recoverable_tool_result`,
    // which coaches a retry *with the correction* instead.
    // Name the tools that ARE callable so the model can self-correct in one
    // turn instead of re-issuing the denied call or guessing another unlisted
    // name (live fw-gpt-oss-120b transcripts: cheap models emit Codex/container
    // vocab they were never shown, read a bare denial, and thrash). The active
    // policy's allowlist is the source of truth; omit the clause when the
    // surface is unbounded (allow-all) so we never assert a misleading list.
    let allowed = crate::orchestration::current_allowed_tool_names();
    let available_clause = if allowed.is_empty() {
        String::new()
    } else {
        format!(" Available tools: {}.", allowed.join(", "))
    };
    let next_step = format!(
        "The `{tool_name}` tool is not permitted right now. Do not retry the same call. \
         Make progress with the tools you are allowed to use, or if this capability is \
         essential, briefly tell the user what you need permission for and why.\
         {available_clause}"
    );
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
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
    if let Some(text) = value.as_str() {
        text.to_string()
    } else if value.is_null() {
        "(no output)".to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
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

pub(super) fn next_call_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Outcome of a single tool dispatch — pairs the result with the
/// backend that actually ran it (harn#691). The agent loop reads the
/// `executor` value when emitting `AgentEvent::ToolCallUpdate` so
/// clients can render "via mcp:linear" / "via host bridge" badges.
pub(super) struct ToolDispatchOutcome {
    pub result: Result<serde_json::Value, VmError>,
    pub executor: Option<ToolExecutor>,
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

    // Honor the declared executor (harn#743) ahead of the historic
    // heuristic so a tool defined as `executor: "host_bridge"` always
    // reports `HostBridge` on the wire — even if a stale handler value
    // happens to be on the dict, and even if the host bridge is also
    // capable of serving builtins.
    let declared = declared_executor_for_tool(tools_val, tool_name);
    let mut attempt = 0usize;
    let mut executor: Option<ToolExecutor> = None;
    loop {
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
                        result: Err(VmError::CategorizedError {
                            message: format!(
                                "tool '{tool_name}' is MCP-served but no child VM context was available"
                            ),
                            category: ErrorCategory::ToolRejected,
                        }),
                        executor,
                    };
                };
                let args_vm = crate::stdlib::json_to_vm_value(tool_args);
                let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
                let outcome = vm.call_closure_pub(&handler, &[args_vm]).await;
                let captured = vm.take_output();
                if let Some(ctx) = ctx {
                    ctx.forward_output(&captured);
                }
                match outcome {
                    Ok(val) => Ok(serde_json::Value::String(val.display())),
                    Err(VmError::CategorizedError {
                        message,
                        category: ErrorCategory::ToolRejected,
                    }) => Ok(denied_tool_result(tool_name, message)),
                    Err(e) => Err(e),
                }
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
                    result: Err(VmError::CategorizedError {
                        message: format!(
                            "tool '{tool_name}' is Harn-owned but no child VM context was available"
                        ),
                        category: ErrorCategory::ToolRejected,
                    }),
                    executor,
                };
            };
            let args_vm = crate::stdlib::json_to_vm_value(tool_args);
            let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
            let outcome = vm.call_closure_pub(&handler, &[args_vm]).await;
            let captured = vm.take_output();
            if let Some(ctx) = ctx {
                ctx.forward_output(&captured);
            }
            match outcome {
                Ok(val) => Ok(serde_json::Value::String(val.display())),
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                Err(e) => Err(e),
            }
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
        match &result {
            Ok(_) => break ToolDispatchOutcome { result, executor },
            Err(VmError::CategorizedError {
                category: ErrorCategory::ToolRejected,
                ..
            }) => break ToolDispatchOutcome { result, executor },
            Err(_) if attempt < tool_retries => {
                attempt += 1;
                let delay = tool_backoff_ms * (1u64 << attempt.min(5));
                crate::clock_mock::sleep(tokio::time::Duration::from_millis(delay)).await;
            }
            Err(_) => break ToolDispatchOutcome { result, executor },
        }
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
mod tests {
    //! Harn#691: every dispatch path tags `ToolCallUpdate.executor` with
    //! the backend that ran the tool. These tests exercise each branch
    //! of `dispatch_tool_execution` without spinning up the full agent
    //! loop.

    use super::*;
    use crate::value::VmDictExt;

    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // D5: a permission denial must tell the model what to do instead, not just
    // report `{"error":"permission_denied"}`. The bare object made the model
    // retry the same blocked call; the `next_step` field gives it an out.
    #[test]
    fn denied_tool_result_includes_actionable_next_step() {
        let result = denied_tool_result("run", "shell access is disabled");
        assert_eq!(result["error"], serde_json::json!("permission_denied"));
        assert_eq!(result["tool"], serde_json::json!("run"));
        assert_eq!(
            result["reason"],
            serde_json::json!("shell access is disabled")
        );
        let next = result["next_step"]
            .as_str()
            .expect("denial should carry a next_step string");
        assert!(
            next.contains("Do not retry"),
            "next_step should steer the model off a retry loop: {next}"
        );
        assert!(
            next.contains("run"),
            "next_step should name the denied tool: {next}"
        );
        // No active policy → no allow-all list to assert, so the "Available
        // tools:" clause is omitted rather than claiming a misleading set.
        assert!(
            !next.contains("Available tools:"),
            "with no active policy the denial should not assert an allow list: {next}"
        );
    }

    // F3: when an execution policy advertises an explicit tool allowlist, the
    // denial must NAME those tools so a cheap model can self-correct in one turn
    // instead of re-emitting an unlisted name. Grounded in fw-gpt-oss-120b
    // transcripts where the model called Codex/container vocab it was never
    // shown and only saw a bare "not permitted" denial.
    #[test]
    fn denied_tool_result_names_available_tools_under_policy() {
        use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

        push_execution_policy(CapabilityPolicy {
            tools: vec![
                "look".to_string(),
                "search".to_string(),
                "edit".to_string(),
                "run".to_string(),
                "read_command_output".to_string(),
            ],
            ..Default::default()
        });
        let result = denied_tool_result("repo_browser.open_file", "tool exceeds tool ceiling");
        pop_execution_policy();

        let next = result["next_step"]
            .as_str()
            .expect("denial should carry a next_step string");
        assert!(
            next.contains("Available tools:"),
            "next_step should name the allowed tools under an active policy: {next}"
        );
        for tool in ["look", "search", "edit", "run", "read_command_output"] {
            assert!(
                next.contains(tool),
                "next_step should list the allowed tool {tool}: {next}"
            );
        }
    }

    // A RECOVERABLE schema/argument rejection must coach a retry WITH the
    // correction — `error: "invalid_arguments"` (not permission_denied) and a
    // next_step that re-calls the tool naming the missing param. Reverting the
    // recoverable/denied split (routing this through `denied_tool_result`) makes
    // every assertion below fail: the error flips to permission_denied and the
    // next_step says "Do not retry the same call".
    #[test]
    fn recoverable_tool_result_coaches_retry_with_named_missing_param() {
        let result = recoverable_tool_result(
            "edit",
            "Tool 'edit' is missing required parameter(s): path. \
             Provide all required parameters and try again.",
        );
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        assert_ne!(result["error"], serde_json::json!("permission_denied"));
        assert_eq!(result["tool"], serde_json::json!("edit"));
        let next = result["next_step"]
            .as_str()
            .expect("recoverable result should carry a next_step string");
        assert!(
            !next.contains("Do not retry"),
            "recoverable next_step must be retry-positive, not a don't-retry denial: {next}"
        );
        assert!(
            next.contains("Re-call") && next.contains("edit"),
            "next_step should tell the model to re-call the named tool: {next}"
        );
        assert!(
            next.contains("path"),
            "next_step should name the specific missing parameter: {next}"
        );
    }

    // The empty/malformed tool-name slip (harn#3194) is also recoverable: it
    // must get retry-positive feedback, never the permission-denial body.
    #[test]
    fn recoverable_tool_result_handles_empty_tool_name() {
        let result = recoverable_tool_result(
            "<unnamed>",
            "Tool call is missing a name. Emit one tool call per turn as \
             `name({ ... })` using a non-empty tool name from the allowed list, then retry.",
        );
        assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
        let next = result["next_step"]
            .as_str()
            .expect("recoverable result should carry a next_step string");
        assert!(
            !next.contains("Do not retry"),
            "empty-name feedback must be retry-positive: {next}"
        );
        assert!(
            next.contains("fixable") && next.contains("name"),
            "next_step should frame the missing name as a fixable slip: {next}"
        );
    }

    #[test]
    fn recoverable_tool_result_is_not_a_denial() {
        // The loop keys flow control off `is_denied_tool_result`; a fixable
        // argument error must NOT trip the denial detector.
        let result = recoverable_tool_result(
            "edit",
            "Tool 'edit' is missing required parameter(s): path. \
             Provide all required parameters and try again.",
        );
        assert!(
            !is_denied_tool_result(&result),
            "recoverable invalid_arguments result must not read as a denial"
        );
        assert!(!is_denied_tool_result(&serde_json::Value::String(
            result.to_string()
        )));
    }

    #[test]
    fn extract_missing_params_pulls_named_list() {
        assert_eq!(
            extract_missing_params(
                "Tool 'edit' is missing required parameter(s): path, mode. \
                 Provide all required parameters and try again."
            ),
            Some("path, mode".to_string())
        );
        assert_eq!(extract_missing_params("Tool call is missing a name."), None);
    }

    fn tools_dict(entries: Vec<(&str, crate::value::DictMap)>) -> VmValue {
        let list: Vec<VmValue> = entries
            .into_iter()
            .map(|(name, mut entry)| {
                entry
                    .entry(crate::value::intern_key("name"))
                    .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(name.to_string())));
                VmValue::dict(entry)
            })
            .collect();
        let mut dict = crate::value::DictMap::new();
        dict.insert(
            crate::value::intern_key("tools"),
            VmValue::List(std::sync::Arc::new(list)),
        );
        VmValue::dict(dict)
    }

    #[test]
    fn denied_tool_result_detects_rendered_blocked_json() {
        let blocked = serde_json::json!({
            "blocked": true,
            "status": "blocked",
            "reason": "policy rejected command"
        });
        assert!(is_denied_tool_result(&blocked));
        assert!(is_denied_tool_result(&serde_json::Value::String(
            blocked.to_string()
        )));
        assert!(!is_denied_tool_result(&serde_json::json!({
            "status": "completed",
            "stdout": "ok"
        })));
    }

    #[test]
    fn ok_result_failure_category_detects_failure_bodies() {
        // The pre-fix bug: these all returned Ok(value) from dispatch and were
        // laundered into `ok: true` because they aren't *denials*.
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"ok": false, "error": "boom"})),
            Some("tool_error")
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"success": false})),
            Some("tool_error")
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"status": "error", "stderr": "x"})),
            Some("tool_error")
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"status": "failed"})),
            Some("tool_error")
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"isError": true, "content": []})),
            Some("tool_error")
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"error": "disk full"})),
            Some("tool_error")
        );
        // Stringified envelope (host bridges that stringify) still detected.
        let stringified =
            serde_json::Value::String(r#"{"ok": false, "error": "boom"}"#.to_string());
        assert_eq!(ok_result_failure_category(&stringified), Some("tool_error"));
    }

    #[test]
    fn ok_result_failure_category_passes_through_successes() {
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"ok": true, "stdout": "done"})),
            None
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"status": "completed"})),
            None
        );
        // A null/empty error with positive signals is a success, not a failure.
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"ok": true, "error": null})),
            None
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!({"ok": true, "error": "  "})),
            None
        );
        // Plain string output and arrays are ordinary success.
        assert_eq!(
            ok_result_failure_category(&serde_json::Value::String("file contents".to_string())),
            None
        );
        assert_eq!(
            ok_result_failure_category(&serde_json::json!(["a", "b"])),
            None
        );
        assert_eq!(ok_result_failure_category(&serde_json::Value::Null), None);
    }

    #[test]
    fn mcp_server_for_tool_finds_top_level_annotation() {
        // mcp_list_tools tags every entry with `_mcp_server`. The
        // helper picks that up so the dispatch site can tag the
        // executor as `McpServer { server_name }`.
        let mut entry = crate::value::DictMap::new();
        entry.put_str("_mcp_server", "linear");
        let tools = tools_dict(vec![("create_issue", entry)]);
        assert_eq!(
            mcp_server_for_tool(Some(&tools), "create_issue"),
            Some("linear".to_string())
        );
    }

    #[test]
    fn mcp_server_for_tool_finds_nested_function_annotation() {
        // OpenAI-shape tools nest `_mcp_server` inside a `function`
        // sub-dict; the search must drill down a level.
        let mut function = crate::value::DictMap::new();
        function.put_str("name", "create_issue");
        function.put_str("_mcp_server", "linear");
        let mut entry = crate::value::DictMap::new();
        entry.insert(
            crate::value::intern_key("function"),
            VmValue::dict(function),
        );
        // The outer entry has no `name` — fall back to function.name.
        let mut dict = crate::value::DictMap::new();
        dict.insert(
            crate::value::intern_key("tools"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
                std::sync::Arc::new(entry),
            )])),
        );
        let tools = VmValue::dict(dict);
        assert_eq!(
            mcp_server_for_tool(Some(&tools), "create_issue"),
            Some("linear".to_string())
        );
    }

    #[test]
    fn mcp_server_for_tool_returns_none_for_plain_tool() {
        let tools = tools_dict(vec![("read", crate::value::DictMap::new())]);
        assert!(mcp_server_for_tool(Some(&tools), "read").is_none());
        assert!(mcp_server_for_tool(Some(&tools), "missing").is_none());
        assert!(mcp_server_for_tool(None, "read").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_tags_harn_builtin_for_local_short_circuit() {
        // `read_file` is a `handle_tool_locally` short-circuit — the
        // dispatcher resolves it without touching tools_val or the
        // bridge, and tags executor=HarnBuiltin.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "harn#691").expect("write");
        let args = serde_json::json!({ "path": path.to_string_lossy() });
        let outcome = dispatch_tool_execution("read_file", &args, None, None, 0, 0).await;
        assert!(outcome.result.is_ok(), "got: {:?}", outcome.result);
        assert_eq!(outcome.executor, Some(ToolExecutor::HarnBuiltin));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_tags_host_bridge_when_only_bridge_can_serve() {
        // No `handle_tool_locally` short-circuit, no script handler in
        // tools_val — the bridge is the only backend left, so the
        // executor must be `HostBridge`. Use a writer that errors so
        // the call fails fast without needing a real host process.
        let bridge = crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_| Err("test bridge: no host attached".to_string())),
            1,
        );
        let bridge = Arc::new(bridge);
        let args = serde_json::json!({});
        let outcome =
            dispatch_tool_execution("custom_host_tool", &args, None, Some(&bridge), 0, 0).await;
        // The call itself fails (no host responds) but the executor
        // reflects the path that was attempted.
        assert!(outcome.result.is_err());
        assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_tags_mcp_server_when_tool_is_mcp_owned_via_bridge() {
        // The bridge is present AND the tool entry carries a
        // `_mcp_server` annotation: the executor must point to the
        // MCP server, not the bridge that proxied the call.
        let bridge = crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_| Err("test bridge".to_string())),
            1,
        );
        let bridge = Arc::new(bridge);
        let mut entry = crate::value::DictMap::new();
        entry.put_str("_mcp_server", "linear");
        let tools = tools_dict(vec![("create_issue", entry)]);
        let args = serde_json::json!({});
        let outcome =
            dispatch_tool_execution("create_issue", &args, Some(&tools), Some(&bridge), 0, 0).await;
        assert_eq!(
            outcome.executor,
            Some(ToolExecutor::McpServer {
                server_name: "linear".to_string()
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_returns_none_executor_when_no_backend_available() {
        // No local short-circuit, no script handler, no bridge — the
        // dispatcher reports the tool as unavailable and the executor
        // stays `None` so callers don't blame a specific backend.
        let outcome =
            dispatch_tool_execution("nonexistent_tool", &serde_json::json!({}), None, None, 0, 0)
                .await;
        assert!(outcome.result.is_err());
        assert!(outcome.executor.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_honors_declared_host_bridge_executor() {
        // harn#743: when a tool declares `executor: "host_bridge"`, the
        // dispatcher tags the event as HostBridge regardless of the
        // historic handler/`_mcp_server` heuristic.
        let bridge = crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_| Err("test bridge".to_string())),
            1,
        );
        let bridge = Arc::new(bridge);
        let mut entry = crate::value::DictMap::new();
        entry.put_str("executor", "host_bridge");
        entry.put_str("host_capability", "interaction.ask");
        let tools = tools_dict(vec![("ask_user", entry)]);
        let outcome = dispatch_tool_execution(
            "ask_user",
            &serde_json::json!({"prompt": "x"}),
            Some(&tools),
            Some(&bridge),
            0,
            0,
        )
        .await;
        assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_honors_declared_provider_native_executor() {
        // Provider-native tools must never reach a runtime backend.
        // The dispatcher rejects with ProviderNative as the executor so
        // the ACP event reflects "model already executed this".
        let mut entry = crate::value::DictMap::new();
        entry.put_str("executor", "provider_native");
        let tools = tools_dict(vec![("tool_search", entry)]);
        let outcome = dispatch_tool_execution(
            "tool_search",
            &serde_json::json!({}),
            Some(&tools),
            None,
            0,
            0,
        )
        .await;
        assert_eq!(outcome.executor, Some(ToolExecutor::ProviderNative));
        assert!(outcome.result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_honors_declared_mcp_server_executor() {
        // Declared mcp_server uses the configured server name, not the
        // implicit `_mcp_server` annotation.
        let bridge = crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_| Err("test bridge".to_string())),
            1,
        );
        let bridge = Arc::new(bridge);
        let mut entry = crate::value::DictMap::new();
        entry.put_str("executor", "mcp_server");
        entry.put_str("mcp_server", "github");
        let tools = tools_dict(vec![("github_search_issues", entry)]);
        let outcome = dispatch_tool_execution(
            "github_search_issues",
            &serde_json::json!({"query": "x"}),
            Some(&tools),
            Some(&bridge),
            0,
            0,
        )
        .await;
        assert_eq!(
            outcome.executor,
            Some(ToolExecutor::McpServer {
                server_name: "github".to_string()
            })
        );
    }
}
