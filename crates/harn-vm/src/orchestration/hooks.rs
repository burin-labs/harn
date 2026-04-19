//! Runtime lifecycle hooks — tool, agent-turn, and worker interception.

use std::cell::RefCell;
use std::rc::Rc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::agent_events::WorkerEvent;
use crate::value::{VmClosure, VmError, VmValue};

/// Manifest / runtime hook event names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    #[serde(rename = "PreToolUse")]
    PreToolUse,
    #[serde(rename = "PostToolUse")]
    PostToolUse,
    #[serde(rename = "PreAgentTurn")]
    PreAgentTurn,
    #[serde(rename = "PostAgentTurn")]
    PostAgentTurn,
    #[serde(rename = "WorkerSpawned")]
    WorkerSpawned,
    #[serde(rename = "WorkerCompleted")]
    WorkerCompleted,
    #[serde(rename = "WorkerFailed")]
    WorkerFailed,
    #[serde(rename = "WorkerCancelled")]
    WorkerCancelled,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PreAgentTurn => "PreAgentTurn",
            Self::PostAgentTurn => "PostAgentTurn",
            Self::WorkerSpawned => "WorkerSpawned",
            Self::WorkerCompleted => "WorkerCompleted",
            Self::WorkerFailed => "WorkerFailed",
            Self::WorkerCancelled => "WorkerCancelled",
        }
    }

    pub fn from_worker_event(event: WorkerEvent) -> Self {
        match event {
            WorkerEvent::WorkerSpawned => Self::WorkerSpawned,
            WorkerEvent::WorkerCompleted => Self::WorkerCompleted,
            WorkerEvent::WorkerFailed => Self::WorkerFailed,
            WorkerEvent::WorkerCancelled => Self::WorkerCancelled,
        }
    }
}

/// Action returned by a PreToolUse hook.
#[derive(Clone, Debug)]
pub enum PreToolAction {
    /// Allow the tool call to proceed unchanged.
    Allow,
    /// Deny the tool call with an explanation.
    Deny(String),
    /// Allow but replace the arguments.
    Modify(serde_json::Value),
}

/// Action returned by a PostToolUse hook.
#[derive(Clone, Debug)]
pub enum PostToolAction {
    /// Pass the result through unchanged.
    Pass,
    /// Replace the result text.
    Modify(String),
}

/// Callback types for legacy tool lifecycle hooks.
pub type PreToolHookFn = Rc<dyn Fn(&str, &serde_json::Value) -> PreToolAction>;
pub type PostToolHookFn = Rc<dyn Fn(&str, &str) -> PostToolAction>;

/// A registered tool hook with a name pattern and callbacks.
#[derive(Clone)]
pub struct ToolHook {
    /// Glob-style pattern matched against tool names (e.g. `"*"`, `"exec*"`, `"read_file"`).
    pub pattern: String,
    /// Called before tool execution. Return `Deny` to reject, `Modify` to rewrite args.
    pub pre: Option<PreToolHookFn>,
    /// Called after tool execution with the result text. Return `Modify` to rewrite.
    pub post: Option<PostToolHookFn>,
}

impl std::fmt::Debug for ToolHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolHook")
            .field("pattern", &self.pattern)
            .field("has_pre", &self.pre.is_some())
            .field("has_post", &self.post.is_some())
            .finish()
    }
}

#[derive(Clone)]
enum PatternMatcher {
    ToolNameGlob(String),
    EventExpression(String),
}

impl std::fmt::Debug for PatternMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolNameGlob(pattern) => f.debug_tuple("ToolNameGlob").field(pattern).finish(),
            Self::EventExpression(pattern) => {
                f.debug_tuple("EventExpression").field(pattern).finish()
            }
        }
    }
}

#[derive(Clone)]
enum RuntimeHookHandler {
    NativePreTool(PreToolHookFn),
    NativePostTool(PostToolHookFn),
    Vm {
        handler_name: String,
        closure: Rc<VmClosure>,
    },
}

impl std::fmt::Debug for RuntimeHookHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativePreTool(_) => f.write_str("NativePreTool(..)"),
            Self::NativePostTool(_) => f.write_str("NativePostTool(..)"),
            Self::Vm { handler_name, .. } => f
                .debug_struct("Vm")
                .field("handler_name", handler_name)
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
struct RuntimeHook {
    event: HookEvent,
    matcher: PatternMatcher,
    handler: RuntimeHookHandler,
}

thread_local! {
    static RUNTIME_HOOKS: RefCell<Vec<RuntimeHook>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    pattern == name
}

pub fn register_tool_hook(hook: ToolHook) {
    if let Some(pre) = hook.pre {
        RUNTIME_HOOKS.with(|hooks| {
            hooks.borrow_mut().push(RuntimeHook {
                event: HookEvent::PreToolUse,
                matcher: PatternMatcher::ToolNameGlob(hook.pattern.clone()),
                handler: RuntimeHookHandler::NativePreTool(pre),
            });
        });
    }
    if let Some(post) = hook.post {
        RUNTIME_HOOKS.with(|hooks| {
            hooks.borrow_mut().push(RuntimeHook {
                event: HookEvent::PostToolUse,
                matcher: PatternMatcher::ToolNameGlob(hook.pattern),
                handler: RuntimeHookHandler::NativePostTool(post),
            });
        });
    }
}

pub fn register_vm_hook(
    event: HookEvent,
    pattern: impl Into<String>,
    handler_name: impl Into<String>,
    closure: Rc<VmClosure>,
) {
    RUNTIME_HOOKS.with(|hooks| {
        hooks.borrow_mut().push(RuntimeHook {
            event,
            matcher: PatternMatcher::EventExpression(pattern.into()),
            handler: RuntimeHookHandler::Vm {
                handler_name: handler_name.into(),
                closure,
            },
        });
    });
}

pub fn clear_tool_hooks() {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow_mut()
            .retain(|hook| !matches!(hook.event, HookEvent::PreToolUse | HookEvent::PostToolUse));
    });
}

pub fn clear_runtime_hooks() {
    RUNTIME_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

fn value_at_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        let serde_json::Value::Object(map) = current else {
            return None;
        };
        current = map.get(segment)?;
    }
    Some(current)
}

fn value_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(|number| number != 0)
            .or_else(|| value.as_u64().map(|number| number != 0))
            .or_else(|| value.as_f64().map(|number| number != 0.0))
            .unwrap_or(false),
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(values) => !values.is_empty(),
        serde_json::Value::Object(values) => !values.is_empty(),
    }
}

fn value_to_pattern_string(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn strip_quoted(value: &str) -> &str {
    value
        .trim()
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .or_else(|| {
            value
                .trim()
                .strip_prefix('\'')
                .and_then(|text| text.strip_suffix('\''))
        })
        .unwrap_or(value.trim())
}

fn expression_matches(pattern: &str, payload: &serde_json::Value) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if let Some((lhs, rhs)) = pattern.split_once("=~") {
        let value = value_to_pattern_string(value_at_path(payload, lhs.trim()));
        let regex = strip_quoted(rhs);
        return Regex::new(regex).is_ok_and(|compiled| compiled.is_match(&value));
    }
    if let Some((lhs, rhs)) = pattern.split_once("==") {
        let value = value_to_pattern_string(value_at_path(payload, lhs.trim()));
        return value == strip_quoted(rhs);
    }
    if let Some((lhs, rhs)) = pattern.split_once("!=") {
        let value = value_to_pattern_string(value_at_path(payload, lhs.trim()));
        return value != strip_quoted(rhs);
    }
    if pattern.contains('.') {
        return value_at_path(payload, pattern).is_some_and(value_truthy);
    }
    glob_match(
        pattern,
        &value_to_pattern_string(value_at_path(payload, "tool.name")),
    )
}

fn hook_matches(hook: &RuntimeHook, tool_name: Option<&str>, payload: &serde_json::Value) -> bool {
    match &hook.matcher {
        PatternMatcher::ToolNameGlob(pattern) => {
            tool_name.is_some_and(|candidate| glob_match(pattern, candidate))
        }
        PatternMatcher::EventExpression(pattern) => expression_matches(pattern, payload),
    }
}

async fn invoke_vm_hook(
    closure: &Rc<VmClosure>,
    payload: &serde_json::Value,
) -> Result<VmValue, VmError> {
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    let arg = crate::stdlib::json_to_vm_value(payload);
    vm.call_closure_pub(closure, &[arg], &[]).await
}

fn parse_pre_tool_result(value: VmValue) -> Result<PreToolAction, VmError> {
    match value {
        VmValue::Nil => Ok(PreToolAction::Allow),
        VmValue::Dict(map) => {
            if let Some(reason) = map.get("deny") {
                return Ok(PreToolAction::Deny(reason.display()));
            }
            if let Some(args) = map.get("args") {
                return Ok(PreToolAction::Modify(crate::llm::vm_value_to_json(args)));
            }
            Ok(PreToolAction::Allow)
        }
        other => Err(VmError::Runtime(format!(
            "PreToolUse hook must return nil or {{deny, args}}, got {}",
            other.type_name()
        ))),
    }
}

fn parse_post_tool_result(value: VmValue) -> Result<PostToolAction, VmError> {
    match value {
        VmValue::Nil => Ok(PostToolAction::Pass),
        VmValue::String(text) => Ok(PostToolAction::Modify(text.to_string())),
        VmValue::Dict(map) => {
            if let Some(result) = map.get("result") {
                return Ok(PostToolAction::Modify(result.display()));
            }
            Ok(PostToolAction::Pass)
        }
        other => Err(VmError::Runtime(format!(
            "PostToolUse hook must return nil, string, or {{result}}, got {}",
            other.type_name()
        ))),
    }
}

/// Run all matching PreToolUse hooks. Returns the final action.
pub async fn run_pre_tool_hooks(
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<PreToolAction, VmError> {
    let hooks = RUNTIME_HOOKS.with(|hooks| hooks.borrow().clone());
    let mut current_args = args.clone();
    for hook in hooks
        .iter()
        .filter(|hook| hook.event == HookEvent::PreToolUse)
    {
        let payload = serde_json::json!({
            "event": HookEvent::PreToolUse.as_str(),
            "tool": {
                "name": tool_name,
                "args": current_args.clone(),
            },
        });
        if !hook_matches(hook, Some(tool_name), &payload) {
            continue;
        }
        let action = match &hook.handler {
            RuntimeHookHandler::NativePreTool(pre) => pre(tool_name, &current_args),
            RuntimeHookHandler::Vm { closure, .. } => {
                parse_pre_tool_result(invoke_vm_hook(closure, &payload).await?)?
            }
            RuntimeHookHandler::NativePostTool(_) => continue,
        };
        match action {
            PreToolAction::Allow => {}
            PreToolAction::Deny(reason) => return Ok(PreToolAction::Deny(reason)),
            PreToolAction::Modify(new_args) => {
                current_args = new_args;
            }
        }
    }
    if current_args != *args {
        Ok(PreToolAction::Modify(current_args))
    } else {
        Ok(PreToolAction::Allow)
    }
}

/// Run all matching PostToolUse hooks. Returns the (possibly modified) result.
pub async fn run_post_tool_hooks(
    tool_name: &str,
    args: &serde_json::Value,
    result: &str,
) -> Result<String, VmError> {
    let hooks = RUNTIME_HOOKS.with(|hooks| hooks.borrow().clone());
    let mut current = result.to_string();
    for hook in hooks
        .iter()
        .filter(|hook| hook.event == HookEvent::PostToolUse)
    {
        let payload = serde_json::json!({
            "event": HookEvent::PostToolUse.as_str(),
            "tool": {
                "name": tool_name,
                "args": args,
            },
            "result": {
                "text": current.clone(),
            },
        });
        if !hook_matches(hook, Some(tool_name), &payload) {
            continue;
        }
        let action = match &hook.handler {
            RuntimeHookHandler::NativePostTool(post) => post(tool_name, &current),
            RuntimeHookHandler::Vm { closure, .. } => {
                parse_post_tool_result(invoke_vm_hook(closure, &payload).await?)?
            }
            RuntimeHookHandler::NativePreTool(_) => continue,
        };
        match action {
            PostToolAction::Pass => {}
            PostToolAction::Modify(new_result) => {
                current = new_result;
            }
        }
    }
    Ok(current)
}

pub async fn run_lifecycle_hooks(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let hooks = RUNTIME_HOOKS.with(|hooks| hooks.borrow().clone());
    for hook in hooks.iter().filter(|hook| hook.event == event) {
        if !hook_matches(hook, None, payload) {
            continue;
        }
        if let RuntimeHookHandler::Vm { closure, .. } = &hook.handler {
            let _ = invoke_vm_hook(closure, payload).await?;
        }
    }
    Ok(())
}
