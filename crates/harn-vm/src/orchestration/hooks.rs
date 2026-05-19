//! Runtime lifecycle hooks — tool, agent-turn, and worker interception.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::agent_events::WorkerEvent;
use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
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
    #[serde(rename = "WorkerSuspended")]
    WorkerSuspended,
    #[serde(rename = "WorkerResumed")]
    WorkerResumed,
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
    #[serde(rename = "SessionStart")]
    SessionStart,
    #[serde(rename = "SessionEnd")]
    SessionEnd,
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit,
    #[serde(rename = "PreCompact")]
    PreCompact,
    #[serde(rename = "PostCompact")]
    PostCompact,
    #[serde(rename = "PostTurn")]
    PostTurn,
    #[serde(rename = "PermissionAsked")]
    PermissionAsked,
    #[serde(rename = "PermissionReplied")]
    PermissionReplied,
    #[serde(rename = "FileEdited")]
    FileEdited,
    #[serde(rename = "SessionError")]
    SessionError,
    #[serde(rename = "SessionIdle")]
    SessionIdle,
    #[serde(rename = "PreFinish")]
    PreFinish,
    #[serde(rename = "PostFinish")]
    PostFinish,
    #[serde(rename = "OnUnsettledDetected")]
    OnUnsettledDetected,
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
            Self::WorkerSuspended => "WorkerSuspended",
            Self::WorkerResumed => "WorkerResumed",
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
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::PostTurn => "PostTurn",
            Self::PermissionAsked => "PermissionAsked",
            Self::PermissionReplied => "PermissionReplied",
            Self::FileEdited => "FileEdited",
            Self::SessionError => "SessionError",
            Self::SessionIdle => "SessionIdle",
            Self::PreFinish => "PreFinish",
            Self::PostFinish => "PostFinish",
            Self::OnUnsettledDetected => "OnUnsettledDetected",
        }
    }

    /// Parse a session-level hook event name. Returns `Err` for unknown
    /// or non-session events; persona/tool events are intentionally
    /// rejected so each registration surface owns its own event set.
    pub fn parse_session_event(name: &str) -> Result<Self, String> {
        match name.trim() {
            "SessionStart" | "session_start" => Ok(Self::SessionStart),
            "SessionEnd" | "session_end" => Ok(Self::SessionEnd),
            "UserPromptSubmit" | "user_prompt_submit" => Ok(Self::UserPromptSubmit),
            "PreCompact" | "pre_compact" => Ok(Self::PreCompact),
            "PostCompact" | "post_compact" => Ok(Self::PostCompact),
            "PostTurn" | "post_turn" => Ok(Self::PostTurn),
            "PermissionAsked" | "permission_asked" => Ok(Self::PermissionAsked),
            "PermissionReplied" | "permission_replied" => Ok(Self::PermissionReplied),
            "FileEdited" | "file_edited" => Ok(Self::FileEdited),
            "SessionError" | "session_error" | "error" => Ok(Self::SessionError),
            "SessionIdle" | "session_idle" => Ok(Self::SessionIdle),
            "PreFinish" | "pre_finish" => Ok(Self::PreFinish),
            "PostFinish" | "post_finish" => Ok(Self::PostFinish),
            "OnUnsettledDetected" | "on_unsettled_detected" => Ok(Self::OnUnsettledDetected),
            other => Err(format!("unknown session hook event `{other}`")),
        }
    }

    pub fn from_worker_event(event: WorkerEvent) -> Self {
        match event {
            WorkerEvent::WorkerSpawned => Self::WorkerSpawned,
            WorkerEvent::WorkerProgressed => Self::WorkerProgressed,
            WorkerEvent::WorkerWaitingForInput => Self::WorkerWaitingForInput,
            WorkerEvent::WorkerSuspended => Self::WorkerSuspended,
            WorkerEvent::WorkerResumed => Self::WorkerResumed,
            WorkerEvent::WorkerCompleted => Self::WorkerCompleted,
            WorkerEvent::WorkerFailed => Self::WorkerFailed,
            WorkerEvent::WorkerCancelled => Self::WorkerCancelled,
        }
    }
}

/// Control flow returned by a session-level lifecycle hook.
///
/// Most session events are advisory (`Allow`). The two veto-capable
/// events — `UserPromptSubmit` and `PreCompact` — accept `Block`.
/// `PermissionAsked` additionally accepts a `Decision` short-circuit so
/// hooks can override the dynamic permission policy entirely.
#[derive(Clone, Debug)]
pub enum HookControl {
    Allow,
    Block {
        reason: String,
    },
    Decision {
        kind: String,
        reason: Option<String>,
    },
}

impl HookControl {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block { .. } => "block",
            Self::Decision { kind, .. } => match kind.as_str() {
                "allow" => "decision_allow",
                "deny" => "decision_deny",
                "ask" => "decision_ask",
                _ => "decision_unknown",
            },
        }
    }
}

pub type ReminderSpec = SystemReminder;

/// Side effect emitted by a hook in addition to any control/action
/// result. Reminder effects are appended to the active session
/// transcript's pending reminder event set.
#[derive(Clone, Debug)]
pub enum HookEffect {
    Reminder(ReminderSpec),
}

#[derive(Clone, Debug)]
struct HookOutcome {
    control: HookControl,
    effects: Vec<HookEffect>,
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
    /// Inject a reminder, then continue with the inner pre-tool action.
    Reminder {
        spec: ReminderSpec,
        then: Box<PreToolAction>,
    },
}

/// Action returned by a PostToolUse hook.
#[derive(Clone, Debug)]
pub enum PostToolAction {
    /// Pass the result through unchanged.
    Pass,
    /// Replace the result text.
    Modify(String),
    /// Inject a reminder, then continue with the inner post-tool action.
    Reminder {
        spec: ReminderSpec,
        then: Box<PostToolAction>,
    },
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
    pub handler_name: String,
}

#[derive(Clone, Debug)]
struct VmLifecycleHookRegistration {
    handler_name: String,
    closure: Rc<VmClosure>,
}

thread_local! {
    static RUNTIME_HOOKS: RefCell<Vec<RuntimeHook>> = const { RefCell::new(Vec::new()) };
    /// Pending `FileEdited` notifications queued from sync builtins
    /// (e.g. `write_file`). Drained at safe async boundaries — typically
    /// at the start of each agent-loop turn — so VM closure handlers
    /// can run inside an async builtin context.
    static FILE_EDIT_QUEUE: RefCell<Vec<FileEditedNotification>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug)]
pub struct FileEditedNotification {
    pub path: String,
    pub metadata: serde_json::Value,
}

/// Queue a file-edited notification. Safe to call from sync contexts.
pub fn queue_file_edited(path: &str, metadata: serde_json::Value) {
    FILE_EDIT_QUEUE.with(|queue| {
        queue.borrow_mut().push(FileEditedNotification {
            path: path.to_string(),
            metadata,
        });
    });
}

/// Drain queued file-edited notifications. Returns them in the order
/// they were queued; the caller is responsible for invoking matching
/// `FileEdited` hooks (async context required).
pub fn drain_file_edits() -> Vec<FileEditedNotification> {
    FILE_EDIT_QUEUE.with(|queue| std::mem::take(&mut *queue.borrow_mut()))
}

pub fn clear_file_edit_queue() {
    FILE_EDIT_QUEUE.with(|queue| queue.borrow_mut().clear());
}

pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
        if let Ok(glob) = globset::Glob::new(pattern) {
            if glob.compile_matcher().is_match(name) {
                return true;
            }
        }
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

/// Clear only session-level lifecycle hooks (session_start, session_end,
/// user_prompt_submit, etc.). Leaves tool, persona, step, worker, and
/// agent-turn hooks installed. Mirrors `clear_tool_hooks()` /
/// `clear_persona_hooks()` for the new surface.
pub fn clear_session_hooks() {
    RUNTIME_HOOKS.with(|hooks| {
        hooks.borrow_mut().retain(|hook| {
            !matches!(
                hook.event,
                HookEvent::SessionStart
                    | HookEvent::SessionEnd
                    | HookEvent::UserPromptSubmit
                    | HookEvent::PreCompact
                    | HookEvent::PostCompact
                    | HookEvent::PostTurn
                    | HookEvent::PermissionAsked
                    | HookEvent::PermissionReplied
                    | HookEvent::FileEdited
                    | HookEvent::SessionError
                    | HookEvent::SessionIdle
                    | HookEvent::PreFinish
                    | HookEvent::PostFinish
                    | HookEvent::OnUnsettledDetected
            )
        });
    });
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
    event: HookEvent,
    registrations: Vec<VmLifecycleHookRegistration>,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    let arg = crate::stdlib::json_to_vm_value(payload);
    let session_id = payload
        .get("session")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    for registration in registrations {
        record_hook_call(&session_id, event, &registration.handler_name, payload);
        let raw = vm
            .call_closure_pub(&registration.closure, &[arg.clone()])
            .await?;
        let effects = parse_hook_effects(event, &raw)?;
        record_hook_returned(
            &session_id,
            event,
            &registration.handler_name,
            &HookControl::Allow,
            &raw,
        );
        inject_hook_effects(session_id.as_str(), effects)?;
    }
    Ok(())
}

fn reminder_error(context: &str, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("{context}: {}", message.into()))
}

fn required_reminder_spec_string(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    context: &str,
) -> Result<String, VmError> {
    match options.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.to_string()),
        Some(VmValue::String(_)) | None | Some(VmValue::Nil) => Err(reminder_error(
            context,
            format!("`{key}` must be a non-empty string"),
        )),
        Some(other) => Err(reminder_error(
            context,
            format!("`{key}` must be a string, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_spec_string(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    context: &str,
) -> Result<Option<String>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(reminder_error(
            context,
            format!("`{key}` must be a string or nil, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_spec_bool(
    options: &BTreeMap<String, VmValue>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, VmError> {
    match options.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(reminder_error(
            context,
            format!("`{key}` must be a bool or nil, got {}", other.type_name()),
        )),
    }
}

fn reminder_spec_tags(
    options: &BTreeMap<String, VmValue>,
    context: &str,
) -> Result<Vec<String>, VmError> {
    match options.get("tags") {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(values)) => {
            let mut tags = Vec::new();
            for value in values.iter() {
                let VmValue::String(tag) = value else {
                    return Err(reminder_error(
                        context,
                        format!("`tags` entries must be strings, got {}", value.type_name()),
                    ));
                };
                let trimmed = tag.trim();
                if trimmed.is_empty() {
                    return Err(reminder_error(
                        context,
                        "`tags` entries must be non-empty strings",
                    ));
                }
                if !tags.iter().any(|existing| existing == trimmed) {
                    tags.push(trimmed.to_string());
                }
            }
            Ok(tags)
        }
        Some(other) => Err(reminder_error(
            context,
            format!("`tags` must be a list or nil, got {}", other.type_name()),
        )),
    }
}

fn optional_reminder_spec_ttl(
    options: &BTreeMap<String, VmValue>,
    context: &str,
) -> Result<Option<i64>, VmError> {
    match options.get("ttl_turns") {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) if *value > 0 => Ok(Some(*value)),
        Some(VmValue::Int(_)) => Err(reminder_error(context, "`ttl_turns` must be > 0")),
        Some(other) => Err(reminder_error(
            context,
            format!(
                "`ttl_turns` must be an int or nil, got {}",
                other.type_name()
            ),
        )),
    }
}

fn optional_reminder_spec_propagate(
    options: &BTreeMap<String, VmValue>,
    context: &str,
) -> Result<Option<ReminderPropagate>, VmError> {
    optional_reminder_spec_string(options, "propagate", context)?
        .map(|value| match value.as_str() {
            "all" => Ok(ReminderPropagate::All),
            "session" => Ok(ReminderPropagate::Session),
            "none" => Ok(ReminderPropagate::None),
            _ => Err(reminder_error(
                context,
                "`propagate` must be one of all, session, or none",
            )),
        })
        .transpose()
}

fn optional_reminder_spec_role_hint(
    options: &BTreeMap<String, VmValue>,
    context: &str,
) -> Result<Option<ReminderRoleHint>, VmError> {
    optional_reminder_spec_string(options, "role_hint", context)?
        .map(|value| match value.as_str() {
            "system" => Ok(ReminderRoleHint::System),
            "developer" => Ok(ReminderRoleHint::Developer),
            "user_block" => Ok(ReminderRoleHint::UserBlock),
            "ephemeral_cache" => Ok(ReminderRoleHint::EphemeralCache),
            _ => Err(reminder_error(
                context,
                "`role_hint` must be one of system, developer, user_block, or ephemeral_cache",
            )),
        })
        .transpose()
}

fn parse_reminder_spec(value: &VmValue, context: &str) -> Result<ReminderSpec, VmError> {
    let Some(options) = value.as_dict() else {
        return Err(reminder_error(
            context,
            format!("reminder spec must be a dict, got {}", value.type_name()),
        ));
    };
    const ALLOWED: &[&str] = &[
        "body",
        "tags",
        "dedupe_key",
        "ttl_turns",
        "preserve_on_compact",
        "propagate",
        "role_hint",
    ];
    let unknown = options
        .keys()
        .filter(|key| !ALLOWED.contains(&key.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(reminder_error(
            context,
            format!("unknown reminder option(s): {}", unknown.join(", ")),
        ));
    }
    Ok(SystemReminder {
        id: uuid::Uuid::now_v7().to_string(),
        tags: reminder_spec_tags(options, context)?,
        dedupe_key: optional_reminder_spec_string(options, "dedupe_key", context)?,
        ttl_turns: optional_reminder_spec_ttl(options, context)?,
        preserve_on_compact: optional_reminder_spec_bool(options, "preserve_on_compact", context)?
            .unwrap_or(false),
        propagate: optional_reminder_spec_propagate(options, context)?
            .unwrap_or(ReminderPropagate::Session),
        role_hint: optional_reminder_spec_role_hint(options, context)?
            .unwrap_or(ReminderRoleHint::System),
        source: ReminderSource::Hook,
        body: required_reminder_spec_string(options, "body", context)?,
        fired_at_turn: 0,
        originating_agent_id: None,
    })
}

fn looks_like_reminder_spec(map: &BTreeMap<String, VmValue>) -> bool {
    map.contains_key("body")
        && !map.contains_key("deny")
        && !map.contains_key("args")
        && !map.contains_key("result")
        && !map.contains_key("output")
        && !map.contains_key("modify")
        && !map.contains_key("block")
        && !map.contains_key("decision")
        && !map.contains_key("action")
        && !map.contains_key("control")
}

fn parse_hook_effect_item(event: HookEvent, value: &VmValue) -> Result<HookEffect, VmError> {
    let context = format!("{} hook reminder", event.as_str());
    if let Some(map) = value.as_dict() {
        if let Some(reminder) = map.get("reminder") {
            return Ok(HookEffect::Reminder(parse_reminder_spec(
                reminder, &context,
            )?));
        }
        if matches!(
            map.get("type")
                .or_else(|| map.get("kind"))
                .map(|value| value.display())
                .as_deref(),
            Some("reminder" | "Reminder")
        ) {
            let spec = map
                .get("spec")
                .or_else(|| map.get("reminder"))
                .ok_or_else(|| reminder_error(&context, "reminder effect missing `spec`"))?;
            return Ok(HookEffect::Reminder(parse_reminder_spec(spec, &context)?));
        }
        if looks_like_reminder_spec(map) {
            return Ok(HookEffect::Reminder(parse_reminder_spec(value, &context)?));
        }
    }
    Err(reminder_error(
        &context,
        "hook effect must be {reminder: {...}} or a reminder spec",
    ))
}

pub fn parse_hook_effects(event: HookEvent, value: &VmValue) -> Result<Vec<HookEffect>, VmError> {
    let Some(map) = value.as_dict() else {
        if let VmValue::List(items) = value {
            return items
                .iter()
                .map(|item| parse_hook_effect_item(event, item))
                .collect();
        }
        return Ok(Vec::new());
    };

    let mut effects = Vec::new();
    if let Some(items) = map.get("effects") {
        match items {
            VmValue::List(list) => {
                for item in list.iter() {
                    effects.push(parse_hook_effect_item(event, item)?);
                }
            }
            other => effects.push(parse_hook_effect_item(event, other)?),
        }
    }
    if let Some(reminder) = map.get("reminder") {
        let context = format!("{} hook reminder", event.as_str());
        effects.push(HookEffect::Reminder(parse_reminder_spec(
            reminder, &context,
        )?));
    } else if effects.is_empty() && looks_like_reminder_spec(map) {
        let context = format!("{} hook reminder", event.as_str());
        effects.push(HookEffect::Reminder(parse_reminder_spec(value, &context)?));
    }
    Ok(effects)
}

fn action_value_after_effects(value: VmValue, default_action: VmValue) -> VmValue {
    let VmValue::Dict(map) = value else {
        return value;
    };
    if let Some(then) = map.get("then") {
        return then.clone();
    }
    let has_effects = map.contains_key("effects")
        || map.contains_key("reminder")
        || looks_like_reminder_spec(map.as_ref());
    if !has_effects {
        return VmValue::Dict(map);
    }
    let mut action = map.as_ref().clone();
    action.remove("effects");
    action.remove("reminder");
    action.remove("then");
    if action.keys().any(|key| {
        matches!(
            key.as_str(),
            "deny" | "args" | "result" | "output" | "modify" | "block" | "decision" | "action"
        )
    }) {
        VmValue::Dict(Rc::new(action))
    } else {
        default_action
    }
}

pub fn collect_hook_effects_and_action(
    event: HookEvent,
    value: VmValue,
    default_action: VmValue,
) -> Result<(VmValue, Vec<HookEffect>), VmError> {
    let mut current = value;
    let mut effects = Vec::new();
    for _ in 0..32 {
        let current_effects = parse_hook_effects(event, &current)?;
        if current_effects.is_empty() {
            return Ok((current, effects));
        }
        effects.extend(current_effects);
        current = action_value_after_effects(current, default_action.clone());
    }
    Err(VmError::Runtime(format!(
        "{} hook reminder return nested too deeply",
        event.as_str()
    )))
}

fn inject_hook_effects(session_id: &str, effects: Vec<HookEffect>) -> Result<(), VmError> {
    if effects.is_empty() {
        return Ok(());
    }
    let target_session = if session_id.is_empty() {
        crate::agent_sessions::current_session_id().unwrap_or_default()
    } else {
        session_id.to_string()
    };
    if target_session.is_empty() {
        return Ok(());
    }
    for effect in effects {
        match effect {
            HookEffect::Reminder(spec) => {
                crate::agent_sessions::inject_reminder(&target_session, spec)
                    .map_err(VmError::Runtime)?;
            }
        }
    }
    Ok(())
}

pub fn inject_hook_effects_into_current_session(effects: Vec<HookEffect>) -> Result<(), VmError> {
    inject_hook_effects("", effects)
}

fn wrap_pre_tool_effects(effects: Vec<HookEffect>, mut action: PreToolAction) -> PreToolAction {
    for effect in effects.into_iter().rev() {
        match effect {
            HookEffect::Reminder(spec) => {
                action = PreToolAction::Reminder {
                    spec,
                    then: Box::new(action),
                };
            }
        }
    }
    action
}

fn wrap_post_tool_effects(effects: Vec<HookEffect>, mut action: PostToolAction) -> PostToolAction {
    for effect in effects.into_iter().rev() {
        match effect {
            HookEffect::Reminder(spec) => {
                action = PostToolAction::Reminder {
                    spec,
                    then: Box::new(action),
                };
            }
        }
    }
    action
}

fn parse_pre_tool_result(value: VmValue) -> Result<PreToolAction, VmError> {
    let (value, effects) =
        collect_hook_effects_and_action(HookEvent::PreToolUse, value, VmValue::Nil)?;
    match value {
        VmValue::Nil => Ok(wrap_pre_tool_effects(effects, PreToolAction::Allow)),
        VmValue::Dict(map) => {
            if let Some(reason) = map.get("deny") {
                return Ok(wrap_pre_tool_effects(
                    effects,
                    PreToolAction::Deny(reason.display()),
                ));
            }
            if let Some(args) = map.get("args") {
                return Ok(wrap_pre_tool_effects(
                    effects,
                    PreToolAction::Modify(crate::llm::vm_value_to_json(args)),
                ));
            }
            Ok(wrap_pre_tool_effects(effects, PreToolAction::Allow))
        }
        other => Err(VmError::Runtime(format!(
            "PreToolUse hook must return nil or {{deny, args}}, got {}",
            other.type_name()
        ))),
    }
}

fn parse_post_tool_result(value: VmValue) -> Result<PostToolAction, VmError> {
    let (value, effects) =
        collect_hook_effects_and_action(HookEvent::PostToolUse, value, VmValue::Nil)?;
    match value {
        VmValue::Nil => Ok(wrap_post_tool_effects(effects, PostToolAction::Pass)),
        VmValue::String(text) => Ok(wrap_post_tool_effects(
            effects,
            PostToolAction::Modify(text.to_string()),
        )),
        VmValue::Dict(map) => {
            if let Some(result) = map.get("result") {
                return Ok(wrap_post_tool_effects(
                    effects,
                    PostToolAction::Modify(result.display()),
                ));
            }
            Ok(wrap_post_tool_effects(effects, PostToolAction::Pass))
        }
        other => Err(VmError::Runtime(format!(
            "PostToolUse hook must return nil, string, or {{result}}, got {}",
            other.type_name()
        ))),
    }
}

pub fn apply_pre_tool_action(
    action: PreToolAction,
    current_args: &mut serde_json::Value,
) -> Result<Option<String>, VmError> {
    match action {
        PreToolAction::Allow => Ok(None),
        PreToolAction::Deny(reason) => Ok(Some(reason)),
        PreToolAction::Modify(new_args) => {
            *current_args = new_args;
            Ok(None)
        }
        PreToolAction::Reminder { spec, then } => {
            inject_hook_effects_into_current_session(vec![HookEffect::Reminder(spec)])?;
            apply_pre_tool_action(*then, current_args)
        }
    }
}

fn apply_post_tool_action(action: PostToolAction, current: String) -> Result<String, VmError> {
    match action {
        PostToolAction::Pass => Ok(current),
        PostToolAction::Modify(new_result) => Ok(new_result),
        PostToolAction::Reminder { spec, then } => {
            inject_hook_effects_into_current_session(vec![HookEffect::Reminder(spec)])?;
            apply_post_tool_action(*then, current)
        }
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
        if let Some(reason) = apply_pre_tool_action(action, &mut current_args)? {
            return Ok(PreToolAction::Deny(reason));
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
            PostToolAction::Reminder { spec, then } => {
                inject_hook_effects_into_current_session(vec![HookEffect::Reminder(spec)])?;
                current = apply_post_tool_action(*then, current)?;
            }
        }
    }
    Ok(current)
}

pub async fn run_lifecycle_hooks(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let registrations = matching_vm_lifecycle_registrations(event, payload);
    if registrations.is_empty() {
        return Ok(());
    }
    invoke_vm_lifecycle_hooks(event, registrations, payload).await
}

/// Run veto-capable session-level lifecycle hooks. Successive hooks see
/// `Allow`; the first non-`Allow` return short-circuits and is returned
/// to the caller. Hook invocations and decisions are captured on the
/// active session's transcript under `hook_call`, `hook_returned`, and
/// `hook_vetoed` so a replay reproduces the same control flow.
pub async fn run_lifecycle_hooks_with_control(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<HookControl, VmError> {
    let registrations = matching_vm_lifecycle_registrations(event, payload);
    if registrations.is_empty() {
        return Ok(HookControl::Allow);
    }
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "session lifecycle hook requires an async builtin VM context".to_string(),
        ));
    };
    let arg = crate::stdlib::json_to_vm_value(payload);
    let session_id = payload
        .get("session")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    for registration in registrations {
        record_hook_call(&session_id, event, &registration.handler_name, payload);
        let raw = vm
            .call_closure_pub(&registration.closure, &[arg.clone()])
            .await?;
        let outcome = parse_hook_outcome(event, &raw)?;
        record_hook_returned(
            &session_id,
            event,
            &registration.handler_name,
            &outcome.control,
            &raw,
        );
        inject_hook_effects(session_id.as_str(), outcome.effects)?;
        if !matches!(outcome.control, HookControl::Allow) {
            record_hook_vetoed(
                &session_id,
                event,
                &registration.handler_name,
                &outcome.control,
            );
            return Ok(outcome.control);
        }
    }
    Ok(HookControl::Allow)
}

fn parse_hook_outcome(event: HookEvent, value: &VmValue) -> Result<HookOutcome, VmError> {
    let effects = parse_hook_effects(event, value)?;
    let action_value = if matches!(value, VmValue::List(_)) {
        VmValue::Nil
    } else {
        action_value_after_effects(value.clone(), VmValue::Nil)
    };
    let control = parse_hook_control(event, &action_value)?;
    Ok(HookOutcome { control, effects })
}

fn parse_hook_control(event: HookEvent, value: &VmValue) -> Result<HookControl, VmError> {
    match value {
        VmValue::Nil | VmValue::Bool(true) => Ok(HookControl::Allow),
        VmValue::Bool(false) => Ok(HookControl::Block {
            reason: format!("{} hook returned false", event.as_str()),
        }),
        VmValue::Dict(map) => {
            if let Some(decision) = map.get("decision") {
                let kind = decision.display();
                let kind_norm = kind.trim().to_ascii_lowercase();
                if !matches!(kind_norm.as_str(), "allow" | "deny" | "ask") {
                    return Err(VmError::Runtime(format!(
                        "{} hook `decision` must be \"allow\", \"deny\", or \"ask\"; got \"{kind}\"",
                        event.as_str()
                    )));
                }
                let reason = map.get("reason").and_then(|v| match v {
                    VmValue::Nil => None,
                    other => Some(other.display()),
                });
                return Ok(HookControl::Decision {
                    kind: kind_norm,
                    reason,
                });
            }
            let block = map.get("block").map(vm_value_truthy).unwrap_or(false);
            if block {
                let reason = map
                    .get("reason")
                    .map(|v| v.display())
                    .unwrap_or_else(|| format!("{} hook blocked the operation", event.as_str()));
                return Ok(HookControl::Block { reason });
            }
            Ok(HookControl::Allow)
        }
        other => Err(VmError::Runtime(format!(
            "{} hook must return nil, bool, or a control dict; got {}",
            event.as_str(),
            other.type_name()
        ))),
    }
}

fn vm_value_truthy(value: &VmValue) -> bool {
    match value {
        VmValue::Nil => false,
        VmValue::Bool(value) => *value,
        VmValue::Int(value) => *value != 0,
        VmValue::Float(value) => *value != 0.0,
        VmValue::String(value) => !value.is_empty(),
        VmValue::List(value) => !value.is_empty(),
        VmValue::Dict(value) => !value.is_empty(),
        _ => true,
    }
}

fn record_hook_call(
    session_id: &str,
    event: HookEvent,
    handler: &str,
    payload: &serde_json::Value,
) {
    if session_id.is_empty() {
        return;
    }
    let metadata = serde_json::json!({
        "event": event.as_str(),
        "handler": handler,
        "payload": payload,
    });
    let entry = crate::llm::helpers::transcript_event(
        "hook_call",
        "system",
        "internal",
        &format!("hook {} invoked: {}", event.as_str(), handler),
        Some(metadata),
    );
    let _ = crate::agent_sessions::append_event(session_id, entry);
}

fn record_hook_returned(
    session_id: &str,
    event: HookEvent,
    handler: &str,
    control: &HookControl,
    raw: &VmValue,
) {
    if session_id.is_empty() {
        return;
    }
    let metadata = serde_json::json!({
        "event": event.as_str(),
        "handler": handler,
        "result": control.as_str(),
        "raw": crate::llm::vm_value_to_json(raw),
    });
    let entry = crate::llm::helpers::transcript_event(
        "hook_returned",
        "system",
        "internal",
        &format!(
            "hook {} returned {} from {}",
            event.as_str(),
            control.as_str(),
            handler
        ),
        Some(metadata),
    );
    let _ = crate::agent_sessions::append_event(session_id, entry);
}

fn record_hook_vetoed(session_id: &str, event: HookEvent, handler: &str, control: &HookControl) {
    if session_id.is_empty() {
        return;
    }
    let (reason, decision) = match control {
        HookControl::Allow => return,
        HookControl::Block { reason } => (reason.clone(), None),
        HookControl::Decision { kind, reason } => (
            reason.clone().unwrap_or_else(|| format!("decision={kind}")),
            Some(kind.clone()),
        ),
    };
    let metadata = serde_json::json!({
        "event": event.as_str(),
        "handler": handler,
        "reason": reason,
        "decision": decision,
    });
    let entry = crate::llm::helpers::transcript_event(
        "hook_vetoed",
        "system",
        "internal",
        &format!("hook {} vetoed by {}: {reason}", event.as_str(), handler),
        Some(metadata),
    );
    let _ = crate::agent_sessions::append_event(session_id, entry);
}

pub fn matching_vm_lifecycle_hooks(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Vec<VmLifecycleHookInvocation> {
    matching_vm_lifecycle_registrations(event, payload)
        .into_iter()
        .map(|registration| VmLifecycleHookInvocation {
            closure: registration.closure,
            handler_name: registration.handler_name,
        })
        .collect()
}

fn matching_vm_lifecycle_registrations(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Vec<VmLifecycleHookRegistration> {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .iter()
            .filter(|hook| hook.event == event)
            .filter(|hook| hook_matches(hook, None, payload))
            .filter_map(|hook| match &hook.handler {
                RuntimeHookHandler::Vm {
                    closure,
                    handler_name,
                } => Some(VmLifecycleHookRegistration {
                    handler_name: handler_name.clone(),
                    closure: Rc::clone(closure),
                }),
                RuntimeHookHandler::NativePreTool(_) | RuntimeHookHandler::NativePostTool(_) => {
                    None
                }
            })
            .collect()
    })
}
