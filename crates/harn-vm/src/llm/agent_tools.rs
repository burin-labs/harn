//! Tool dispatch, loop detection, and tool-format normalization helpers
//! extracted from `agent.rs` for maintainability.

use std::collections::HashMap;
use std::rc::Rc;

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

pub(super) fn stable_hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason.into(),
    })
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
    value
        .get("error")
        .and_then(|error| error.as_str())
        .is_some_and(|error| error == "permission_denied")
}

pub(crate) fn merge_agent_loop_policy(
    requested: Option<crate::orchestration::CapabilityPolicy>,
) -> Result<Option<crate::orchestration::CapabilityPolicy>, VmError> {
    match (crate::orchestration::current_execution_policy(), requested) {
        (Some(current), Some(requested)) => current
            .intersect(&requested)
            .map(Some)
            .map_err(VmError::Runtime),
        (None, Some(requested)) => Ok(Some(requested)),
        (_, None) => Ok(None),
    }
}

/// Intersect any active ambient approval policy with a loop-requested one.
/// Approval intersection is always safe (strictly more restrictive), so no error path.
pub(crate) fn merge_agent_loop_approval_policy(
    requested: Option<crate::orchestration::ToolApprovalPolicy>,
) -> Option<crate::orchestration::ToolApprovalPolicy> {
    match (crate::orchestration::current_approval_policy(), requested) {
        (Some(current), Some(requested)) => Some(current.intersect(&requested)),
        (Some(current), None) => Some(current),
        (None, Some(requested)) => Some(requested),
        (None, None) => None,
    }
}

pub(super) struct ToolCallTracker {
    /// (tool_name, args_hash) -> (consecutive_count, last_result_hash)
    entries: HashMap<(String, u64), (usize, u64)>,
    /// Thresholds for intervention tiers
    warn_threshold: usize,
    block_threshold: usize,
    skip_threshold: usize,
}

/// What the tracker recommends for a given tool call.
pub(super) enum LoopIntervention {
    /// No loop detected — proceed normally.
    Proceed,
    /// Warn: append a redirection hint after the tool result.
    Warn { count: usize },
    /// Block: replace the result with a hard redirect, still execute to track.
    Block { count: usize },
    /// Skip: do not execute, inject a skip message.
    Skip { count: usize },
}

impl ToolCallTracker {
    pub(super) fn new(warn: usize, block: usize, skip: usize) -> Self {
        Self {
            entries: HashMap::new(),
            warn_threshold: warn,
            block_threshold: block,
            skip_threshold: skip,
        }
    }

    /// Check if a tool call is a repeated loop.  Call BEFORE execution.
    pub(super) fn check(&self, tool_name: &str, args_hash: u64) -> LoopIntervention {
        let key = (tool_name.to_string(), args_hash);
        if let Some(&(count, _result_hash)) = self.entries.get(&key) {
            let next = count + 1;
            if next >= self.skip_threshold {
                return LoopIntervention::Skip { count: next };
            }
            if next >= self.block_threshold {
                return LoopIntervention::Block { count: next };
            }
            if next >= self.warn_threshold {
                return LoopIntervention::Warn { count: next };
            }
        }
        LoopIntervention::Proceed
    }

    /// Record a tool call result.  Returns the intervention to apply
    /// based on whether the result is new or identical to the last one.
    pub(super) fn record(
        &mut self,
        tool_name: &str,
        args_hash: u64,
        result_hash: u64,
    ) -> LoopIntervention {
        let key = (tool_name.to_string(), args_hash);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.1 == result_hash {
                entry.0 += 1;
                let count = entry.0;
                if count >= self.skip_threshold {
                    return LoopIntervention::Skip { count };
                }
                if count >= self.block_threshold {
                    return LoopIntervention::Block { count };
                }
                if count >= self.warn_threshold {
                    return LoopIntervention::Warn { count };
                }
            } else {
                entry.0 = 1;
                entry.1 = result_hash;
            }
        } else {
            self.entries.insert(key, (1, result_hash));
        }
        LoopIntervention::Proceed
    }
}

pub(super) fn loop_intervention_message(
    tool_name: &str,
    result_text: &str,
    intervention: &LoopIntervention,
) -> Option<String> {
    match intervention {
        LoopIntervention::Proceed => None,
        LoopIntervention::Warn { count, .. } => {
            let first_line = result_text.lines().next().unwrap_or("(empty)");
            Some(format!(
                "\n[LOOP DETECTED] This exact {tool_name}() call has produced the same result {count} times. \
                 The result says: \"{first_line}\". \
                 Try a DIFFERENT tool or DIFFERENT parameters."
            ))
        }
        LoopIntervention::Block { count } => Some(format!(
            "BLOCKED: {tool_name}() has failed {count} times identically. \
                 You MUST use a different approach. \
                 Pick a different available tool or change your parameters."
        )),
        LoopIntervention::Skip { count } => Some(format!(
            "BLOCKED: {tool_name}() was NOT executed (repeated {count} times identically). \
                 This call will not be executed again with these arguments. \
                 You MUST change your approach NOW."
        )),
    }
}

pub(super) fn next_call_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub(super) fn normalize_native_tools_for_format(
    tool_format: &str,
    native_tools: Option<Vec<serde_json::Value>>,
) -> Option<Vec<serde_json::Value>> {
    if tool_format == "native" {
        native_tools
    } else {
        None
    }
}

pub(super) fn normalize_tool_examples_for_format(
    tool_format: &str,
    tool_examples: Option<String>,
) -> Option<String> {
    if tool_format == "native" {
        return None;
    }
    tool_examples.and_then(|examples| {
        let trimmed = examples.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(super) fn required_tool_choice_for_provider(provider: &str) -> serde_json::Value {
    if provider == "anthropic" {
        serde_json::json!({"type": "any"})
    } else {
        serde_json::json!("required")
    }
}

pub(super) fn normalize_tool_choice_for_format(
    provider: &str,
    tool_format: &str,
    native_tools: Option<&[serde_json::Value]>,
    tool_choice: Option<serde_json::Value>,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> Option<serde_json::Value> {
    if tool_format != "native" {
        return None;
    }
    if native_tools.is_none_or(|tools| tools.is_empty()) {
        return None;
    }
    if let Some(choice) = tool_choice {
        return Some(choice);
    }
    if turn_policy.is_some_and(|policy| policy.require_action_or_yield) {
        return Some(required_tool_choice_for_provider(provider));
    }
    None
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
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> ToolDispatchOutcome {
    dispatch_tool_execution_with_mcp(
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
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    mcp_clients: Option<&std::collections::BTreeMap<String, crate::mcp::VmMcpClientHandle>>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
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
                let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
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
                match vm.call_closure_pub(&handler, &[args_vm]).await {
                    Ok(val) => Ok(serde_json::Value::String(val.display())),
                    Err(VmError::CategorizedError {
                        message,
                        category: ErrorCategory::ToolRejected,
                    }) => Ok(denied_tool_result(tool_name, message)),
                    Err(e) => Ok(serde_json::Value::String(format!("Error: {e}"))),
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
        } else if let Some(local_result) = handle_tool_locally(tool_name, tool_args) {
            // VM-stdlib short-circuit (read_file / list_directory). Any
            // other tool falls through to the script-handler / bridge
            // path below.
            executor = Some(ToolExecutor::HarnBuiltin);
            Ok(serde_json::Value::String(local_result))
        } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
            // A Harn-side handler closure exists — but if the tool was
            // sourced from `mcp_list_tools`, the dict carries the
            // originating server name as `_mcp_server`, and the call is
            // semantically "served by MCP" even though dispatch goes
            // through a Harn closure that ultimately invokes mcp_call.
            executor = Some(match mcp_server_for_tool(tools_val, tool_name) {
                Some(server_name) => ToolExecutor::McpServer { server_name },
                None => ToolExecutor::HarnBuiltin,
            });
            let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
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
            match vm.call_closure_pub(&handler, &[args_vm]).await {
                Ok(val) => Ok(serde_json::Value::String(val.display())),
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                Err(e) => Ok(serde_json::Value::String(format!("Error: {e}"))),
            }
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
                    "Tool '{}' is not available in the current environment. \
                     Use only the tools listed in the tool-calling contract.",
                    tool_name
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
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
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
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
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
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
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
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
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
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
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
) -> Option<Rc<VmClosure>> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = match entry.get("name") {
            Some(v) => v.display(),
            None => continue,
        };
        if name == tool_name {
            if let Some(VmValue::Closure(c)) = entry.get("handler") {
                return Some(Rc::clone(c));
            }
            return None;
        }
    }
    None
}

/// Refuse to start an agent loop whose tool registry contains entries
/// with no executable backend (harn#743). The validator runs once at
/// loop entry so a misconfigured registry surfaces here, with a tool
/// name and the documented set of executors, instead of failing later
/// at the first model call with an unhelpful `[builtin_call]
/// unhandled: <name>` error.
///
/// A tool entry is considered backed when any one of:
/// - it has a callable `handler` (a `Closure` value) — the canonical
///   `executor: "harn"` shape;
/// - its `executor` field is set to one of the documented backends
///   (`"host_bridge"`, `"mcp_server"`, `"provider_native"`, or the
///   `"harn"`/`"harn_builtin"` aliases — handler-required cases are
///   already caught by the harn-executor branch above);
/// - it carries the `_mcp_server` annotation that `mcp_list_tools`
///   injects on every tool dict, signalling MCP-served dispatch even
///   when the user pushed the raw entry into the registry without
///   going through `tool_define`.
///
/// Synthetic tools surfaced via `opts.native_tools` (e.g. the OpenAI
/// `__harn_tool_search` meta-tool) live outside `tools_val` and are
/// not considered here — those are dispatched through provider-native
/// paths regardless of the user's registry.
pub(crate) fn validate_tool_registry_executors(tools_val: Option<&VmValue>) -> Result<(), VmError> {
    let Some(dict) = tools_val.and_then(|v| v.as_dict()) else {
        return Ok(());
    };
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return Ok(()),
    };
    for tool in tools_list.iter() {
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        if matches!(entry.get("handler"), Some(VmValue::Closure(_))) {
            continue;
        }
        let executor = entry.get("executor").and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        });
        if let Some(executor) = executor.as_deref() {
            match executor {
                "host_bridge" | "mcp_server" | "provider_native" => continue,
                // `harn`/`harn_builtin` reaches here only when the
                // handler closure was lost between `tool_define` and
                // `agent_loop` — `tool_define` rejects this combo at
                // definition time, but the runtime still has to guard
                // against tools mutated/copied after registration.
                // The exception is the VM-stdlib short-circuit set
                // (`read_file`, `list_directory`): `handle_tool_locally`
                // provides the implicit Harn-side handler, so the entry
                // is dispatchable even without a registered closure.
                "harn" | "harn_builtin" if super::tools::is_vm_stdlib_short_circuit(&name) => {
                    continue
                }
                "harn" | "harn_builtin" => {
                    return Err(VmError::Runtime(format!(
                        "agent_loop: tool '{name}' declares executor: \"{executor}\" \
                         but has no callable `handler`. Reattach the handler fn or \
                         change the executor to a backend that does not require one."
                    )));
                }
                other => {
                    return Err(VmError::Runtime(format!(
                        "agent_loop: tool '{name}' declares unknown executor \"{other}\". \
                         Expected one of: \"harn\", \"host_bridge\", \"mcp_server\", \
                         \"provider_native\"."
                    )));
                }
            }
        }
        if matches!(entry.get("_mcp_server"), Some(VmValue::String(_))) {
            continue;
        }
        if let Some(VmValue::Dict(func)) = entry.get("function") {
            if matches!(func.get("_mcp_server"), Some(VmValue::String(_))) {
                continue;
            }
        }
        return Err(VmError::Runtime(format!(
            "agent_loop: tool '{name}' has no executable backend — no `handler` \
             closure, no declared `executor`, and no `_mcp_server` annotation. \
             Either attach a handler fn (executor: \"harn\"), declare an \
             alternate backend (executor: \"host_bridge\" + host_capability, \
             executor: \"mcp_server\" + mcp_server, or executor: \"provider_native\"), \
             or remove the tool from the registry. Starting the loop with this \
             tool would surface as `[builtin_call] unhandled: {name}` at the \
             first model call."
        )));
    }
    Ok(())
}

pub(super) fn classify_tool_mutation(tool_name: &str) -> String {
    crate::orchestration::current_tool_mutation_classification(tool_name)
}

pub(super) fn declared_paths(tool_name: &str, tool_args: &serde_json::Value) -> Vec<String> {
    crate::orchestration::current_tool_declared_paths(tool_name, tool_args)
}

#[cfg(test)]
mod tests {
    //! Harn#691: every dispatch path tags `ToolCallUpdate.executor` with
    //! the backend that ran the tool. These tests exercise each branch
    //! of `dispatch_tool_execution` without spinning up the full agent
    //! loop.

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn tools_dict(entries: Vec<(&str, BTreeMap<String, VmValue>)>) -> VmValue {
        let list: Vec<VmValue> = entries
            .into_iter()
            .map(|(name, mut entry)| {
                entry
                    .entry("name".to_string())
                    .or_insert_with(|| VmValue::String(Rc::from(name.to_string())));
                VmValue::Dict(Rc::new(entry))
            })
            .collect();
        let mut dict = BTreeMap::new();
        dict.insert("tools".to_string(), VmValue::List(Rc::new(list)));
        VmValue::Dict(Rc::new(dict))
    }

    #[test]
    fn mcp_server_for_tool_finds_top_level_annotation() {
        // mcp_list_tools tags every entry with `_mcp_server`. The
        // helper picks that up so the dispatch site can tag the
        // executor as `McpServer { server_name }`.
        let mut entry = BTreeMap::new();
        entry.insert(
            "_mcp_server".to_string(),
            VmValue::String(Rc::from("linear".to_string())),
        );
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
        let mut function = BTreeMap::new();
        function.insert(
            "name".to_string(),
            VmValue::String(Rc::from("create_issue".to_string())),
        );
        function.insert(
            "_mcp_server".to_string(),
            VmValue::String(Rc::from("linear".to_string())),
        );
        let mut entry = BTreeMap::new();
        entry.insert("function".to_string(), VmValue::Dict(Rc::new(function)));
        // The outer entry has no `name` — fall back to function.name.
        let mut dict = BTreeMap::new();
        dict.insert(
            "tools".to_string(),
            VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(entry))])),
        );
        let tools = VmValue::Dict(Rc::new(dict));
        assert_eq!(
            mcp_server_for_tool(Some(&tools), "create_issue"),
            Some("linear".to_string())
        );
    }

    #[test]
    fn mcp_server_for_tool_returns_none_for_plain_tool() {
        let tools = tools_dict(vec![("read", BTreeMap::new())]);
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
        let bridge = Rc::new(bridge);
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
        let bridge = Rc::new(bridge);
        let mut entry = BTreeMap::new();
        entry.insert(
            "_mcp_server".to_string(),
            VmValue::String(Rc::from("linear".to_string())),
        );
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
        let bridge = Rc::new(bridge);
        let mut entry = BTreeMap::new();
        entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from("host_bridge")),
        );
        entry.insert(
            "host_capability".to_string(),
            VmValue::String(Rc::from("interaction.ask")),
        );
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
        let mut entry = BTreeMap::new();
        entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from("provider_native")),
        );
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
        let bridge = Rc::new(bridge);
        let mut entry = BTreeMap::new();
        entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from("mcp_server")),
        );
        entry.insert(
            "mcp_server".to_string(),
            VmValue::String(Rc::from("github")),
        );
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

    #[test]
    fn validate_tool_registry_rejects_handlerless_undeclared_tool() {
        // harn#743 pre-flight: no handler, no executor, no
        // `_mcp_server` annotation — the loop refuses to start.
        let mut entry = BTreeMap::new();
        entry.insert(
            "name".to_string(),
            VmValue::String(Rc::from("ask_user".to_string())),
        );
        let tools = tools_dict(vec![("ask_user", entry)]);
        let err = validate_tool_registry_executors(Some(&tools)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("ask_user"),
            "error must name the offending tool: {message}"
        );
        assert!(
            message.contains("no executable backend"),
            "error must mention missing backend: {message}"
        );
    }

    #[test]
    fn validate_tool_registry_accepts_declared_host_bridge_tool() {
        // A host_bridge declaration with no handler is valid — the
        // bridge serves the call.
        let mut entry = BTreeMap::new();
        entry.insert(
            "executor".to_string(),
            VmValue::String(Rc::from("host_bridge")),
        );
        let tools = tools_dict(vec![("ask_user", entry)]);
        validate_tool_registry_executors(Some(&tools))
            .expect("host_bridge declaration must satisfy pre-flight");
    }

    #[test]
    fn validate_tool_registry_accepts_vm_stdlib_short_circuit_without_handler() {
        // `read_file` and `list_directory` are served by
        // `handle_tool_locally` — they don't need a registered handler
        // to be dispatchable.
        let mut entry = BTreeMap::new();
        entry.insert("executor".to_string(), VmValue::String(Rc::from("harn")));
        let tools = tools_dict(vec![("read_file", entry)]);
        validate_tool_registry_executors(Some(&tools))
            .expect("VM-stdlib short-circuit must satisfy pre-flight without a handler");
    }

    #[test]
    fn validate_tool_registry_accepts_mcp_list_tools_entry() {
        // Tools pushed in straight from `mcp_list_tools` carry the
        // `_mcp_server` annotation; the validator treats that as a
        // sufficient backend declaration.
        let mut entry = BTreeMap::new();
        entry.insert(
            "_mcp_server".to_string(),
            VmValue::String(Rc::from("github")),
        );
        let tools = tools_dict(vec![("github_search", entry)]);
        validate_tool_registry_executors(Some(&tools))
            .expect("_mcp_server-tagged entry must satisfy pre-flight");
    }
}
