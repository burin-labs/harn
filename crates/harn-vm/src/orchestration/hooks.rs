//! Runtime lifecycle hooks — tool, agent-turn, and worker interception.

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use harn_parser::diagnostic_codes::Code;

use crate::agent_events::WorkerEvent;
use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
use crate::value::{VmClosure, VmError, VmValue};

tokio::task_local! {
    static HOOK_REMINDER_REPORTS_TASK: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>;
}

fn record_hook_reminder_report(report: serde_json::Value) {
    let _ = HOOK_REMINDER_REPORTS_TASK.try_with(|reports| reports.lock().push(report));
}

pub async fn scope_hook_reminder_reports<F, T>(future: F) -> (T, Vec<serde_json::Value>)
where
    F: Future<Output = T>,
{
    let reports = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let output = HOOK_REMINDER_REPORTS_TASK
        .scope(reports.clone(), future)
        .await;
    let reports = std::mem::take(&mut *reports.lock());
    (output, reports)
}

/// High-level grouping for a hook event. Drives `parse_session_event` /
/// `parse_provider_event` routing, reminder support, and the
/// `clear_session_hooks` filter, so each behavior derives from the
/// variant's declared kind rather than a hand-maintained match arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HookEventKind {
    /// Tool-call lifecycle (PreToolUse / PostToolUse).
    Tool,
    /// Agent-turn lifecycle (PreAgentTurn / PostAgentTurn).
    AgentTurn,
    /// Worker lifecycle — the only kind that rejects reminder effects.
    Worker,
    /// Step lifecycle (PreStep / PostStep).
    Step,
    /// Notification surfaces (budget / approval / handoff / persona).
    Notification,
    /// Session-level lifecycle. Eligible for `parse_session_event` and
    /// scoped clearing via `clear_session_hooks`.
    Session,
}

/// `hook_events!` — single source of truth for `HookEvent`. Emits the
/// enum, `as_str`, `kind`, `supports_reminder_effects`,
/// `is_session_lifecycle`, `parse_session_event`, `parse_provider_event`,
/// `from_worker_event`, and the canonical `ALL` slice. Adding a variant
/// requires only one new line — every dispatch table is derived.
///
/// Each entry has the form
/// `Variant { kind: Kind [, provider_parse: true] [, aliases: [..]] }`:
/// `provider_parse` flags variants accepted directly by
/// `parse_provider_event` (Worker variants are accepted by virtue of
/// `kind: Worker`); `aliases` lists explicit extra wire names beyond
/// the auto-derived `snake_case` of the variant identifier.
macro_rules! hook_events {
    (
        $(
            $(#[$attr:meta])*
            $variant:ident {
                kind: $kind:ident
                $(, provider_parse: $provider_parse:literal)?
                $(, aliases: [$($alias:literal),* $(,)?])?
                $(,)?
            }
        ),* $(,)?
    ) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum HookEvent {
            $(
                $(#[$attr])*
                $variant,
            )*
        }

        impl HookEvent {
            /// Canonical PascalCase wire name.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)*
                }
            }

            /// High-level grouping — drives every other routing predicate.
            pub const fn kind(self) -> HookEventKind {
                match self {
                    $(Self::$variant => HookEventKind::$kind,)*
                }
            }

            /// Reminder effects are rejected by Worker events because
            /// they fire from contexts without a pending tool-call /
            /// transcript slot for the reminder to attach to.
            pub const fn supports_reminder_effects(self) -> bool {
                !matches!(self.kind(), HookEventKind::Worker)
            }

            /// Whether `clear_session_hooks` and `parse_session_event`
            /// own this variant.
            pub const fn is_session_lifecycle(self) -> bool {
                matches!(self.kind(), HookEventKind::Session)
            }

            /// All variants in declaration order. Stable enough that
            /// `parse_*` functions can iterate it.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// Whether this variant is accepted directly by
            /// `parse_provider_event` (independent of the
            /// session-parser fallback). Worker variants are accepted
            /// implicitly by their kind.
            const fn in_provider_parse(self) -> bool {
                match self {
                    $(Self::$variant => hook_events!(@or_false $($provider_parse)?),)*
                }
            }

            /// Explicit non-snake-case aliases declared on the variant.
            const fn extra_aliases(self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$($($alias),*)?],)*
                }
            }

            /// Parse a session-level hook event name. Returns `Err` for
            /// unknown or non-session events; persona/tool/worker events
            /// are intentionally rejected so each registration surface
            /// owns its own event set. Accepts the canonical PascalCase
            /// spelling, its auto-derived snake_case, and any explicit
            /// `aliases: [...]` declared in `hook_events!`.
            pub fn parse_session_event(name: &str) -> Result<Self, String> {
                let trimmed = name.trim();
                for &event in Self::ALL.iter().filter(|e| e.is_session_lifecycle()) {
                    if event_matches_name(event, trimmed) {
                        return Ok(event);
                    }
                }
                Err(format!("unknown session hook event `{trimmed}`"))
            }

            /// Parse a reminder-provider event name. Accepts Worker
            /// events, any variant flagged `provider_parse: true` in
            /// `hook_events!`, and (by fallback) every session event.
            pub fn parse_provider_event(name: &str) -> Result<Self, String> {
                let trimmed = name.trim();
                for &event in Self::ALL.iter().filter(|e| {
                    matches!(e.kind(), HookEventKind::Worker) || e.in_provider_parse()
                }) {
                    if event_matches_name(event, trimmed) {
                        return Ok(event);
                    }
                }
                Self::parse_session_event(trimmed)
                    .map_err(|_| format!("unknown reminder provider event `{trimmed}`"))
            }
        }
    };
    (@or_false $val:literal) => { $val };
    (@or_false) => { false };
}

hook_events! {
    PreToolUse              { kind: Tool },
    PostToolUse             { kind: Tool, provider_parse: true },
    PreAgentTurn            { kind: AgentTurn },
    PostAgentTurn           { kind: AgentTurn, provider_parse: true },
    WorkerSpawned           { kind: Worker },
    WorkerProgressed        { kind: Worker },
    WorkerWaitingForInput   { kind: Worker },
    WorkerSuspended         { kind: Worker },
    WorkerResumed           { kind: Worker },
    WorkerCompleted         { kind: Worker },
    WorkerFailed            { kind: Worker },
    WorkerStopped           { kind: Worker },
    WorkerCancelled         { kind: Worker },
    PreStep                 { kind: Step },
    PostStep                { kind: Step, provider_parse: true },
    OnBudgetThreshold       { kind: Notification, provider_parse: true },
    OnApprovalRequested     { kind: Notification },
    OnHandoffEmitted        { kind: Notification },
    OnPersonaPaused         { kind: Notification },
    OnPersonaResumed        { kind: Notification },
    SessionStart            { kind: Session },
    SessionEnd              { kind: Session },
    UserPromptSubmit        { kind: Session },
    PreCompact              { kind: Session },
    PostCompact             { kind: Session },
    PostTurn                { kind: Session },
    PermissionAsked         { kind: Session },
    PermissionReplied       { kind: Session },
    FileEdited              { kind: Session },
    SessionError            { kind: Session, aliases: ["error"] },
    SessionIdle             { kind: Session },
    PreFinish               { kind: Session },
    PostFinish              { kind: Session },
    OnUnsettledDetected     { kind: Session },
    PreSuspend              { kind: Session },
    PostSuspend             { kind: Session },
    PreResume               { kind: Session },
    PostResume              { kind: Session },
    PreDrain                { kind: Session },
    PostDrain               { kind: Session },
    OnDrainDecision         { kind: Session },
    /// Fired by `__agent_loop_checkpoint(kind, ...)` at every safe
    /// injection seam in the agent loop. Pattern-match on `payload.kind`
    /// to subscribe to specific seams (e.g. `kind=="pre_tool_dispatch"`)
    /// or use `*` to observe every checkpoint pass.
    LoopCheckpoint          { kind: Session },
}

impl HookEvent {
    pub fn from_worker_event(event: WorkerEvent) -> Self {
        match event {
            WorkerEvent::WorkerSpawned => Self::WorkerSpawned,
            WorkerEvent::WorkerProgressed => Self::WorkerProgressed,
            WorkerEvent::WorkerWaitingForInput => Self::WorkerWaitingForInput,
            WorkerEvent::WorkerSuspended => Self::WorkerSuspended,
            WorkerEvent::WorkerResumed => Self::WorkerResumed,
            WorkerEvent::WorkerCompleted => Self::WorkerCompleted,
            WorkerEvent::WorkerFailed => Self::WorkerFailed,
            WorkerEvent::WorkerStopped => Self::WorkerStopped,
            WorkerEvent::WorkerCancelled => Self::WorkerCancelled,
        }
    }
}

fn pascal_to_snake_buf(pascal: &str, buf: &mut String) {
    buf.clear();
    buf.reserve(pascal.len() + 4);
    for (i, c) in pascal.char_indices() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                buf.push('_');
            }
            buf.push(c.to_ascii_lowercase());
        } else {
            buf.push(c);
        }
    }
}

fn event_matches_name(event: HookEvent, candidate: &str) -> bool {
    let pascal = event.as_str();
    if candidate == pascal {
        return true;
    }
    if event.extra_aliases().contains(&candidate) {
        return true;
    }
    let mut snake = String::new();
    pascal_to_snake_buf(pascal, &mut snake);
    candidate == snake
}

/// Control flow returned by a session-level lifecycle hook.
///
/// Most session events are advisory (`Allow`). Veto-capable events —
/// `UserPromptSubmit`, `PreCompact`, plus the lifecycle gates
/// `PreSuspend` / `PreResume` / `PreDrain` / `OnDrainDecision` /
/// `OnUnsettledDetected` — accept `Block`. `PermissionAsked` accepts a
/// `Decision` short-circuit so hooks can override the dynamic
/// permission policy entirely. Lifecycle gates that support payload
/// rewriting (PreSuspend / PreResume / PreDrain / OnDrainDecision /
/// OnUnsettledDetected) accept `Modify { payload }` to amend the
/// dispatched event — the dispatcher applies the modified payload
/// before resuming the lifecycle step. `PreFinish` rejects `Block`
/// explicitly; the runtime surfaces a dedicated error pointing at
/// `OnFinish.block_until_settled`.
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
    Modify {
        payload: serde_json::Value,
    },
}

impl HookControl {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block { .. } => "block",
            Self::Modify { .. } => "modify",
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
pub type PreToolHookFn = Arc<dyn Fn(&str, &serde_json::Value) -> PreToolAction + Send + Sync>;
pub type PostToolHookFn = Arc<dyn Fn(&str, &str) -> PostToolAction + Send + Sync>;

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

/// A manifest hook handler that has not been resolved to a [`VmClosure`]
/// yet. Resolving a handler requires loading its module's whole import
/// graph (for an IDE host that is ~1s of instantiation), so eager
/// resolution at registration time made every test — even pure-logic
/// unit tests that never fire a hook — pay that cost. A lazy handler
/// defers the module load until the hook actually fires, against the
/// firing child VM (whose `module_cache` already holds the graph if the
/// test imported it, making the fire-time load a cache hit and keeping
/// per-test module-state isolation intact).
#[derive(Clone, Debug)]
pub struct LazyVmHookHandler {
    /// Directory of the manifest that declared the hook.
    pub manifest_dir: std::path::PathBuf,
    /// Source path of the module the handler lives in.
    pub module_path: std::path::PathBuf,
    /// Exported function name to resolve from that module.
    pub function_name: String,
}

#[derive(Clone)]
enum RuntimeHookHandler {
    NativePreTool(PreToolHookFn),
    NativePostTool(PostToolHookFn),
    Vm {
        handler_name: String,
        closure: Arc<VmClosure>,
    },
    /// Manifest hook whose closure is resolved on first fire. See
    /// [`LazyVmHookHandler`].
    LazyVm {
        handler_name: String,
        lazy: LazyVmHookHandler,
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
            Self::LazyVm { handler_name, .. } => f
                .debug_struct("LazyVm")
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
    /// Eagerly-resolved closure, or `None` for a lazy manifest hook that
    /// must be resolved with [`VmLifecycleHookInvocation::resolve`].
    closure: Option<Arc<VmClosure>>,
    lazy: Option<LazyVmHookHandler>,
    pub handler_name: String,
}

impl VmLifecycleHookInvocation {
    /// Resolve this invocation's handler closure against `vm`, loading the
    /// handler's module on demand for a lazy manifest hook (a cache hit
    /// when `vm` already imported the graph).
    pub async fn resolve(&self, vm: &mut crate::vm::Vm) -> Result<Arc<VmClosure>, VmError> {
        match (&self.closure, &self.lazy) {
            (Some(closure), _) => Ok(Arc::clone(closure)),
            (None, Some(lazy)) => resolve_lazy_hook_closure(vm, lazy).await,
            (None, None) => Err(VmError::Runtime(format!(
                "lifecycle hook '{}' has no handler",
                self.handler_name
            ))),
        }
    }
}

#[derive(Clone, Debug)]
enum VmLifecycleHandlerRef {
    Eager(Arc<VmClosure>),
    Lazy(LazyVmHookHandler),
}

#[derive(Clone, Debug)]
struct VmLifecycleHookRegistration {
    handler_name: String,
    handler: VmLifecycleHandlerRef,
}

thread_local! {
    static RUNTIME_HOOKS: RefCell<Vec<RuntimeHook>> = const { RefCell::new(Vec::new()) };
    /// Pending `FileEdited` notifications queued from sync builtins
    /// (e.g. `write_file`). Drained at safe async boundaries — typically
    /// at the start of each agent-loop turn — so VM closure handlers
    /// can run inside an async builtin context.
    static FILE_EDIT_QUEUE: RefCell<Vec<FileEditedNotification>> = const { RefCell::new(Vec::new()) };
    /// Optional singleton PreToolUse hook owned by stdlib opt-in surfaces
    /// (currently the `path_scope_guard` from #2221). Kept separate from
    /// `RUNTIME_HOOKS` so the runtime can swap or clear it without
    /// touching user-registered hooks.
    static SINGLETON_PRE_TOOL_HOOK: RefCell<Option<PreToolHookFn>> = const { RefCell::new(None) };
}

/// Install (or replace, with `None`) the singleton runtime pre-tool
/// hook. The singleton runs ahead of user-registered hooks so a tagged
/// deny lands in the reminder path before any other hook fires.
pub fn set_singleton_pre_tool_hook(hook: Option<PreToolHookFn>) {
    SINGLETON_PRE_TOOL_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

pub fn singleton_pre_tool_hook() -> Option<PreToolHookFn> {
    SINGLETON_PRE_TOOL_HOOK.with(|slot| slot.borrow().clone())
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

// The workspace-wide name matcher (re-exported as
// `crate::orchestration::glob_match` for the tool-surface, permission, and
// step-runtime call sites). Semantics live in `harn-glob`.
pub(crate) use harn_glob::match_name as glob_match;

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
    closure: Arc<VmClosure>,
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

/// Register a manifest hook whose handler closure is resolved on first
/// fire instead of at registration time. See [`LazyVmHookHandler`].
pub fn register_vm_hook_lazy(
    event: HookEvent,
    pattern: impl Into<String>,
    handler_name: impl Into<String>,
    lazy: LazyVmHookHandler,
) {
    RUNTIME_HOOKS.with(|hooks| {
        hooks.borrow_mut().push(RuntimeHook {
            event,
            matcher: compile_event_pattern(pattern.into()),
            handler: RuntimeHookHandler::LazyVm {
                handler_name: handler_name.into(),
                lazy,
            },
        });
    });
}

/// Resolve a lazy hook handler to its closure against `vm`, loading the
/// handler's module (a cache hit when the firing VM already imported the
/// graph). The resolved closure is memoized on the firing VM's module
/// cache via `load_module_exports`, so repeated fires within one VM stay
/// cheap, while a fresh VM (next test) re-resolves against its own state.
async fn resolve_lazy_hook_closure(
    vm: &mut crate::vm::Vm,
    lazy: &LazyVmHookHandler,
) -> Result<Arc<VmClosure>, VmError> {
    let exports = vm
        .load_module_exports(&lazy.module_path)
        .await
        .map_err(|error| {
            VmError::Runtime(format!(
                "failed to load manifest hook module '{}': {error}",
                lazy.module_path.display()
            ))
        })?;
    exports.get(&lazy.function_name).cloned().ok_or_else(|| {
        VmError::Runtime(format!(
            "manifest hook handler '{}' is not exported by module '{}'",
            lazy.function_name,
            lazy.module_path.display()
        ))
    })
}

async fn resolve_lifecycle_handler(
    vm: &mut crate::vm::Vm,
    handler: &VmLifecycleHandlerRef,
) -> Result<Arc<VmClosure>, VmError> {
    match handler {
        VmLifecycleHandlerRef::Eager(closure) => Ok(Arc::clone(closure)),
        VmLifecycleHandlerRef::Lazy(lazy) => resolve_lazy_hook_closure(vm, lazy).await,
    }
}

pub fn clear_tool_hooks() {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow_mut()
            .retain(|hook| !matches!(hook.event, HookEvent::PreToolUse | HookEvent::PostToolUse));
    });
    set_singleton_pre_tool_hook(None);
}

pub fn clear_runtime_hooks() {
    RUNTIME_HOOKS.with(|hooks| hooks.borrow_mut().clear());
    set_singleton_pre_tool_hook(None);
    super::clear_command_policies();
}

/// Clear only session-level lifecycle hooks (session_start, session_end,
/// user_prompt_submit, etc.). Leaves tool, persona, step, worker, and
/// agent-turn hooks installed. Mirrors `clear_tool_hooks()` /
/// `clear_persona_hooks()` for the new surface.
pub fn clear_session_hooks() {
    RUNTIME_HOOKS.with(|hooks| {
        hooks
            .borrow_mut()
            .retain(|hook| !hook.event.is_session_lifecycle());
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

/// Invoke a VM-backed hook handler (eager [`RuntimeHookHandler::Vm`] or
/// lazily-resolved [`RuntimeHookHandler::LazyVm`]) against a child of the
/// firing VM. Returns `None` for handlers that are not VM-backed.
async fn invoke_vm_hook_handler(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    handler: &RuntimeHookHandler,
    payload: &serde_json::Value,
) -> Result<Option<VmValue>, VmError> {
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return Err(VmError::Runtime(
            "runtime hook requires an async builtin VM context".to_string(),
        ));
    };
    let closure = match handler {
        RuntimeHookHandler::Vm { closure, .. } => Arc::clone(closure),
        RuntimeHookHandler::LazyVm { lazy, .. } => resolve_lazy_hook_closure(&mut vm, lazy).await?,
        _ => return Ok(None),
    };
    let arg = crate::stdlib::json_to_vm_value(payload);
    // First-party registered hook (`register_session_hook` /
    // `register_checkpoint_hook`): the runtime chose to invoke this closure,
    // so its body's bridge/builtin calls are a trusted bridge call and must
    // not trip the agent loop's active execution policy. Held across the await.
    let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
    let result = vm.call_closure_pub(&closure, &[arg]).await;
    if let Some(ctx) = ctx {
        ctx.forward_output(&vm.take_output());
    }
    Ok(Some(result?))
}

async fn invoke_vm_lifecycle_hooks(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: HookEvent,
    registrations: Vec<VmLifecycleHookRegistration>,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
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
        // First-party registered lifecycle hook: the runtime chose to invoke
        // this closure, so its body's bridge/builtin calls are a trusted bridge
        // call and must not trip the agent loop's active execution policy. Held
        // across resolution and the invocation await for this registration.
        let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
        record_hook_call(&session_id, event, &registration.handler_name, payload);
        let closure = resolve_lifecycle_handler(&mut vm, &registration.handler).await?;
        let raw = vm.call_closure_pub(&closure, &[arg.clone()]).await?;
        if let Some(ctx) = ctx {
            ctx.forward_output(&vm.take_output());
        }
        let effects = parse_hook_effects(event, &raw)?;
        record_hook_returned(
            &session_id,
            event,
            &registration.handler_name,
            &HookControl::Allow,
            &raw,
        );
        inject_hook_effects(session_id.as_str(), effects, Some(event))?;
    }
    Ok(())
}

fn reminder_error(context: &str, message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("{context}: {}", message.into()))
}

fn reminder_code_error(context: &str, code: Code, message: impl Into<String>) -> VmError {
    reminder_error(context, format!("{}: {}", code.as_str(), message.into()))
}

fn unsupported_reminder_event_error(event: HookEvent, context: &str) -> VmError {
    reminder_code_error(
        context,
        Code::ReminderUnsupportedHookEvent,
        format!(
            "{} does not support reminder effects; use a session, tool, step, or persona hook",
            event.as_str()
        ),
    )
}

fn required_reminder_spec_string(
    options: &crate::value::DictMap,
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
    options: &crate::value::DictMap,
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
    options: &crate::value::DictMap,
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
    options: &crate::value::DictMap,
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
    options: &crate::value::DictMap,
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
    options: &crate::value::DictMap,
    context: &str,
) -> Result<Option<ReminderPropagate>, VmError> {
    optional_reminder_spec_string(options, "propagate", context)?
        .map(|value| match value.as_str() {
            "all" => Ok(ReminderPropagate::All),
            "session" => Ok(ReminderPropagate::Session),
            "none" => Ok(ReminderPropagate::None),
            _ => Err(reminder_code_error(
                context,
                Code::ReminderUnknownPropagate,
                "`propagate` must be one of all, session, or none",
            )),
        })
        .transpose()
}

fn optional_reminder_spec_role_hint(
    options: &crate::value::DictMap,
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
        .map(|key| key.as_str())
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
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

fn looks_like_reminder_spec(map: &crate::value::DictMap) -> bool {
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
            if !event.supports_reminder_effects() {
                return Err(unsupported_reminder_event_error(event, &context));
            }
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
            if !event.supports_reminder_effects() {
                return Err(unsupported_reminder_event_error(event, &context));
            }
            let spec = map
                .get("spec")
                .or_else(|| map.get("reminder"))
                .ok_or_else(|| reminder_error(&context, "reminder effect missing `spec`"))?;
            return Ok(HookEffect::Reminder(parse_reminder_spec(spec, &context)?));
        }
        if looks_like_reminder_spec(map) {
            if !event.supports_reminder_effects() {
                return Err(unsupported_reminder_event_error(event, &context));
            }
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
        if !event.supports_reminder_effects() {
            return Err(unsupported_reminder_event_error(event, &context));
        }
        effects.push(HookEffect::Reminder(parse_reminder_spec(
            reminder, &context,
        )?));
    } else if effects.is_empty() && looks_like_reminder_spec(map) {
        let context = format!("{} hook reminder", event.as_str());
        if !event.supports_reminder_effects() {
            return Err(unsupported_reminder_event_error(event, &context));
        }
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
        VmValue::dict(action)
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

fn inject_hook_effects(
    session_id: &str,
    effects: Vec<HookEffect>,
    event: Option<HookEvent>,
) -> Result<(), VmError> {
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
                let reminder_id = spec.id.clone();
                let tags = spec.tags.clone();
                let dedupe_key = spec.dedupe_key.clone();
                let role_hint = spec.role_hint.as_str();
                let source = spec.source.as_str();
                let ttl_turns = spec.ttl_turns;
                let report = crate::agent_sessions::inject_reminder(&target_session, spec)
                    .map_err(VmError::Runtime)?;
                record_hook_reminder_report(serde_json::json!({
                    "hook_event": event.map(|event| event.as_str()),
                    "session_id": &target_session,
                    "tool_call_id": crate::agent_sessions::current_tool_call_id(),
                    "reminder_id": reminder_id,
                    "tags": tags,
                    "dedupe_key": dedupe_key,
                    "role_hint": role_hint,
                    "source": source,
                    "ttl_turns": ttl_turns,
                    "deduped_count": report.deduped_count,
                }));
            }
        }
    }
    Ok(())
}

pub fn inject_hook_effects_into_current_session(effects: Vec<HookEffect>) -> Result<(), VmError> {
    inject_hook_effects("", effects, None)
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
            inject_hook_effects(
                "",
                vec![HookEffect::Reminder(spec)],
                Some(HookEvent::PreToolUse),
            )?;
            apply_pre_tool_action(*then, current_args)
        }
    }
}

fn apply_post_tool_action(action: PostToolAction, current: String) -> Result<String, VmError> {
    match action {
        PostToolAction::Pass => Ok(current),
        PostToolAction::Modify(new_result) => Ok(new_result),
        PostToolAction::Reminder { spec, then } => {
            inject_hook_effects(
                "",
                vec![HookEffect::Reminder(spec)],
                Some(HookEvent::PostToolUse),
            )?;
            apply_post_tool_action(*then, current)
        }
    }
}

/// Run all matching PreToolUse hooks. Returns the final action.
pub async fn run_pre_tool_hooks(
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<PreToolAction, VmError> {
    run_pre_tool_hooks_with_ctx(None, tool_name, args).await
}

pub async fn run_pre_tool_hooks_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<PreToolAction, VmError> {
    let hooks = runtime_hooks_for_event(HookEvent::PreToolUse);
    let mut current_args = args.clone();
    // Singleton runtime hook (currently the stdlib path_scope_guard) runs
    // before user-registered hooks so a tagged deny lands in the
    // PostToolUse / reminder path before any other hook fires.
    if let Some(singleton) = singleton_pre_tool_hook() {
        let action = singleton(tool_name, &current_args);
        if let Some(reason) = apply_pre_tool_action(action, &mut current_args)? {
            return Ok(PreToolAction::Deny(reason));
        }
    }
    for hook in &hooks {
        let payload = if matches!(hook.matcher, PatternMatcher::EventExpression { .. }) {
            Some(serde_json::json!({
                "event": HookEvent::PreToolUse.as_str(),
                "tool": {
                    "name": tool_name,
                    "args": current_args.clone(),
                    "tool_call_id": crate::agent_sessions::current_tool_call_id(),
                },
                "tool_call_id": crate::agent_sessions::current_tool_call_id(),
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
            RuntimeHookHandler::Vm { .. } | RuntimeHookHandler::LazyVm { .. } => {
                let payload = payload.as_ref().ok_or_else(|| {
                    VmError::Runtime("VM PreToolUse hook requires an event payload".to_string())
                })?;
                let Some(value) = invoke_vm_hook_handler(ctx, &hook.handler, payload).await? else {
                    continue;
                };
                parse_pre_tool_result(value)?
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
    run_post_tool_hooks_with_ctx(None, tool_name, args, result).await
}

pub async fn run_post_tool_hooks_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
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
                    "tool_call_id": crate::agent_sessions::current_tool_call_id(),
                },
                "tool_call_id": crate::agent_sessions::current_tool_call_id(),
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
            RuntimeHookHandler::Vm { .. } | RuntimeHookHandler::LazyVm { .. } => {
                let payload = payload.as_ref().ok_or_else(|| {
                    VmError::Runtime("VM PostToolUse hook requires an event payload".to_string())
                })?;
                let Some(value) = invoke_vm_hook_handler(ctx, &hook.handler, payload).await? else {
                    continue;
                };
                parse_post_tool_result(value)?
            }
            RuntimeHookHandler::NativePreTool(_) => continue,
        };
        match action {
            PostToolAction::Pass => {}
            PostToolAction::Modify(new_result) => {
                current = new_result;
            }
            PostToolAction::Reminder { spec, then } => {
                inject_hook_effects(
                    "",
                    vec![HookEffect::Reminder(spec)],
                    Some(HookEvent::PostToolUse),
                )?;
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
    run_lifecycle_hooks_with_ctx(None, event, payload).await
}

pub async fn run_lifecycle_hooks_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<(), VmError> {
    let registrations = matching_vm_lifecycle_registrations(event, payload);
    if registrations.is_empty() {
        return Ok(());
    }
    invoke_vm_lifecycle_hooks(ctx, event, registrations, payload).await
}

/// Run veto-capable session-level lifecycle hooks. Successive hooks see
/// `Allow`; the first non-`Allow` return short-circuits and is returned
/// to the caller. Hook invocations and decisions are captured on the
/// active session's transcript under `hook_call`, `hook_returned`, and
/// `hook_vetoed` so a replay reproduces the same control flow.
///
/// `Modify` does not short-circuit: subsequent hooks see the rewritten
/// payload, and the final `HookControl::Modify` returned by the chain
/// carries the merged payload back to the dispatcher so the recording
/// layer captures the post-modify shape (replay determinism). If a
/// later hook in the same chain returns `Allow`, the merged
/// `Modify { payload }` from earlier hooks is still surfaced.
pub async fn run_lifecycle_hooks_with_control(
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<HookControl, VmError> {
    run_lifecycle_hooks_with_control_with_ctx(None, event, payload).await
}

pub async fn run_lifecycle_hooks_with_control_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    event: HookEvent,
    payload: &serde_json::Value,
) -> Result<HookControl, VmError> {
    let registrations = matching_vm_lifecycle_registrations(event, payload);
    if registrations.is_empty() {
        return Ok(HookControl::Allow);
    }
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        return Err(VmError::Runtime(
            "session lifecycle hook requires an async builtin VM context".to_string(),
        ));
    };
    let session_id = payload
        .get("session")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut current_payload = payload.clone();
    let mut accumulated_modify: Option<serde_json::Value> = None;
    for registration in registrations {
        // First-party registered lifecycle hook (see `invoke_vm_lifecycle_hooks`):
        // the runtime chose to invoke this closure, so its body's bridge/builtin
        // calls are a trusted bridge call and must not trip the agent loop's
        // active execution policy. Held across the invocation await.
        let _trusted_bridge_guard = crate::orchestration::allow_trusted_bridge_calls();
        let arg = crate::stdlib::json_to_vm_value(&current_payload);
        record_hook_call(
            &session_id,
            event,
            &registration.handler_name,
            &current_payload,
        );
        let closure = resolve_lifecycle_handler(&mut vm, &registration.handler).await?;
        let raw = vm.call_closure_pub(&closure, &[arg]).await?;
        if let Some(ctx) = ctx {
            ctx.forward_output(&vm.take_output());
        }
        let outcome = parse_hook_outcome(event, &raw)?;
        record_hook_returned(
            &session_id,
            event,
            &registration.handler_name,
            &outcome.control,
            &raw,
        );
        inject_hook_effects(session_id.as_str(), outcome.effects, Some(event))?;
        match outcome.control {
            HookControl::Allow => continue,
            HookControl::Modify { payload: modified } => {
                current_payload = modified.clone();
                accumulated_modify = Some(modified);
            }
            other @ (HookControl::Block { .. } | HookControl::Decision { .. }) => {
                record_hook_vetoed(&session_id, event, &registration.handler_name, &other);
                return Ok(other);
            }
        }
    }
    if let Some(payload) = accumulated_modify {
        Ok(HookControl::Modify { payload })
    } else {
        Ok(HookControl::Allow)
    }
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

/// Public alias for the internal `parse_hook_control`. Used by the
/// pipeline-finish dispatcher (`fire_finish_lifecycle_event`) to
/// translate the action half of a hook return value into a control
/// signal so it can honor the lifecycle table (PreFinish rejects
/// Block, OnUnsettledDetected respects Block, etc.).
pub fn parse_hook_control_for_finish(
    event: HookEvent,
    value: &VmValue,
) -> Result<HookControl, VmError> {
    parse_hook_control(event, value)
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
            if let Some(modify) = map.get("modify") {
                return Ok(HookControl::Modify {
                    payload: crate::llm::vm_value_to_json(modify),
                });
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
        HookControl::Modify { .. } => return,
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
        .map(|registration| match registration.handler {
            VmLifecycleHandlerRef::Eager(closure) => VmLifecycleHookInvocation {
                closure: Some(closure),
                lazy: None,
                handler_name: registration.handler_name,
            },
            VmLifecycleHandlerRef::Lazy(lazy) => VmLifecycleHookInvocation {
                closure: None,
                lazy: Some(lazy),
                handler_name: registration.handler_name,
            },
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
                    handler: VmLifecycleHandlerRef::Eager(Arc::clone(closure)),
                }),
                RuntimeHookHandler::LazyVm { lazy, handler_name } => {
                    Some(VmLifecycleHookRegistration {
                        handler_name: handler_name.clone(),
                        handler: VmLifecycleHandlerRef::Lazy(lazy.clone()),
                    })
                }
                RuntimeHookHandler::NativePreTool(_) | RuntimeHookHandler::NativePostTool(_) => {
                    None
                }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_string(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (crate::value::intern_key(key), value))
                .collect::<crate::value::DictMap>(),
        )
    }

    fn error_message(result: Result<Vec<HookEffect>, VmError>) -> String {
        match result.expect_err("expected hook reminder parse error") {
            VmError::Runtime(message) => message,
            other => panic!("expected runtime error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_reminder_option_reports_code() {
        let value = dict(vec![(
            "reminder",
            dict(vec![
                ("body", vm_string("remember this")),
                ("typo_key", VmValue::Bool(true)),
            ]),
        )]);
        let message = error_message(parse_hook_effects(HookEvent::PostTurn, &value));
        assert!(message.contains(Code::ReminderUnknownOption.as_str()));
        assert!(message.contains("typo_key"), "{message}");
    }

    #[test]
    fn unknown_reminder_propagate_reports_specific_code() {
        let value = dict(vec![(
            "reminder",
            dict(vec![
                ("body", vm_string("remember this")),
                ("propagate", vm_string("workspace")),
            ]),
        )]);
        let message = error_message(parse_hook_effects(HookEvent::PostTurn, &value));
        assert!(message.contains(Code::ReminderUnknownPropagate.as_str()));
        assert!(message.contains("propagate"), "{message}");
    }

    #[test]
    fn worker_events_reject_reminder_effects_with_specific_code() {
        let value = dict(vec![(
            "reminder",
            dict(vec![("body", vm_string("worker lifecycle"))]),
        )]);
        let message = error_message(parse_hook_effects(HookEvent::WorkerSpawned, &value));
        assert!(message.contains(Code::ReminderUnsupportedHookEvent.as_str()));
        assert!(message.contains("WorkerSpawned"), "{message}");
    }

    #[test]
    fn as_str_round_trips_through_serde() {
        // The macro relies on serde's default unit-variant encoding
        // (identifier = wire name) instead of a per-variant
        // `#[serde(rename)]`. Lock that contract so a future variant
        // can't drift by accident.
        for &event in HookEvent::ALL {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(json, format!("\"{}\"", event.as_str()));
            let parsed: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, event);
        }
    }

    #[test]
    fn parse_session_event_accepts_both_spellings_for_every_session_variant() {
        // The macro auto-derives snake_case from the PascalCase
        // identifier; this test guards against a future variant whose
        // name doesn't round-trip cleanly (e.g. unexpected punctuation).
        for &event in HookEvent::ALL.iter().filter(|e| e.is_session_lifecycle()) {
            let pascal = event.as_str();
            let mut snake = String::new();
            pascal_to_snake_buf(pascal, &mut snake);
            assert_eq!(
                HookEvent::parse_session_event(pascal).unwrap(),
                event,
                "PascalCase `{pascal}`",
            );
            assert_eq!(
                HookEvent::parse_session_event(&snake).unwrap(),
                event,
                "snake_case `{snake}`",
            );
        }
    }

    #[test]
    fn parse_session_event_rejects_non_session_variants() {
        // Tool, agent-turn, worker, step, and notification events must
        // not be accepted by the session parser — each surface owns
        // its own event set.
        for &event in HookEvent::ALL.iter().filter(|e| !e.is_session_lifecycle()) {
            let err = HookEvent::parse_session_event(event.as_str())
                .expect_err("non-session event slipped through");
            assert!(err.contains("unknown session hook event"), "{err}");
        }
    }

    #[test]
    fn parse_provider_event_accepts_worker_and_session_and_flagged_variants() {
        // Worker variants are accepted by kind, session variants by
        // the fallback, and explicitly-flagged variants
        // (`provider_parse: true`) by the first-pass loop. The whole
        // set should round-trip.
        for &event in HookEvent::ALL.iter().filter(|e| {
            matches!(e.kind(), HookEventKind::Worker | HookEventKind::Session)
                || e.in_provider_parse()
        }) {
            assert_eq!(
                HookEvent::parse_provider_event(event.as_str()).unwrap(),
                event,
                "{event:?}",
            );
        }
    }

    #[test]
    fn session_error_accepts_legacy_short_alias() {
        // `SessionError` carries an explicit `"error"` alias for
        // backward compat with the original event name.
        assert_eq!(
            HookEvent::parse_session_event("error").unwrap(),
            HookEvent::SessionError,
        );
        assert_eq!(
            HookEvent::parse_session_event("SessionError").unwrap(),
            HookEvent::SessionError,
        );
        assert_eq!(
            HookEvent::parse_session_event("session_error").unwrap(),
            HookEvent::SessionError,
        );
    }

    #[test]
    fn supports_reminder_effects_excludes_only_worker_kind() {
        for &event in HookEvent::ALL {
            let supports = event.supports_reminder_effects();
            let expected = !matches!(event.kind(), HookEventKind::Worker);
            assert_eq!(
                supports,
                expected,
                "{event:?} ({:?}) reminder support disagrees with kind",
                event.kind(),
            );
        }
    }

    #[test]
    fn from_worker_event_covers_every_worker_variant() {
        for worker in WorkerEvent::ALL {
            let event = HookEvent::from_worker_event(worker);
            assert!(
                matches!(event.kind(), HookEventKind::Worker),
                "WorkerEvent::{worker:?} mapped to non-Worker kind {:?}",
                event.kind(),
            );
            assert_eq!(event.as_str(), worker.as_str());
        }
    }
}
