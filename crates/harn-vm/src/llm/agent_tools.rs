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
pub(super) async fn dispatch_tool_execution(
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> ToolDispatchOutcome {
    use super::tools::handle_tool_locally;

    let mut attempt = 0usize;
    let mut executor: Option<ToolExecutor> = None;
    // harn#743: when the registry declares an executor for this tool
    // (`tool_define(..., {executor: "..."})`), pin the transcript tag
    // to the declared value. Falls back to dispatch-time inference for
    // VM-stdlib tools and registries that pre-date the declaration.
    let declared_executor = declared_executor_for_tool(tools_val, tool_name);
    loop {
        let result =
            if let Some(local_result) = handle_tool_locally(tool_name, tool_args) {
                // VM-stdlib short-circuit (read_file / list_directory). Any
                // other tool falls through to the script-handler / bridge
                // path below.
                executor = Some(
                    declared_executor
                        .clone()
                        .unwrap_or(ToolExecutor::HarnBuiltin),
                );
                Ok(serde_json::Value::String(local_result))
            } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
                // A Harn-side handler closure exists — but if the tool was
                // sourced from `mcp_list_tools`, the dict carries the
                // originating server name as `_mcp_server`, and the call is
                // semantically "served by MCP" even though dispatch goes
                // through a Harn closure that ultimately invokes mcp_call.
                executor = Some(declared_executor.clone().unwrap_or_else(|| {
                    match mcp_server_for_tool(tools_val, tool_name) {
                        Some(server_name) => ToolExecutor::McpServer { server_name },
                        None => ToolExecutor::HarnBuiltin,
                    }
                }));
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
                executor = Some(declared_executor.clone().unwrap_or_else(|| {
                    match mcp_server_for_tool(tools_val, tool_name) {
                        Some(server_name) => ToolExecutor::McpServer { server_name },
                        None => ToolExecutor::HostBridge,
                    }
                }));
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

/// Map a tool entry's declared executor to the [`ToolExecutor`] tag.
/// `executor: "mcp_server"` honors the entry's own `mcp_server` field,
/// falling back to a `_mcp_server` tag (e.g. from `mcp_list_tools`)
/// when the author defined the tool through `tool_define` but only
/// named the server elsewhere — keeps the wire-level executor consistent
/// with how MCP-discovered tools are tagged today (harn#743).
fn executor_from_entry(
    entry: &std::collections::BTreeMap<String, VmValue>,
) -> Option<ToolExecutor> {
    use crate::stdlib::tools::{
        EXECUTOR_HARN, EXECUTOR_HOST_BRIDGE, EXECUTOR_MCP_SERVER, EXECUTOR_PROVIDER_NATIVE,
    };
    let kind = match entry.get("executor") {
        Some(VmValue::String(s)) => s.to_string(),
        _ => return None,
    };
    match kind.as_str() {
        EXECUTOR_HARN => Some(ToolExecutor::HarnBuiltin),
        EXECUTOR_HOST_BRIDGE => Some(ToolExecutor::HostBridge),
        EXECUTOR_MCP_SERVER => {
            let server_name = match entry.get("mcp_server") {
                Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
                _ => match entry.get("_mcp_server") {
                    Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
                    _ => return None,
                },
            };
            Some(ToolExecutor::McpServer { server_name })
        }
        EXECUTOR_PROVIDER_NATIVE => Some(ToolExecutor::ProviderNative),
        _ => None,
    }
}

/// Read the declared executor for a tool from `tools_val`. The
/// declaration wins over dispatch-time inference so the ACP transcript
/// reflects the source-of-truth executor identity (harn#743). Falls
/// back to the `_mcp_server` tag for MCP-discovered tools that never
/// went through `tool_define`.
pub(super) fn declared_executor_for_tool(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<ToolExecutor> {
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
        if let Some(executor) = executor_from_entry(entry) {
            return Some(executor);
        }
        // Legacy / MCP-discovered entries with no `executor` field but
        // a `_mcp_server` tag — match what `mcp_server_for_tool`
        // returns to keep the executor tag consistent.
        if let Some(VmValue::String(s)) = entry.get("_mcp_server") {
            if !s.is_empty() {
                return Some(ToolExecutor::McpServer {
                    server_name: s.to_string(),
                });
            }
        }
        return None;
    }
    None
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
    async fn dispatch_honors_declared_host_bridge_executor_over_mcp_tag() {
        // harn#743: when an entry declares `executor: "host_bridge"`
        // the transcript must reflect the declaration even if the
        // entry also carries an `_mcp_server` tag (e.g. a host-shimmed
        // tool that proxies a fake server name). The declaration is
        // the source of truth.
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
            VmValue::String(Rc::from("host_bridge".to_string())),
        );
        entry.insert(
            "host_capability".to_string(),
            VmValue::String(Rc::from("interaction.ask".to_string())),
        );
        // Decoy `_mcp_server`: would normally tag the executor as
        // McpServer, but the declared `host_bridge` wins.
        entry.insert(
            "_mcp_server".to_string(),
            VmValue::String(Rc::from("linear".to_string())),
        );
        let tools = tools_dict(vec![("ask_user", entry)]);
        let args = serde_json::json!({});
        let outcome =
            dispatch_tool_execution("ask_user", &args, Some(&tools), Some(&bridge), 0, 0).await;
        assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_honors_declared_mcp_server_executor() {
        // harn#743: `executor: "mcp_server"` with an explicit
        // `mcp_server` field maps straight to McpServer { server_name }
        // in the transcript.
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
            VmValue::String(Rc::from("mcp_server".to_string())),
        );
        entry.insert(
            "mcp_server".to_string(),
            VmValue::String(Rc::from("github".to_string())),
        );
        let tools = tools_dict(vec![("create_pr", entry)]);
        let args = serde_json::json!({});
        let outcome =
            dispatch_tool_execution("create_pr", &args, Some(&tools), Some(&bridge), 0, 0).await;
        assert_eq!(
            outcome.executor,
            Some(ToolExecutor::McpServer {
                server_name: "github".to_string()
            })
        );
    }
}
