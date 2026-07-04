//! JSON-RPC 2.0 bridge for host communication.
//!
//! When `harn run --bridge` is used, the VM delegates builtins (llm_call,
//! file I/O, tool execution) to a host process over stdin/stdout JSON-RPC.
//! The host application handles these requests using its own providers.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::sync::{oneshot, Mutex, Notify};

use harn_parser::diagnostic_codes::Code;

use crate::orchestration::MutationSessionRecord;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::visible_text::VisibleTextState;
use crate::vm::Vm;

/// Default timeout for bridge calls (5 minutes).
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

pub type HostBridgeWriter = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

fn stdout_writer(stdout_lock: Arc<std::sync::Mutex<()>>) -> HostBridgeWriter {
    Arc::new(move |line: &str| {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(line.as_bytes())
            .map_err(|e| format!("Bridge write error: {e}"))?;
        stdout
            .write_all(b"\n")
            .map_err(|e| format!("Bridge write error: {e}"))?;
        stdout
            .flush()
            .map_err(|e| format!("Bridge flush error: {e}"))?;
        Ok(())
    })
}

/// A JSON-RPC 2.0 bridge to a host process over stdin/stdout.
///
/// The bridge sends requests to the host on stdout and receives responses
/// on stdin. A background task reads stdin and dispatches responses to
/// waiting callers by request ID. All stdout writes are serialized through
/// a mutex to prevent interleaving.
pub struct HostBridge {
    next_id: AtomicU64,
    /// Pending request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Whether the host has sent a cancel notification.
    cancelled: Arc<AtomicBool>,
    /// Wakes pending host calls when cancellation arrives.
    cancel_notify: Arc<Notify>,
    /// Transport writer used to send JSON-RPC to the host.
    writer: HostBridgeWriter,
    /// ACP session ID (set in ACP mode for session-scoped notifications).
    session_id: std::sync::Mutex<String>,
    /// Name of the currently executing Harn script (without .harn suffix).
    script_name: std::sync::Mutex<String>,
    /// Transcript injections queued by the host while a run is active.
    queued_transcript_injections: HostBridgeInjectionState,
    /// Host-triggered resume signal for daemon agents.
    resume_requested: Arc<AtomicBool>,
    /// Host-triggered skill-registry invalidation signal. Set when the
    /// host sends a `skills/update` notification; consumed by the CLI
    /// between runs (watch mode, long-running agents) to rebuild the
    /// layered skill catalog from its current filesystem + host state.
    skills_reload_requested: Arc<AtomicBool>,
    /// Whether the current daemon-mode agent loop is blocked in idle wait.
    daemon_idle: Arc<AtomicBool>,
    /// Canonical ACP `stopReason` recorded by the most recent `agent_loop`
    /// finalize during this prompt. Read once by the ACP adapter when the
    /// pipeline returns and populated by `host_agent_session_finalize`.
    /// Pipelines that don't run an agent loop leave this `None`, in which
    /// case the adapter falls back to `end_turn`.
    prompt_stop_reason: std::sync::Mutex<Option<String>>,
    /// Per-call visible assistant text state for call_progress notifications.
    visible_call_states: std::sync::Mutex<HashMap<String, VisibleTextState>>,
    /// Whether an LLM call's deltas should be exposed to end users while streaming.
    visible_call_streams: std::sync::Mutex<HashMap<String, bool>>,
    /// Optional in-process host-module backend used by `harn playground`.
    in_process: Option<InProcessHost>,
}

struct InProcessHost {
    module_path: PathBuf,
    exported_functions: BTreeMap<String, Arc<VmClosure>>,
    vm: Vm,
}

impl InProcessHost {
    /// Box-pin'd to break the static recursion between the VM's hot dispatch
    /// loop and the bridge: a bridge-backed builtin spawns a child VM that
    /// calls back into the dispatch loop via `call_closure_pub`. Indirecting
    /// at this slow-path boundary keeps the recursion satisfied without
    /// allocating per call in the hot per-callback path.
    fn dispatch<'a>(
        &'a self,
        method: &'a str,
        params: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, VmError>> + Send + 'a>> {
        Box::pin(async move {
            match method {
                "builtin_call" => {
                    let name = params
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let args = params
                        .get("args")
                        .and_then(|value| value.as_array())
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|value| json_result_to_vm_value(&value))
                        .collect::<Vec<_>>();
                    self.invoke_export(name, &args).await
                }
                "host/tools/list" => self
                    .invoke_optional_export("host_tools_list", &[])
                    .await
                    .map(|value| value.unwrap_or_else(|| serde_json::json!({ "tools": [] }))),
                "session/request_permission" => self.request_permission(params).await,
                other => Err(VmError::Runtime(format!(
                    "playground host backend does not implement bridge method '{other}'"
                ))),
            }
        })
    }

    async fn invoke_export(
        &self,
        name: &str,
        args: &[VmValue],
    ) -> Result<serde_json::Value, VmError> {
        let Some(closure) = self.exported_functions.get(name) else {
            return Err(VmError::Runtime(format!(
                "Playground host is missing capability '{name}'. Define `pub fn {name}(...)` in {}",
                self.module_path.display()
            )));
        };

        let mut vm = self.vm.child_vm_for_host();
        let result = vm.call_closure_pub(closure, args).await?;
        Ok(crate::llm::vm_value_to_json(&result))
    }

    async fn invoke_optional_export(
        &self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<serde_json::Value>, VmError> {
        if !self.exported_functions.contains_key(name) {
            return Ok(None);
        }
        self.invoke_export(name, args).await.map(Some)
    }

    async fn request_permission(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        // No exported `request_permission` means the playground host has no
        // approval policy, so it grants through the canonical ACP option.
        let Some(closure) = self.exported_functions.get("request_permission") else {
            return Ok(crate::llm::acp_permission::allow_response());
        };

        let tool_call = params.get("toolCall");
        let tool_name = tool_call
            .and_then(|tool_call| tool_call.pointer("/_meta/harn/toolName"))
            .or_else(|| tool_call.and_then(|tool_call| tool_call.get("toolName")))
            .or_else(|| tool_call.and_then(|tool_call| tool_call.get("title")))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let tool_args = tool_call
            .and_then(|tool_call| tool_call.get("rawInput"))
            .map(json_result_to_vm_value)
            .unwrap_or(VmValue::Nil);
        let full_payload = json_result_to_vm_value(&params);

        let arg_count = closure.func.params.len();
        let args = if arg_count >= 3 {
            vec![
                VmValue::String(arcstr::ArcStr::from(tool_name.to_string())),
                tool_args,
                full_payload,
            ]
        } else if arg_count == 2 {
            vec![
                VmValue::String(arcstr::ArcStr::from(tool_name.to_string())),
                tool_args,
            ]
        } else if arg_count == 1 {
            vec![full_payload]
        } else {
            Vec::new()
        };

        let mut vm = self.vm.child_vm_for_host();
        let result = vm.call_closure_pub(closure, &args).await?;
        // Translate the script's verdict into a canonical ACP response
        // (`{ outcome: { outcome: "selected" | "cancelled", optionId? } }`).
        // The script API stays ergonomic — bool / string-reason / dict — but
        // the wire shape is canonical.
        let payload = match result {
            VmValue::Bool(granted) => {
                if granted {
                    crate::llm::acp_permission::allow_response()
                } else {
                    crate::llm::acp_permission::reject_response(None)
                }
            }
            VmValue::String(reason) if !reason.is_empty() => {
                crate::llm::acp_permission::reject_response(Some(reason.to_string()))
            }
            other => {
                let json = crate::llm::vm_value_to_json(&other);
                if let Some(granted) = json.get("granted").and_then(|value| value.as_bool()) {
                    if granted {
                        crate::llm::acp_permission::allow_response()
                    } else {
                        crate::llm::acp_permission::reject_response(
                            json.get("reason")
                                .and_then(|value| value.as_str())
                                .map(str::to_string),
                        )
                    }
                } else if json.get("outcome").is_some() {
                    // The script already returned a canonical-shaped outcome.
                    json
                } else if other.is_truthy() {
                    crate::llm::acp_permission::allow_response()
                } else {
                    crate::llm::acp_permission::reject_response(None)
                }
            }
        };
        Ok(payload)
    }
}

/// How a queued bridge injection is delivered into the agent loop.
///
/// `AuditOnly` injections drain at `loop_exit`, *after* the last LLM call has
/// returned, so they land in the transcript audit but are **never rendered into
/// a model prompt**.
/// Hosts that want the model to react to the reminder on its final
/// iteration should use `FinishStep` instead, which drains at every
/// `iteration_start` / `post_tool_dispatch` / `iteration_end` checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedUserMessageMode {
    InterruptImmediate,
    FinishStep,
    AuditOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryCheckpoint {
    InterruptImmediate,
    AfterCurrentOperation,
    EndOfInteraction,
}

impl QueuedUserMessageMode {
    fn from_str(value: &str) -> Self {
        match value {
            "interrupt_immediate" | "interrupt" => Self::InterruptImmediate,
            // `steer` is the ACP `session/inject` alias for mid-turn
            // user-message delivery at the next tool boundary; it maps to
            // the same `FinishStep` checkpoint as `finish_step`.
            "finish_step" | "after_current_operation" | "steer" => Self::FinishStep,
            // `queue` is the explicit ACP alias for the audit-only path.
            "queue" => Self::AuditOnly,
            // Unknown / missing modes fall through to the safest option:
            // record for audit, do not preempt the loop. Pre-#2212 hosts
            // that send `wait_for_completion` are caught by this arm —
            // the canonical name is `audit_only` going forward.
            _ => Self::AuditOnly,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InterruptImmediate => "interrupt_immediate",
            Self::FinishStep => "finish_step",
            Self::AuditOnly => "audit_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedUserMessage {
    pub message_id: String,
    pub content: String,
    pub transcript_content: serde_json::Value,
    pub mode: QueuedUserMessageMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedReminder {
    pub reminder: crate::llm::helpers::SystemReminder,
    pub mode: QueuedUserMessageMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueuedTranscriptInjection {
    User(QueuedUserMessage),
    Reminder(QueuedReminder),
}

#[derive(Debug, Default)]
struct QueuedTranscriptInjections {
    queue: VecDeque<QueuedTranscriptInjection>,
    revoked_user_message_ids: HashSet<String>,
    delivered_user_message_ids: HashSet<String>,
    revoked_reminder_ids: HashSet<String>,
    delivered_reminder_ids: HashSet<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HostBridgeInjectionState {
    inner: Arc<Mutex<QueuedTranscriptInjections>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingUserMessageMutationResult {
    Mutated,
    AlreadyRevoked,
    AlreadyDelivered,
    UnknownMessageId,
}

impl QueuedTranscriptInjection {
    fn mode(&self) -> QueuedUserMessageMode {
        match self {
            Self::User(message) => message.mode,
            Self::Reminder(reminder) => reminder.mode,
        }
    }

    fn pending_json(&self, position: usize) -> serde_json::Value {
        match self {
            Self::User(message) => serde_json::json!({
                "kind": "user",
                "id": message.message_id,
                "messageId": message.message_id,
                "mode": message.mode.as_str(),
                "position": position,
                "content": message.transcript_content,
            }),
            Self::Reminder(reminder) => serde_json::json!({
                "kind": "reminder",
                "id": reminder.reminder.id,
                "reminderId": reminder.reminder.id,
                "mode": reminder.mode.as_str(),
                "position": position,
                "body": reminder.reminder.body,
                "tags": reminder.reminder.tags,
                "dedupeKey": reminder.reminder.dedupe_key,
                "ttlTurns": reminder.reminder.ttl_turns,
                "preserveOnCompact": reminder.reminder.preserve_on_compact,
                "propagate": reminder.reminder.propagate.as_str(),
                "roleHint": reminder.reminder.role_hint.as_str(),
                "source": reminder.reminder.source.as_str(),
                "firedAtTurn": reminder.reminder.fired_at_turn,
                "originatingAgentId": reminder.reminder.originating_agent_id,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingReminderMutationResult {
    Mutated,
    AlreadyRevoked,
    AlreadyDelivered,
    UnknownReminderId,
}

fn new_inject_message_id() -> String {
    format!("msg_inj_{}", uuid::Uuid::now_v7().simple())
}

impl HostBridgeInjectionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn push_pending_user_message(
        &self,
        content: String,
        transcript_content: serde_json::Value,
        mode: &str,
    ) -> String {
        let message_id = new_inject_message_id();
        self.inner
            .lock()
            .await
            .queue
            .push_back(QueuedTranscriptInjection::User(QueuedUserMessage {
                message_id: message_id.clone(),
                content,
                transcript_content,
                mode: QueuedUserMessageMode::from_str(mode),
            }));
        message_id
    }

    pub async fn revoke_pending_user_message(
        &self,
        message_id: &str,
    ) -> PendingUserMessageMutationResult {
        let mut state = self.inner.lock().await;
        let mut retained = VecDeque::new();
        let mut revoked = false;
        while let Some(injection) = state.queue.pop_front() {
            match &injection {
                QueuedTranscriptInjection::User(message) if message.message_id == message_id => {
                    revoked = true;
                }
                _ => retained.push_back(injection),
            }
        }
        state.queue = retained;
        if revoked {
            state
                .revoked_user_message_ids
                .insert(message_id.to_string());
            return PendingUserMessageMutationResult::Mutated;
        }
        if state.revoked_user_message_ids.contains(message_id) {
            PendingUserMessageMutationResult::AlreadyRevoked
        } else if state.delivered_user_message_ids.contains(message_id) {
            PendingUserMessageMutationResult::AlreadyDelivered
        } else {
            PendingUserMessageMutationResult::UnknownMessageId
        }
    }

    pub async fn revoke_pending_reminder(
        &self,
        reminder_id: &str,
    ) -> PendingReminderMutationResult {
        let mut state = self.inner.lock().await;
        let mut retained = VecDeque::new();
        let mut revoked = false;
        while let Some(injection) = state.queue.pop_front() {
            match &injection {
                QueuedTranscriptInjection::Reminder(reminder)
                    if reminder.reminder.id == reminder_id =>
                {
                    revoked = true;
                }
                _ => retained.push_back(injection),
            }
        }
        state.queue = retained;
        if revoked {
            state.revoked_reminder_ids.insert(reminder_id.to_string());
            return PendingReminderMutationResult::Mutated;
        }
        if state.revoked_reminder_ids.contains(reminder_id) {
            PendingReminderMutationResult::AlreadyRevoked
        } else if state.delivered_reminder_ids.contains(reminder_id) {
            PendingReminderMutationResult::AlreadyDelivered
        } else {
            PendingReminderMutationResult::UnknownReminderId
        }
    }

    pub async fn replace_pending_user_message(
        &self,
        message_id: &str,
        content: String,
        transcript_content: serde_json::Value,
    ) -> PendingUserMessageMutationResult {
        let mut state = self.inner.lock().await;
        for injection in &mut state.queue {
            if let QueuedTranscriptInjection::User(message) = injection {
                if message.message_id == message_id {
                    message.content = content;
                    message.transcript_content = transcript_content;
                    return PendingUserMessageMutationResult::Mutated;
                }
            }
        }
        if state.revoked_user_message_ids.contains(message_id) {
            PendingUserMessageMutationResult::AlreadyRevoked
        } else if state.delivered_user_message_ids.contains(message_id) {
            PendingUserMessageMutationResult::AlreadyDelivered
        } else {
            PendingUserMessageMutationResult::UnknownMessageId
        }
    }

    async fn push_session_reminder(&self, reminder: QueuedReminder) {
        self.inner
            .lock()
            .await
            .queue
            .push_back(QueuedTranscriptInjection::Reminder(reminder));
    }

    pub async fn pending_injections_json(&self) -> serde_json::Value {
        let state = self.inner.lock().await;
        let injections = state
            .queue
            .iter()
            .enumerate()
            .map(|(position, injection)| injection.pending_json(position))
            .collect::<Vec<_>>();
        serde_json::json!({
            "pendingCount": injections.len(),
            "injections": injections,
        })
    }
}

fn reminder_unknown_option_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderUnknownOption.as_str(),
        message.as_ref()
    )
}

fn session_remind_shape_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderInvalidShape.as_str(),
        message.as_ref()
    )
}

fn reminder_unknown_propagate_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderUnknownPropagate.as_str(),
        message.as_ref()
    )
}

fn string_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    required: bool,
) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) if required => Err(session_remind_shape_error(
            format!("`{key}` must be a non-empty string"),
        )),
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if required && value.trim().is_empty() => Err(
            session_remind_shape_error(format!("`{key}` must be a non-empty string")),
        ),
        Some(serde_json::Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be a string, got {other}"
        ))),
    }
}

fn bool_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<bool>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be a bool, got {other}"
        ))),
    }
}

fn int_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => {
            let Some(value) = value.as_i64() else {
                return Err(session_remind_shape_error(format!(
                    "`{key}` must be an integer"
                )));
            };
            Ok(Some(value))
        }
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be an int, got {other}"
        ))),
    }
}

fn tags_field(map: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<String>, String> {
    let Some(value) = map.get("tags") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(values) = value.as_array() else {
        return Err(session_remind_shape_error("`tags` must be a list"));
    };
    let mut tags = Vec::new();
    for value in values {
        let Some(tag) = value.as_str() else {
            return Err(session_remind_shape_error(format!(
                "`tags` entries must be strings, got {value}"
            )));
        };
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(session_remind_shape_error(
                "`tags` entries must be non-empty strings",
            ));
        }
        if !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    Ok(tags)
}

fn session_remind_payload_from_value(
    value: &serde_json::Value,
) -> Result<crate::llm::helpers::SystemReminder, String> {
    let Some(map) = value.as_object() else {
        return Err(session_remind_shape_error(
            "session/remind payload must be a reminder object",
        ));
    };
    const ALLOWED: &[&str] = &[
        "_meta",
        "body",
        "dedupe_key",
        "fired_at_turn",
        "id",
        "preserve_on_compact",
        "propagate",
        "role_hint",
        "source",
        "tags",
        "ttl_turns",
    ];
    let unknown = map
        .keys()
        .filter(|key| !ALLOWED.contains(&key.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        if unknown.contains(&"content") {
            return Err(session_remind_shape_error(
                "session/remind expects reminder `body`, not user-message `content`",
            ));
        }
        return Err(reminder_unknown_option_error(format!(
            "unknown reminder option(s): {}",
            unknown.join(", ")
        )));
    }
    if let Some(meta) = map.get("_meta") {
        if !meta.is_null() && !meta.is_object() {
            return Err(session_remind_shape_error("`_meta` must be an object"));
        }
    }
    let ttl_turns = int_field(map, "ttl_turns")?;
    if let Some(value) = ttl_turns {
        if value <= 0 {
            return Err(session_remind_shape_error("`ttl_turns` must be > 0"));
        }
    }
    let fired_at_turn = int_field(map, "fired_at_turn")?.unwrap_or(0);
    if fired_at_turn < 0 {
        return Err(session_remind_shape_error(
            "`fired_at_turn` must be >= 0 when provided",
        ));
    }
    match string_field(map, "source", false)?.as_deref() {
        None | Some("bridge") => {}
        Some(_) => {
            return Err(session_remind_shape_error(
                "`source` for session/remind must be bridge when provided",
            ))
        }
    }
    let propagate = match string_field(map, "propagate", false)?.as_deref() {
        None => crate::llm::helpers::ReminderPropagate::Session,
        Some("all") => crate::llm::helpers::ReminderPropagate::All,
        Some("session") => crate::llm::helpers::ReminderPropagate::Session,
        Some("none") => crate::llm::helpers::ReminderPropagate::None,
        Some(_) => {
            return Err(reminder_unknown_propagate_error(
                "`propagate` must be one of all, session, or none",
            ))
        }
    };
    let role_hint = match string_field(map, "role_hint", false)?.as_deref() {
        None => crate::llm::helpers::ReminderRoleHint::System,
        Some("system") => crate::llm::helpers::ReminderRoleHint::System,
        Some("developer") => crate::llm::helpers::ReminderRoleHint::Developer,
        Some("user_block") => crate::llm::helpers::ReminderRoleHint::UserBlock,
        Some("ephemeral_cache") => crate::llm::helpers::ReminderRoleHint::EphemeralCache,
        Some(_) => {
            return Err(session_remind_shape_error(
                "`role_hint` must be one of system, developer, user_block, or ephemeral_cache",
            ))
        }
    };
    Ok(crate::llm::helpers::SystemReminder {
        id: string_field(map, "id", false)?.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        tags: tags_field(map)?,
        dedupe_key: string_field(map, "dedupe_key", false)?,
        ttl_turns,
        preserve_on_compact: bool_field(map, "preserve_on_compact")?.unwrap_or(false),
        propagate,
        role_hint,
        source: crate::llm::helpers::ReminderSource::Bridge,
        body: string_field(map, "body", true)?.unwrap_or_default(),
        fired_at_turn,
        originating_agent_id: None,
    })
}

/// Parse the params of a `session/cancel_tool_call` notification and fire
/// the per-tool-call cancellation. Mirrors the shape used by the public
/// `cancel_in_flight_tool_call` builtin so hosts have one wire format
/// regardless of which surface they came through.
///
/// Stdio bridges send this as a notification (no id, no response); the
/// builtin handles request/response semantics in Harn. We deliberately
/// drop malformed payloads silently because notifications can't reply
/// with an error — logging would also be too noisy for partial drops.
fn handle_cancel_tool_call_notification(params: &serde_json::Value) {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let call_id = params
        .get("toolCallId")
        .or_else(|| params.get("tool_call_id"))
        .or_else(|| params.get("callId"))
        .or_else(|| params.get("call_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if call_id.is_empty() {
        return;
    }
    let reason = params
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("host cancelled in-flight tool call")
        .to_string();
    let inject_reminder = params
        .get("injectReminder")
        .or_else(|| params.get("inject_reminder"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    crate::tool_call_cancellations::cancel(session_id, call_id, reason, inject_reminder);
}

fn queued_session_remind_from_params(params: &serde_json::Value) -> Result<QueuedReminder, String> {
    let mode = QueuedUserMessageMode::from_str(
        params
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("audit_only"),
    );
    let reminder_value = if let Some(reminder) = params.get("reminder") {
        reminder.clone()
    } else {
        let Some(params) = params.as_object() else {
            return Err(session_remind_shape_error(
                "session/remind params must be an object",
            ));
        };
        let mut reminder = params.clone();
        reminder.remove("mode");
        reminder.remove("sessionId");
        reminder.remove("session_id");
        serde_json::Value::Object(reminder)
    };
    Ok(QueuedReminder {
        reminder: session_remind_payload_from_value(&reminder_value)?,
        mode,
    })
}

// Default doesn't apply — new() spawns async tasks requiring a tokio LocalSet.
#[allow(clippy::new_without_default)]
impl HostBridge {
    /// Create a new bridge and spawn the stdin reader task.
    ///
    /// Must be called within a tokio LocalSet (uses spawn_local for the
    /// stdin reader since it's single-threaded).
    pub fn new() -> Self {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(Notify::new());
        let queued_transcript_injections = HostBridgeInjectionState::default();
        let resume_requested = Arc::new(AtomicBool::new(false));
        let skills_reload_requested = Arc::new(AtomicBool::new(false));
        let daemon_idle = Arc::new(AtomicBool::new(false));

        // Stdin reader: reads JSON-RPC lines and dispatches responses
        let pending_clone = pending.clone();
        let cancelled_clone = cancelled.clone();
        let cancel_notify_clone = cancel_notify.clone();
        let queued_clone = queued_transcript_injections.clone();
        let resume_clone = resume_requested.clone();
        let skills_reload_clone = skills_reload_requested.clone();
        tokio::task::spawn_local(async move {
            let stdin = tokio::io::stdin();
            let reader = tokio::io::BufReader::new(stdin);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let msg: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Notifications have no id; responses have one.
                if msg.get("id").is_none() {
                    if let Some(method) = msg["method"].as_str() {
                        if method == "cancel" {
                            cancelled_clone.store(true, Ordering::SeqCst);
                            cancel_notify_clone.notify_waiters();
                        } else if method == "agent/resume" {
                            resume_clone.store(true, Ordering::SeqCst);
                        } else if method == "skills/update" {
                            skills_reload_clone.store(true, Ordering::SeqCst);
                        } else if method == "session/remind" {
                            let params = &msg["params"];
                            if let Ok(reminder) = queued_session_remind_from_params(params) {
                                queued_clone.push_session_reminder(reminder).await;
                            }
                        } else if method == "session/cancel_tool_call" {
                            handle_cancel_tool_call_notification(&msg["params"]);
                        }
                    }
                    continue;
                }

                if let Some(id) = msg["id"].as_u64() {
                    let mut pending = pending_clone.lock().await;
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(msg);
                    }
                }
            }

            // stdin closed: drop pending senders to cancel waiters.
            let mut pending = pending_clone.lock().await;
            pending.clear();
        });

        Self {
            next_id: AtomicU64::new(1),
            pending,
            cancelled,
            cancel_notify,
            writer: stdout_writer(Arc::new(std::sync::Mutex::new(()))),
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_transcript_injections,
            resume_requested,
            skills_reload_requested,
            daemon_idle,
            prompt_stop_reason: std::sync::Mutex::new(None),
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: None,
        }
    }

    /// Create a bridge from pre-existing shared state.
    ///
    /// Unlike `new()`, does **not** spawn a stdin reader — the caller is
    /// responsible for dispatching responses into `pending`.  This is used
    /// by ACP mode which already has its own stdin reader.
    pub fn from_parts(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        stdout_lock: Arc<std::sync::Mutex<()>>,
        start_id: u64,
    ) -> Self {
        Self::from_parts_with_writer(pending, cancelled, stdout_writer(stdout_lock), start_id)
    }

    pub fn from_parts_with_writer(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        writer: HostBridgeWriter,
        start_id: u64,
    ) -> Self {
        Self::from_parts_with_writer_and_cancel_notify(
            pending,
            cancelled,
            Arc::new(Notify::new()),
            writer,
            start_id,
        )
    }

    pub fn from_parts_with_writer_and_cancel_notify(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        writer: HostBridgeWriter,
        start_id: u64,
    ) -> Self {
        Self::from_parts_with_writer_cancel_notify_and_injection_state(
            pending,
            cancelled,
            cancel_notify,
            writer,
            start_id,
            None,
        )
    }

    pub fn from_parts_with_writer_cancel_notify_and_injection_state(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        writer: HostBridgeWriter,
        start_id: u64,
        injection_state: Option<HostBridgeInjectionState>,
    ) -> Self {
        Self {
            next_id: AtomicU64::new(start_id),
            pending,
            cancelled,
            cancel_notify,
            writer,
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_transcript_injections: injection_state.unwrap_or_default(),
            resume_requested: Arc::new(AtomicBool::new(false)),
            skills_reload_requested: Arc::new(AtomicBool::new(false)),
            daemon_idle: Arc::new(AtomicBool::new(false)),
            prompt_stop_reason: std::sync::Mutex::new(None),
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: None,
        }
    }

    /// Create an in-process host bridge backed by exported functions from a
    /// Harn module. Used by `harn playground` to avoid JSON-RPC boilerplate.
    pub async fn from_harn_module(mut vm: Vm, module_path: &Path) -> Result<Self, VmError> {
        let exported_functions = vm.load_module_exports(module_path).await?;
        Ok(Self {
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_notify: Arc::new(Notify::new()),
            writer: stdout_writer(Arc::new(std::sync::Mutex::new(()))),
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_transcript_injections: HostBridgeInjectionState::default(),
            resume_requested: Arc::new(AtomicBool::new(false)),
            skills_reload_requested: Arc::new(AtomicBool::new(false)),
            daemon_idle: Arc::new(AtomicBool::new(false)),
            prompt_stop_reason: std::sync::Mutex::new(None),
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: Some(InProcessHost {
                module_path: module_path.to_path_buf(),
                exported_functions,
                vm,
            }),
        })
    }

    /// Set the ACP session ID for session-scoped notifications.
    pub fn set_session_id(&self, id: &str) {
        *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
    }

    /// Set the currently executing script name (without .harn suffix).
    pub fn set_script_name(&self, name: &str) {
        *self.script_name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Get the current script name.
    fn get_script_name(&self) -> String {
        self.script_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Get the session ID.
    pub fn get_session_id(&self) -> String {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Write a complete JSON-RPC line to stdout, serialized through a mutex.
    fn write_line(&self, line: &str) -> Result<(), VmError> {
        (self.writer)(line).map_err(VmError::Runtime)
    }

    /// Send a JSON-RPC request to the host and wait for the response.
    /// Times out after 5 minutes to prevent deadlocks.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        if let Some(in_process) = &self.in_process {
            return in_process.dispatch(method, params).await;
        }

        if self.is_cancelled() {
            return Err(VmError::Runtime("Bridge: operation cancelled".into()));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cancel_wait = self.cancel_notify.notified();
        tokio::pin!(cancel_wait);

        let request = crate::jsonrpc::request(id, method, params);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        let line = serde_json::to_string(&request)
            .map_err(|e| VmError::Runtime(format!("Bridge serialization error: {e}")))?;
        if let Err(e) = self.write_line(&line) {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            return Err(e);
        }

        if self.is_cancelled() {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            return Err(VmError::Runtime("Bridge: operation cancelled".into()));
        }

        let response = tokio::select! {
            result = rx => match result {
                Ok(msg) => msg,
                Err(_) => {
                    // Sender dropped: host closed or stdin reader exited.
                    return Err(VmError::Runtime(
                        "Bridge: host closed connection before responding".into(),
                    ));
                }
            },
            _ = &mut cancel_wait => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(VmError::Runtime("Bridge: operation cancelled".into()));
            }
            _ = tokio::time::sleep(DEFAULT_TIMEOUT) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(VmError::Runtime(format!(
                    "Bridge: host did not respond to '{method}' within {}s",
                    DEFAULT_TIMEOUT.as_secs()
                )));
            }
        };

        if let Some(error) = response.get("error") {
            let message = error["message"].as_str().unwrap_or("Unknown host error");
            let code = error["code"].as_i64().unwrap_or(-1);
            // JSON-RPC -32001 signals the host rejected the tool (not permitted / not in allowlist).
            if code == -32001 {
                return Err(VmError::CategorizedError {
                    message: message.to_string(),
                    category: ErrorCategory::ToolRejected,
                });
            }
            return Err(VmError::Runtime(format!("Host error ({code}): {message}")));
        }

        Ok(response["result"].clone())
    }

    /// Send a JSON-RPC notification to the host (no response expected).
    /// Serialized through the stdout mutex to prevent interleaving.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        let notification = crate::jsonrpc::notification(method, params);
        if self.in_process.is_some() {
            return;
        }
        if let Ok(line) = serde_json::to_string(&notification) {
            let _ = self.write_line(&line);
        }
    }

    /// Check if the host has sent a cancel notification.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn take_resume_signal(&self) -> bool {
        self.resume_requested.swap(false, Ordering::SeqCst)
    }

    pub fn signal_resume(&self) {
        self.resume_requested.store(true, Ordering::SeqCst);
    }

    pub fn set_daemon_idle(&self, idle: bool) {
        self.daemon_idle.store(idle, Ordering::SeqCst);
    }

    pub fn is_daemon_idle(&self) -> bool {
        self.daemon_idle.load(Ordering::SeqCst)
    }

    /// Record the canonical ACP `stopReason` for the current prompt. The
    /// last writer wins, which matches the semantic that an outer
    /// `agent_loop` (the one whose result the user observes) always
    /// finalizes after any inner loops it spawned.
    pub fn set_prompt_stop_reason(&self, reason: &str) {
        *self
            .prompt_stop_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(reason.to_string());
    }

    /// Consume any prompt stop reason recorded during this prompt. The
    /// ACP adapter calls this once after the pipeline returns; pipelines
    /// that didn't run an `agent_loop` see `None` and the adapter falls
    /// back to `end_turn`.
    pub fn take_prompt_stop_reason(&self) -> Option<String> {
        self.prompt_stop_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Consume any pending `skills/update` signal the host has sent.
    /// Returns `true` exactly once per notification, letting callers
    /// trigger a layered-discovery rebuild without polling false
    /// positives. See issue #73 for the hot-reload contract.
    pub fn take_skills_reload_signal(&self) -> bool {
        self.skills_reload_requested.swap(false, Ordering::SeqCst)
    }

    /// Manually mark the skill catalog as stale. Used by tests and by
    /// the CLI when an internal event (e.g. `harn install`) should
    /// trigger the same rebuild a `skills/update` notification would.
    pub fn signal_skills_reload(&self) {
        self.skills_reload_requested.store(true, Ordering::SeqCst);
    }

    /// Call the host's `skills/list` RPC and return the raw JSON array
    /// it responded with. Shape:
    /// `[{ "id": "...", "name": "...", "description": "...", "source": "..." }, ...]`.
    /// The CLI adapter converts each entry into a
    /// [`crate::skills::SkillManifestRef`].
    pub async fn list_host_skills(&self) -> Result<Vec<serde_json::Value>, VmError> {
        let result = self.call("skills/list", serde_json::json!({})).await?;
        match result {
            serde_json::Value::Array(items) => Ok(items),
            serde_json::Value::Object(map) => match map.get("skills") {
                Some(serde_json::Value::Array(items)) => Ok(items.clone()),
                _ => Err(VmError::Runtime(
                    "skills/list: host response must be an array or { skills: [...] }".into(),
                )),
            },
            _ => Err(VmError::Runtime(
                "skills/list: unexpected response shape".into(),
            )),
        }
    }

    /// Call the host's `host/tools/list` RPC and return normalized tool
    /// descriptors. Shape:
    /// `[{ "name": "...", "description": "...", "schema": {...}, "deprecated": false }, ...]`.
    /// The bridge also accepts `{ "tools": [...] }` and
    /// `{ "result": { "tools": [...] } }` wrappers for lenient hosts.
    pub async fn list_host_tools(&self) -> Result<Vec<serde_json::Value>, VmError> {
        let result = self.call("host/tools/list", serde_json::json!({})).await?;
        parse_host_tools_list_response(result)
    }

    /// Call the host's `skills/fetch` RPC for one skill id. Returns the
    /// raw JSON body so the CLI can inspect both the frontmatter fields
    /// and the skill markdown body in whatever shape the host sends.
    pub async fn fetch_host_skill(&self, id: &str) -> Result<serde_json::Value, VmError> {
        self.call("skills/fetch", serde_json::json!({ "id": id }))
            .await
    }

    pub fn injection_state(&self) -> HostBridgeInjectionState {
        self.queued_transcript_injections.clone()
    }

    pub async fn push_pending_user_message(
        &self,
        content: String,
        transcript_content: serde_json::Value,
        mode: &str,
    ) -> String {
        self.queued_transcript_injections
            .push_pending_user_message(content, transcript_content, mode)
            .await
    }

    pub async fn push_queued_user_message(&self, content: String, mode: &str) -> String {
        self.push_pending_user_message(content.clone(), serde_json::Value::String(content), mode)
            .await
    }

    pub async fn revoke_pending_user_message(
        &self,
        message_id: &str,
    ) -> PendingUserMessageMutationResult {
        self.queued_transcript_injections
            .revoke_pending_user_message(message_id)
            .await
    }

    pub async fn revoke_pending_reminder(
        &self,
        reminder_id: &str,
    ) -> PendingReminderMutationResult {
        self.queued_transcript_injections
            .revoke_pending_reminder(reminder_id)
            .await
    }

    pub async fn pending_injections_json(&self) -> serde_json::Value {
        self.queued_transcript_injections
            .pending_injections_json()
            .await
    }

    pub async fn replace_pending_user_message(
        &self,
        message_id: &str,
        content: String,
        transcript_content: serde_json::Value,
    ) -> PendingUserMessageMutationResult {
        self.queued_transcript_injections
            .replace_pending_user_message(message_id, content, transcript_content)
            .await
    }

    pub async fn push_queued_session_remind_from_params(
        &self,
        params: &serde_json::Value,
    ) -> Result<String, String> {
        let reminder = queued_session_remind_from_params(params)?;
        let reminder_id = reminder.reminder.id.clone();
        self.queued_transcript_injections
            .push_session_reminder(reminder)
            .await;
        Ok(reminder_id)
    }

    pub async fn take_queued_user_messages(
        &self,
        include_interrupt_immediate: bool,
        include_finish_step: bool,
        include_audit_only: bool,
    ) -> Vec<QueuedUserMessage> {
        let mut state = self.queued_transcript_injections.inner.lock().await;
        let mut selected = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(injection) = state.queue.pop_front() {
            let should_take = match injection.mode() {
                QueuedUserMessageMode::InterruptImmediate => include_interrupt_immediate,
                QueuedUserMessageMode::FinishStep => include_finish_step,
                QueuedUserMessageMode::AuditOnly => include_audit_only,
            };
            match (should_take, injection) {
                (true, QueuedTranscriptInjection::User(message)) => {
                    state
                        .delivered_user_message_ids
                        .insert(message.message_id.clone());
                    selected.push(message);
                }
                (_, injection) => retained.push_back(injection),
            }
        }
        state.queue = retained;
        selected
    }

    pub async fn take_queued_transcript_injections(
        &self,
        include_interrupt_immediate: bool,
        include_finish_step: bool,
        include_audit_only: bool,
    ) -> Vec<QueuedTranscriptInjection> {
        let mut state = self.queued_transcript_injections.inner.lock().await;
        let mut selected = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(injection) = state.queue.pop_front() {
            let should_take = match injection.mode() {
                QueuedUserMessageMode::InterruptImmediate => include_interrupt_immediate,
                QueuedUserMessageMode::FinishStep => include_finish_step,
                QueuedUserMessageMode::AuditOnly => include_audit_only,
            };
            if should_take {
                match &injection {
                    QueuedTranscriptInjection::User(message) => {
                        state
                            .delivered_user_message_ids
                            .insert(message.message_id.clone());
                    }
                    QueuedTranscriptInjection::Reminder(reminder) => {
                        state
                            .delivered_reminder_ids
                            .insert(reminder.reminder.id.clone());
                    }
                }
                selected.push(injection);
            } else {
                retained.push_back(injection);
            }
        }
        state.queue = retained;
        selected
    }

    pub async fn take_queued_user_messages_for(
        &self,
        checkpoint: DeliveryCheckpoint,
    ) -> Vec<QueuedUserMessage> {
        match checkpoint {
            DeliveryCheckpoint::InterruptImmediate => {
                self.take_queued_user_messages(true, false, false).await
            }
            DeliveryCheckpoint::AfterCurrentOperation => {
                self.take_queued_user_messages(false, true, false).await
            }
            DeliveryCheckpoint::EndOfInteraction => {
                self.take_queued_user_messages(false, false, true).await
            }
        }
    }

    pub async fn take_queued_transcript_injections_for(
        &self,
        checkpoint: DeliveryCheckpoint,
    ) -> Vec<QueuedTranscriptInjection> {
        match checkpoint {
            DeliveryCheckpoint::InterruptImmediate => {
                self.take_queued_transcript_injections(true, false, false)
                    .await
            }
            DeliveryCheckpoint::AfterCurrentOperation => {
                self.take_queued_transcript_injections(false, true, false)
                    .await
            }
            DeliveryCheckpoint::EndOfInteraction => {
                self.take_queued_transcript_injections(false, false, true)
                    .await
            }
        }
    }

    /// Send an output notification (for log/print in bridge mode).
    pub fn send_output(&self, text: &str) {
        self.notify("output", serde_json::json!({"text": text}));
    }

    /// Send a progress notification with optional numeric progress and structured data.
    pub fn send_progress(
        &self,
        phase: &str,
        message: &str,
        progress: Option<i64>,
        total: Option<i64>,
        data: Option<serde_json::Value>,
    ) {
        let mut payload = serde_json::json!({"phase": phase, "message": message});
        if let Some(p) = progress {
            payload["progress"] = serde_json::json!(p);
        }
        if let Some(t) = total {
            payload["total"] = serde_json::json!(t);
        }
        if let Some(d) = data {
            payload["data"] = d;
        }
        self.notify("progress", payload);
    }

    /// Send a structured log notification.
    pub fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
        let mut payload = serde_json::json!({"level": level, "message": message});
        if let Some(f) = fields {
            payload["fields"] = f;
        }
        self.notify("log", payload);
    }

    /// Send a `session/update` with `call_start` — signals the beginning of
    /// an LLM call, tool call, or builtin call for observability.
    pub fn send_call_start(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        metadata: serde_json::Value,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        let stream_publicly = metadata
            .get("stream_publicly")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        self.visible_call_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.to_string(), stream_publicly);
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_start",
                    "content": {
                        "toolCallId": call_id,
                        "call_type": call_type,
                        "name": name,
                        "script": script,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }

    /// Send a `session/update` with `call_progress` — a streaming token delta
    /// from an in-flight LLM call.
    pub fn send_call_progress(
        &self,
        call_id: &str,
        delta: &str,
        accumulated_tokens: u64,
        user_visible: bool,
    ) {
        let session_id = self.get_session_id();
        let (visible_text, visible_delta) = {
            let stream_publicly = self
                .visible_call_streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(call_id)
                .copied()
                .unwrap_or(true);
            if !user_visible || !stream_publicly {
                (String::new(), String::new())
            } else {
                let mut states = self
                    .visible_call_states
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let state = states.entry(call_id.to_string()).or_default();
                state.push(delta, true)
            }
        };
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_progress",
                    "content": {
                        "toolCallId": call_id,
                        "delta": delta,
                        "accumulated_tokens": accumulated_tokens,
                        "visible_text": visible_text,
                        "visible_delta": visible_delta,
                        "user_visible": user_visible,
                    },
                },
            }),
        );
    }

    /// Send a `session/update` with `call_end` — signals completion of a call.
    pub fn send_call_end(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        duration_ms: u64,
        status: &str,
        metadata: serde_json::Value,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        self.visible_call_states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
        self.visible_call_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_end",
                    "content": {
                        "toolCallId": call_id,
                        "call_type": call_type,
                        "name": name,
                        "script": script,
                        "duration_ms": duration_ms,
                        "status": status,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }

    /// Send a worker lifecycle update for delegated/background execution.
    pub fn send_worker_update(
        &self,
        worker_id: &str,
        worker_name: &str,
        status: &str,
        metadata: serde_json::Value,
        audit: Option<&MutationSessionRecord>,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        let started_at = metadata.get("started_at").cloned().unwrap_or_default();
        let finished_at = metadata.get("finished_at").cloned().unwrap_or_default();
        let snapshot_path = metadata.get("snapshot_path").cloned().unwrap_or_default();
        let run_id = metadata.get("child_run_id").cloned().unwrap_or_default();
        let run_path = metadata.get("child_run_path").cloned().unwrap_or_default();
        let lifecycle = serde_json::json!({
            "event": status,
            "worker_id": worker_id,
            "worker_name": worker_name,
            "started_at": started_at,
            "finished_at": finished_at,
        });
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "worker_update",
                    "content": {
                        "worker_id": worker_id,
                        "worker_name": worker_name,
                        "status": status,
                        "script": script,
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "snapshot_path": snapshot_path,
                        "run_id": run_id,
                        "run_path": run_path,
                        "lifecycle": lifecycle,
                        "audit": audit,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }
}

/// Convert a serde_json::Value to a VmValue.
pub fn json_result_to_vm_value(val: &serde_json::Value) -> VmValue {
    crate::stdlib::json_to_vm_value(val)
}

fn parse_host_tools_list_response(
    result: serde_json::Value,
) -> Result<Vec<serde_json::Value>, VmError> {
    let tools = match result {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => match map.get("tools").cloned().or_else(|| {
            map.get("result")
                .and_then(|value| value.get("tools"))
                .cloned()
        }) {
            Some(serde_json::Value::Array(items)) => items,
            _ => {
                return Err(VmError::Runtime(
                    "host/tools/list: host response must be an array or { tools: [...] }".into(),
                ));
            }
        },
        _ => {
            return Err(VmError::Runtime(
                "host/tools/list: unexpected response shape".into(),
            ));
        }
    };

    let mut normalized = Vec::with_capacity(tools.len());
    for tool in tools {
        let serde_json::Value::Object(map) = tool else {
            return Err(VmError::Runtime(
                "host/tools/list: every tool must be an object".into(),
            ));
        };
        let Some(name) = map.get("name").and_then(|value| value.as_str()) else {
            return Err(VmError::Runtime(
                "host/tools/list: every tool must include a string `name`".into(),
            ));
        };
        let description = map
            .get("description")
            .and_then(|value| value.as_str())
            .or_else(|| {
                map.get("short_description")
                    .and_then(|value| value.as_str())
            })
            .unwrap_or_default();
        let schema = map
            .get("schema")
            .cloned()
            .or_else(|| map.get("parameters").cloned())
            .or_else(|| map.get("input_schema").cloned())
            .unwrap_or(serde_json::Value::Null);
        let deprecated = map
            .get("deprecated")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        normalized.push(serde_json::json!({
            "name": name,
            "description": description,
            "schema": schema,
            "deprecated": deprecated,
        }));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bridge() -> HostBridge {
        HostBridge::from_parts(
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(std::sync::Mutex::new(())),
            1,
        )
    }

    fn test_bridge_sharing_injection_state(owner: &HostBridge) -> HostBridge {
        HostBridge::from_parts_with_writer_cancel_notify_and_injection_state(
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
            Arc::new(|_| Ok(())),
            100,
            Some(owner.injection_state()),
        )
    }

    #[test]
    fn test_json_rpc_request_format() {
        let request = crate::jsonrpc::request(
            1,
            "llm_call",
            serde_json::json!({
                "prompt": "Hello",
                "system": "Be helpful",
            }),
        );
        let s = serde_json::to_string(&request).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"method\":\"llm_call\""));
    }

    #[test]
    fn test_json_rpc_notification_format() {
        let notification =
            crate::jsonrpc::notification("output", serde_json::json!({"text": "[harn] hello\n"}));
        let s = serde_json::to_string(&notification).unwrap();
        assert!(s.contains("\"method\":\"output\""));
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn test_json_rpc_error_response_parsing() {
        let response = crate::jsonrpc::error_response(1, -32600, "Invalid request");
        assert!(response.get("error").is_some());
        assert_eq!(
            response["error"]["message"].as_str().unwrap(),
            "Invalid request"
        );
    }

    #[test]
    fn test_json_rpc_success_response_parsing() {
        let response = crate::jsonrpc::response(
            1,
            serde_json::json!({
                "text": "Hello world",
                "input_tokens": 10,
                "output_tokens": 5,
            }),
        );
        assert!(response.get("result").is_some());
        assert_eq!(response["result"]["text"].as_str().unwrap(), "Hello world");
    }

    #[test]
    fn test_cancelled_flag() {
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(!cancelled.load(Ordering::SeqCst));
        cancelled.store(true, Ordering::SeqCst);
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn pending_host_calls_return_when_cancellation_arrives() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let pending = Arc::new(Mutex::new(HashMap::new()));
            let cancelled = Arc::new(AtomicBool::new(false));
            let bridge = HostBridge::from_parts_with_writer(
                pending.clone(),
                cancelled.clone(),
                Arc::new(|_| Ok(())),
                1,
            );

            let call = bridge.call("host/work", serde_json::json!({}));
            tokio::pin!(call);

            loop {
                tokio::select! {
                    result = &mut call => panic!("call completed before cancellation: {result:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                if !pending.lock().await.is_empty() {
                    break;
                }
            }

            cancelled.store(true, Ordering::SeqCst);
            bridge.cancel_notify.notify_waiters();

            let result = tokio::time::timeout(Duration::from_secs(1), call)
                .await
                .expect("pending call should observe cancellation promptly");
            assert!(
                matches!(result, Err(VmError::Runtime(message)) if message.contains("cancelled"))
            );
            assert!(pending.lock().await.is_empty());
        });
    }

    #[test]
    fn call_progress_hides_non_user_visible_deltas() {
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured = lines.clone();
        let bridge = HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                captured
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(line.to_string());
                Ok(())
            }),
            1,
        );

        bridge.send_call_start(
            "call-1",
            "llm",
            "llm_call",
            serde_json::json!({"stream_publicly": true}),
        );
        bridge.send_call_progress(
            "call-1",
            r#"{"verdict":"done","reasoning":"internal"}"#,
            1,
            false,
        );

        let lines = lines.lock().unwrap_or_else(|e| e.into_inner());
        let progress: serde_json::Value =
            serde_json::from_str(&lines[1]).expect("call_progress notification json");
        let content = &progress["params"]["update"]["content"];
        assert_eq!(
            content["delta"],
            r#"{"verdict":"done","reasoning":"internal"}"#
        );
        assert_eq!(content["user_visible"], false);
        assert_eq!(content["visible_text"], "");
        assert_eq!(content["visible_delta"], "");
    }

    #[test]
    fn call_progress_hides_non_public_streams() {
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let captured = lines.clone();
        let bridge = HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                captured
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(line.to_string());
                Ok(())
            }),
            1,
        );

        bridge.send_call_start(
            "call-1",
            "llm",
            "llm_call",
            serde_json::json!({"stream_publicly": false}),
        );
        bridge.send_call_progress("call-1", "secret schema bytes", 1, true);

        let lines = lines.lock().unwrap_or_else(|e| e.into_inner());
        let progress: serde_json::Value =
            serde_json::from_str(&lines[1]).expect("call_progress notification json");
        let content = &progress["params"]["update"]["content"];
        assert_eq!(content["delta"], "secret schema bytes");
        assert_eq!(content["user_visible"], true);
        assert_eq!(content["visible_text"], "");
        assert_eq!(content["visible_delta"], "");
    }

    #[test]
    fn queued_messages_are_filtered_by_delivery_mode() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            bridge
                .push_queued_user_message("first".to_string(), "finish_step")
                .await;
            bridge
                .push_queued_user_message("second".to_string(), "audit_only")
                .await;

            let finish_step = bridge.take_queued_user_messages(false, true, false).await;
            assert_eq!(finish_step.len(), 1);
            assert_eq!(finish_step[0].content, "first");

            let audit_only = bridge.take_queued_user_messages(false, false, true).await;
            assert_eq!(audit_only.len(), 1);
            assert_eq!(audit_only[0].content, "second");
        });
    }

    #[test]
    fn pending_user_messages_support_revoke_replace_and_delivery_states() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            let first_id = bridge
                .push_pending_user_message(
                    "first".to_string(),
                    serde_json::json!("first"),
                    "audit_only",
                )
                .await;
            let second_id = bridge
                .push_pending_user_message(
                    "second".to_string(),
                    serde_json::json!("second"),
                    "audit_only",
                )
                .await;

            assert_eq!(
                bridge
                    .replace_pending_user_message(
                        &second_id,
                        "second edited".to_string(),
                        serde_json::json!("second edited"),
                    )
                    .await,
                PendingUserMessageMutationResult::Mutated
            );
            assert_eq!(
                bridge.revoke_pending_user_message(&first_id).await,
                PendingUserMessageMutationResult::Mutated
            );
            assert_eq!(
                bridge.revoke_pending_user_message(&first_id).await,
                PendingUserMessageMutationResult::AlreadyRevoked
            );

            let delivered = bridge
                .take_queued_user_messages_for(DeliveryCheckpoint::EndOfInteraction)
                .await;
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].message_id, second_id);
            assert_eq!(delivered[0].content, "second edited");

            assert_eq!(
                bridge.revoke_pending_user_message(&second_id).await,
                PendingUserMessageMutationResult::AlreadyDelivered
            );
            assert_eq!(
                bridge
                    .replace_pending_user_message(
                        &second_id,
                        "too late".to_string(),
                        serde_json::json!("too late"),
                    )
                    .await,
                PendingUserMessageMutationResult::AlreadyDelivered
            );
            assert_eq!(
                bridge.revoke_pending_user_message("missing").await,
                PendingUserMessageMutationResult::UnknownMessageId
            );
        });
    }

    #[test]
    fn pending_user_message_replace_preserves_fifo_position_and_mode() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            let first_id = bridge
                .push_pending_user_message(
                    "first".to_string(),
                    serde_json::json!("first"),
                    "finish_step",
                )
                .await;
            let second_id = bridge
                .push_pending_user_message(
                    "second".to_string(),
                    serde_json::json!("second"),
                    "finish_step",
                )
                .await;
            assert_eq!(
                bridge
                    .replace_pending_user_message(
                        &first_id,
                        "first edited".to_string(),
                        serde_json::json!("first edited"),
                    )
                    .await,
                PendingUserMessageMutationResult::Mutated
            );

            let delivered = bridge
                .take_queued_user_messages_for(DeliveryCheckpoint::AfterCurrentOperation)
                .await;
            assert_eq!(
                delivered
                    .iter()
                    .map(|message| (&message.message_id, message.content.as_str(), message.mode))
                    .collect::<Vec<_>>(),
                vec![
                    (&first_id, "first edited", QueuedUserMessageMode::FinishStep,),
                    (&second_id, "second", QueuedUserMessageMode::FinishStep),
                ]
            );
        });
    }

    #[test]
    fn pending_user_message_state_survives_bridge_replacement() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            let revoked_id = bridge
                .push_pending_user_message(
                    "revoke me".to_string(),
                    serde_json::json!("revoke me"),
                    "audit_only",
                )
                .await;
            let delivered_id = bridge
                .push_pending_user_message(
                    "deliver me".to_string(),
                    serde_json::json!("deliver me"),
                    "audit_only",
                )
                .await;
            assert_eq!(
                bridge.revoke_pending_user_message(&revoked_id).await,
                PendingUserMessageMutationResult::Mutated
            );
            bridge.cancelled.store(true, Ordering::SeqCst);

            let replacement_bridge = test_bridge_sharing_injection_state(&bridge);
            assert_eq!(
                replacement_bridge
                    .revoke_pending_user_message(&revoked_id)
                    .await,
                PendingUserMessageMutationResult::AlreadyRevoked
            );
            let delivered = replacement_bridge
                .take_queued_user_messages_for(DeliveryCheckpoint::EndOfInteraction)
                .await;
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].message_id, delivered_id);
            assert_eq!(delivered[0].content, "deliver me");
            assert_eq!(
                bridge.revoke_pending_user_message(&delivered_id).await,
                PendingUserMessageMutationResult::AlreadyDelivered
            );
        });
    }

    #[test]
    fn queued_transcript_injections_preserve_user_reminder_separation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            bridge
                .push_queued_user_message("human follow-up".to_string(), "finish_step")
                .await;
            let reminder_id = bridge
                .push_queued_session_remind_from_params(&serde_json::json!({
                    "body": "Host-provided ambient context.",
                    "tags": ["host"],
                    "dedupe_key": "host-context",
                    "ttl_turns": 2,
                    "mode": "audit_only",
                    "_meta": {"harn": {"source": "test"}},
                }))
                .await
                .expect("valid reminder");

            let finish_step = bridge.take_queued_user_messages(false, true, false).await;
            assert_eq!(finish_step.len(), 1);
            assert_eq!(finish_step[0].content, "human follow-up");

            let no_user_messages = bridge.take_queued_user_messages(false, false, true).await;
            assert!(no_user_messages.is_empty());

            let injections = bridge
                .take_queued_transcript_injections_for(DeliveryCheckpoint::EndOfInteraction)
                .await;
            assert_eq!(injections.len(), 1);
            let QueuedTranscriptInjection::Reminder(reminder) = &injections[0] else {
                panic!("expected queued reminder");
            };
            assert_eq!(reminder.reminder.id, reminder_id);
            assert_eq!(reminder.reminder.body, "Host-provided ambient context.");
            assert_eq!(reminder.reminder.tags, vec!["host".to_string()]);
            assert_eq!(
                reminder.reminder.dedupe_key.as_deref(),
                Some("host-context")
            );
            assert_eq!(reminder.reminder.ttl_turns, Some(2));
            assert_eq!(
                reminder.reminder.source,
                crate::llm::helpers::ReminderSource::Bridge
            );
        });
    }

    #[test]
    fn pending_injections_list_user_messages_and_reminders_in_fifo_order() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            let message_id = bridge
                .push_pending_user_message(
                    "human follow-up".to_string(),
                    serde_json::json!([{"type": "text", "text": "human follow-up"}]),
                    "finish_step",
                )
                .await;
            let reminder_id = bridge
                .push_queued_session_remind_from_params(&serde_json::json!({
                    "id": "rem-test",
                    "body": "Host reminder",
                    "tags": ["host"],
                    "dedupe_key": "host-reminder",
                    "ttl_turns": 2,
                    "mode": "interrupt_immediate",
                }))
                .await
                .expect("valid session/remind payload");

            let pending = bridge.pending_injections_json().await;
            assert_eq!(pending["pendingCount"], 2);
            assert_eq!(pending["injections"][0]["kind"], "user");
            assert_eq!(pending["injections"][0]["id"], message_id);
            assert_eq!(pending["injections"][0]["messageId"], message_id);
            assert_eq!(pending["injections"][0]["mode"], "finish_step");
            assert_eq!(pending["injections"][0]["position"], 0);
            assert_eq!(pending["injections"][1]["kind"], "reminder");
            assert_eq!(pending["injections"][1]["id"], reminder_id);
            assert_eq!(pending["injections"][1]["reminderId"], "rem-test");
            assert_eq!(pending["injections"][1]["mode"], "interrupt_immediate");
            assert_eq!(pending["injections"][1]["body"], "Host reminder");
            assert_eq!(pending["injections"][1]["dedupeKey"], "host-reminder");
            assert_eq!(pending["injections"][1]["ttlTurns"], 2);
            assert_eq!(pending["injections"][1]["position"], 1);
        });
    }

    #[test]
    fn pending_reminders_support_revoke_and_delivery_states() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = test_bridge();
            let revoked_id = bridge
                .push_queued_session_remind_from_params(&serde_json::json!({
                    "id": "rem-revoke",
                    "body": "remove me",
                    "mode": "finish_step",
                }))
                .await
                .expect("valid session/remind payload");
            let delivered_id = bridge
                .push_queued_session_remind_from_params(&serde_json::json!({
                    "id": "rem-deliver",
                    "body": "deliver me",
                    "mode": "finish_step",
                }))
                .await
                .expect("valid session/remind payload");

            assert_eq!(
                bridge.revoke_pending_reminder(&revoked_id).await,
                PendingReminderMutationResult::Mutated
            );
            assert_eq!(
                bridge.revoke_pending_reminder(&revoked_id).await,
                PendingReminderMutationResult::AlreadyRevoked
            );

            let pending = bridge.pending_injections_json().await;
            assert_eq!(pending["pendingCount"], 1);
            assert_eq!(pending["injections"][0]["reminderId"], delivered_id);

            let delivered = bridge
                .take_queued_transcript_injections_for(DeliveryCheckpoint::AfterCurrentOperation)
                .await;
            assert_eq!(delivered.len(), 1);
            let QueuedTranscriptInjection::Reminder(reminder) = &delivered[0] else {
                panic!("expected delivered reminder");
            };
            assert_eq!(reminder.reminder.id, delivered_id);

            assert_eq!(
                bridge.revoke_pending_reminder(&delivered_id).await,
                PendingReminderMutationResult::AlreadyDelivered
            );
            assert_eq!(
                bridge.revoke_pending_reminder("missing").await,
                PendingReminderMutationResult::UnknownReminderId
            );
        });
    }

    #[test]
    fn bridge_remind_modes_honor_delivery_checkpoints() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let cases = [
                (
                    "interrupt_immediate",
                    DeliveryCheckpoint::InterruptImmediate,
                    DeliveryCheckpoint::AfterCurrentOperation,
                ),
                (
                    "finish_step",
                    DeliveryCheckpoint::AfterCurrentOperation,
                    DeliveryCheckpoint::EndOfInteraction,
                ),
                (
                    "audit_only",
                    DeliveryCheckpoint::EndOfInteraction,
                    DeliveryCheckpoint::InterruptImmediate,
                ),
            ];

            for (mode, expected_checkpoint, wrong_checkpoint) in cases {
                let bridge = test_bridge();
                bridge
                    .push_queued_session_remind_from_params(&serde_json::json!({
                        "body": format!("Reminder for {mode}"),
                        "mode": mode,
                    }))
                    .await
                    .expect("valid session/remind payload");

                let premature = bridge
                    .take_queued_transcript_injections_for(wrong_checkpoint)
                    .await;
                assert!(
                    premature.is_empty(),
                    "{mode} reminder must not be delivered at {wrong_checkpoint:?}"
                );

                let delivered = bridge
                    .take_queued_transcript_injections_for(expected_checkpoint)
                    .await;
                assert_eq!(delivered.len(), 1, "{mode} reminder was not delivered");
                let QueuedTranscriptInjection::Reminder(reminder) = &delivered[0] else {
                    panic!("expected reminder for {mode}");
                };
                assert_eq!(reminder.reminder.body, format!("Reminder for {mode}"));
            }
        });
    }

    #[test]
    fn session_remind_validation_rejects_user_message_shape() {
        let err = queued_session_remind_from_params(&serde_json::json!({
            "content": "this is still a user message",
            "mode": "interrupt_immediate",
        }))
        .expect_err("session/remind must require a reminder body");
        assert!(err.contains(Code::ReminderInvalidShape.as_str()));
        assert!(err.contains("body"));
    }

    #[test]
    fn session_remind_validation_rejects_unknown_options_separately() {
        let err = queued_session_remind_from_params(&serde_json::json!({
            "body": "valid body",
            "unknown_host_field": true,
        }))
        .expect_err("session/remind must reject unknown top-level fields");
        assert!(err.contains(Code::ReminderUnknownOption.as_str()));
        assert!(err.contains("unknown_host_field"));
    }

    #[test]
    fn session_remind_validation_rejects_unknown_propagate_with_specific_code() {
        let err = queued_session_remind_from_params(&serde_json::json!({
            "body": "valid body",
            "propagate": "workspace",
        }))
        .expect_err("session/remind must reject unknown propagate values");
        assert!(err.contains(Code::ReminderUnknownPropagate.as_str()));
        assert!(err.contains("propagate"));
    }

    #[test]
    fn test_json_result_to_vm_value_string() {
        let val = serde_json::json!("hello");
        let vm_val = json_result_to_vm_value(&val);
        assert_eq!(vm_val.display(), "hello");
    }

    #[test]
    fn test_json_result_to_vm_value_dict() {
        let val = serde_json::json!({"name": "test", "count": 42});
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("name").unwrap().display(), "test");
        assert_eq!(d.get("count").unwrap().display(), "42");
    }

    #[test]
    fn test_json_result_to_vm_value_null() {
        let val = serde_json::json!(null);
        let vm_val = json_result_to_vm_value(&val);
        assert!(matches!(vm_val, VmValue::Nil));
    }

    #[test]
    fn test_json_result_to_vm_value_nested() {
        let val = serde_json::json!({
            "text": "response",
            "tool_calls": [
                {"id": "tc_1", "name": "read_file", "arguments": {"path": "foo.rs"}}
            ],
            "input_tokens": 100,
            "output_tokens": 50,
        });
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("text").unwrap().display(), "response");
        let VmValue::List(list) = d.get("tool_calls").unwrap() else {
            unreachable!("Expected List for tool_calls");
        };
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn parse_host_tools_list_accepts_object_wrapper() {
        let tools = parse_host_tools_list_response(serde_json::json!({
            "tools": [
                {
                    "name": "Read",
                    "description": "Read a file",
                    "schema": {"type": "object"},
                }
            ]
        }))
        .expect("tool list");

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "Read");
        assert_eq!(tools[0]["deprecated"], false);
    }

    #[test]
    fn parse_host_tools_list_accepts_compat_fields() {
        let tools = parse_host_tools_list_response(serde_json::json!({
            "result": {
                "tools": [
                    {
                        "name": "Edit",
                        "short_description": "Apply an edit",
                        "input_schema": {"type": "object"},
                        "deprecated": true,
                    }
                ]
            }
        }))
        .expect("tool list");

        assert_eq!(tools[0]["description"], "Apply an edit");
        assert_eq!(tools[0]["schema"]["type"], "object");
        assert_eq!(tools[0]["deprecated"], true);
    }

    #[test]
    fn parse_host_tools_list_requires_tool_names() {
        let err = parse_host_tools_list_response(serde_json::json!({
            "tools": [
                {"description": "missing name"}
            ]
        }))
        .expect_err("expected error");
        assert!(err
            .to_string()
            .contains("host/tools/list: every tool must include a string `name`"));
    }

    #[test]
    fn test_timeout_duration() {
        assert_eq!(DEFAULT_TIMEOUT.as_secs(), 300);
    }
}
