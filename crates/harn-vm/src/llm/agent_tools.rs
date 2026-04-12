//! Tool dispatch, loop detection, and tool-format normalization helpers
//! extracted from `agent.rs` for maintainability.

use std::collections::HashMap;
use std::rc::Rc;

use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};

// ---------------------------------------------------------------------------
// Tool loop detection
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tool format normalization
// ---------------------------------------------------------------------------

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
    if tool_format == "text" {
        tool_examples.and_then(|examples| {
            let trimmed = examples.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    } else {
        None
    }
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

pub(super) fn native_protocol_violation_nudge(
    tool_format: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
    saw_text_tool_calls: bool,
) -> String {
    let mut message = if saw_text_tool_calls {
        "This transcript is native-tool-only. Your previous response used handwritten tool-call text, which was not executed. Call an available tool through the provider tool channel instead of writing tool syntax in the assistant message.".to_string()
    } else {
        "This transcript is native-tool-only. Call an available tool through the provider tool channel now instead of replying with prose or bare code.".to_string()
    };
    if let Some(nudge) = super::agent::action_turn_nudge(tool_format, turn_policy, false) {
        message.push(' ');
        message.push_str(&nudge);
    }
    message
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Tools that have no side effects and can be safely dispatched concurrently.
pub(super) fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "read_file"
            | "lookup"
            | "search"
            | "outline"
            | "list_directory"
            | "list_templates"
            | "get_template"
            | "web_search"
            | "web_fetch"
    )
}

/// Dispatch a single tool invocation to its execution backend.
pub(super) async fn dispatch_tool_execution(
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> Result<serde_json::Value, VmError> {
    use super::tools::handle_tool_locally;

    let mut attempt = 0usize;
    loop {
        let result = if let Some(local_result) = handle_tool_locally(tool_name, tool_args) {
            Ok(serde_json::Value::String(local_result))
        } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
            let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
                return Err(VmError::CategorizedError {
                    message: format!(
                        "tool '{tool_name}' is Harn-owned but no child VM context was available"
                    ),
                    category: ErrorCategory::ToolRejected,
                });
            };
            let args_vm = crate::stdlib::json_to_vm_value(tool_args);
            match vm.call_closure_pub(&handler, &[args_vm], &[]).await {
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
                    "Tool '{}' is not available in the current environment. \
                     Use only the tools listed in the tool-calling contract.",
                    tool_name
                ),
                category: ErrorCategory::ToolRejected,
            })
        };
        match &result {
            Ok(_) => break result,
            Err(VmError::CategorizedError {
                category: ErrorCategory::ToolRejected,
                ..
            }) => break result,
            Err(_) if attempt < tool_retries => {
                attempt += 1;
                let delay = tool_backoff_ms * (1u64 << attempt.min(5));
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }
            Err(_) => break result,
        }
    }
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

// ---------------------------------------------------------------------------
// Orchestration helpers
// ---------------------------------------------------------------------------

pub(super) fn classify_tool_mutation(tool_name: &str) -> String {
    crate::orchestration::current_tool_mutation_classification(tool_name)
}

pub(super) fn declared_paths(tool_name: &str, tool_args: &serde_json::Value) -> Vec<String> {
    crate::orchestration::current_tool_declared_paths(tool_name, tool_args)
}
