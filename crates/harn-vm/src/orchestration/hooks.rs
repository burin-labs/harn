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
    #[serde(rename = "WorkerProgressed")]
    WorkerProgressed,
    #[serde(rename = "WorkerWaitingForInput")]
    WorkerWaitingForInput,
    #[serde(rename = "WorkerCompleted")]
    WorkerCompleted,
    #[serde(rename = "WorkerFailed")]
    WorkerFailed,
    #[serde(rename = "WorkerCancelled")]
    WorkerCancelled,
    #[serde(rename = "PreStep")]
    PreStep,
    #[serde(rename = "PostStep")]
    PostStep,
    #[serde(rename = "OnBudgetThreshold")]
    OnBudgetThreshold,
    #[serde(rename = "OnApprovalRequested")]
    OnApprovalRequested,
    #[serde(rename = "OnHandoffEmitted")]
    OnHandoffEmitted,
    #[serde(rename = "OnPersonaPaused")]
    OnPersonaPaused,
    #[serde(rename = "OnPersonaResumed")]
    OnPersonaResumed,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PreAgentTurn => "PreAgentTurn",
            Self::PostAgentTurn => "PostAgentTurn",
            Self::WorkerSpawned => "WorkerSpawned",
            Self::WorkerProgressed => "WorkerProgressed",
            Self::WorkerWaitingForInput => "WorkerWaitingForInput",
            Self::WorkerCompleted => "WorkerCompleted",
            Self::WorkerFailed => "WorkerFailed",
            Self::WorkerCancelled => "WorkerCancelled",
            Self::PreStep => "PreStep",
            Self::PostStep => "PostStep",
            Self::OnBudgetThreshold => "OnBudgetThreshold",
            Self::OnApprovalRequested => "OnApprovalRequested",
            Self::OnHandoffEmitted => "OnHandoffEmitted",
            Self::OnPersonaPaused => "OnPersonaPaused",
            Self::OnPersonaResumed => "OnPersonaResumed",
        }
    }

    pub fn from_worker_event(event: WorkerEvent) -> Self {
        match event {
            WorkerEvent::WorkerSpawned => Self::WorkerSpawned,
            WorkerEvent::WorkerProgressed => Self::WorkerProgressed,
            WorkerEvent::WorkerWaitingForInput => Self::WorkerWaitingForInput,
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
    EventExpression {
        source: String,
        expression: EventPatternExpression,
    },
}

#[derive(Clone)]
enum EventPatternExpression {
    MatchAll,
    NeverMatch,
    Regex { path: String, regex: Regex },
    Equals { path: String, value: String },
    NotEquals { path: String, value: String },
    PathTruthy(String),
    ToolNameGlob(String),
}

impl std::fmt::Debug for PatternMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolNameGlob(pattern) => f.debug_tuple("ToolNameGlob").field(pattern).finish(),
            Self::EventExpression { source, expression } => f
                .debug_struct("EventExpression")
                .field("source", source)
                .field("expression", expression)
                .finish(),
        }
    }
}

impl std::fmt::Debug for EventPatternExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MatchAll => f.write_str("MatchAll"),
            Self::NeverMatch => f.write_str("NeverMatch"),
            Self::Regex { path, regex } => f
                .debug_struct("Regex")
                .field("path", path)
                .field("regex", &regex.as_str())
                .finish(),
            Self::Equals { path, value } => f
                .debug_struct("Equals")
                .field("path", path)
                .field("value", value)
                .finish(),
            Self::NotEquals { path, value } => f
                .debug_struct("NotEquals")
                .field("path", path)
                .field("value", value)
                .finish(),
            Self::PathTruthy(path) => f.debug_tuple("PathTruthy").field(path).finish(),
            Self::ToolNameGlob(pattern) => f.debug_tuple("ToolNameGlob").field(pattern).finish(),
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

#[derive(Clone, Debug)]
pub struct VmLifecycleHookInvocation {
    pub closure: Rc<VmClosure>,
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
            matcher: compile_event_pattern(pattern.into()),
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
    super::clear_command_policies();
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

fn compile_event_pattern(pattern: String) -> PatternMatcher {
    let trimmed = pattern.trim();
    let expression = if trimmed.is_empty() || trimmed == "*" {
        EventPatternExpression::MatchAll
    } else if let Some((lhs, rhs)) = trimmed.split_once("=~") {
        match Regex::new(strip_quoted(rhs)) {
            Ok(regex) => EventPatternExpression::Regex {
                path: lhs.trim().to_string(),
                regex,
            },
            Err(_) => EventPatternExpression::NeverMatch,
        }
    } else if let Some((lhs, rhs)) = trimmed.split_once("==") {
        EventPatternExpression::Equals {
            path: lhs.trim().to_string(),
            value: strip_quoted(rhs).to_string(),
        }
    } else if let Some((lhs, rhs)) = trimmed.split_once("!=") {
        EventPatternExpression::NotEquals {
            path: lhs.trim().to_string(),
            value: strip_quoted(rhs).to_string(),
        }
    } else if trimmed.contains('.') {
        EventPatternExpression::PathTruthy(trimmed.to_string())
    } else {
        EventPatternExpression::ToolNameGlob(trimmed.to_string())
    };
    PatternMatcher::EventExpression {
        source: pattern,
        expression,
    }
}

fn expression_matches(
    source: &str,
    expression: &EventPatternExpression,
    payload: &serde_json::Value,
) -> bool {
    let pattern = source.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if let Some(target) = value_at_path(payload, "target").and_then(serde_json::Value::as_str) {
        if glob_match(pattern, target) {
            return true;
        }
    }
    match expression {
        EventPatternExpression::MatchAll => true,
        EventPatternExpression::NeverMatch => false,
        EventPatternExpression::Regex { path, regex } => {
            let value = value_to_pattern_string(value_at_path(payload, path));
            regex.is_match(&value)
        }
        EventPatternExpression::Equals { path, value } => {
            value_to_pattern_string(value_at_path(payload, path)) == *value
        }
        EventPatternExpression::NotEquals { path, value } => {
            value_to_pattern_string(value_at_path(payload, path)) != *value
        }
        EventPatternExpression::PathTruthy(path) => {
            value_at_path(payload, path).is_some_and(value_truthy)
        }
        EventPatternExpression::ToolNameGlob(pattern) => glob_match(
            pattern,
            &value_to_pattern_string(value_at_path(payload, "tool.name")),
        ),
    }
}

fn hook_matches(hook: &RuntimeHook, tool_name: Option<&str>, payload: &serde_json::Value) -> bool {
    match &hook.matcher {
        PatternMatcher::ToolNameGlob(pattern) => {
            tool_name.is_some_and(|candidate| glob_match(pattern, candidate))
        }
        PatternMatcher::EventExpression { source, expression } => {
            expression_matches(source, expression, payload)
        }
    }
}

fn runtime_hooks_for_event(event: HookEvent) -> Vec<RuntimeHook> {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .iter()
            .filter(|hook| hook.event == event)
            .cloned()
            .collect()
    })
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
    vm.call_closure_pub(closure, &[arg]).await
}

async fn invoke_vm_lifecycle_hooks(
    closures: Vec<Rc<VmClosure>>,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    let arg = crate::stdlib::json_to_vm_value(payload);
    for closure in closures {
        let _ = vm.call_closure_pub(&closure, &[arg.clone()]).await?;
    }
    Ok(())
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
    let hooks = runtime_hooks_for_event(HookEvent::PreToolUse);
    let mut current_args = args.clone();
    for hook in &hooks {
        let payload = if matches!(hook.matcher, PatternMatcher::EventExpression { .. }) {
            Some(serde_json::json!({
                "event": HookEvent::PreToolUse.as_str(),
                "tool": {
                    "name": tool_name,
                    "args": current_args.clone(),
                },
            }))
        } else {
            None
        };
        if !hook_matches(
            hook,
            Some(tool_name),
            payload.as_ref().unwrap_or(&serde_json::Value::Null),
        ) {
            continue;
        }
        let action = match &hook.handler {
            RuntimeHookHandler::NativePreTool(pre) => pre(tool_name, &current_args),
            RuntimeHookHandler::Vm { closure, .. } => {
                let payload = payload.as_ref().ok_or_else(|| {
                    VmError::Runtime("VM PreToolUse hook requires an event payload".to_string())
                })?;
                parse_pre_tool_result(invoke_vm_hook(closure, payload).await?)?
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
    let hooks = runtime_hooks_for_event(HookEvent::PostToolUse);
    let mut current = result.to_string();
    for hook in &hooks {
        let payload = if matches!(hook.matcher, PatternMatcher::EventExpression { .. }) {
            Some(serde_json::json!({
                "event": HookEvent::PostToolUse.as_str(),
                "tool": {
                    "name": tool_name,
                    "args": args,
                },
                "result": {
                    "text": current.clone(),
                },
            }))
        } else {
            None
        };
        if !hook_matches(
            hook,
            Some(tool_name),
            payload.as_ref().unwrap_or(&serde_json::Value::Null),
        ) {
            continue;
        }
        let action = match &hook.handler {
            RuntimeHookHandler::NativePostTool(post) => post(tool_name, &current),
            RuntimeHookHandler::Vm { closure, .. } => {
                let payload = payload.as_ref().ok_or_else(|| {
                    VmError::Runtime("VM PostToolUse hook requires an event payload".to_string())
                })?;
                parse_post_tool_result(invoke_vm_hook(closure, payload).await?)?
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
    let closures = matching_vm_lifecycle_closures(event, payload);
    if closures.is_empty() {
        return Ok(());
    }
    invoke_vm_lifecycle_hooks(closures, payload).await
}

pub fn matching_vm_lifecycle_hooks(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Vec<VmLifecycleHookInvocation> {
    matching_vm_lifecycle_closures(event, payload)
        .into_iter()
        .map(|closure| VmLifecycleHookInvocation { closure })
        .collect()
}

fn matching_vm_lifecycle_closures(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Vec<Rc<VmClosure>> {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .iter()
            .filter(|hook| hook.event == event)
            .filter(|hook| hook_matches(hook, None, payload))
            .filter_map(|hook| match &hook.handler {
                RuntimeHookHandler::Vm { closure, .. } => Some(Rc::clone(closure)),
                RuntimeHookHandler::NativePreTool(_) | RuntimeHookHandler::NativePostTool(_) => {
                    None
                }
            })
            .collect()
    })
}
