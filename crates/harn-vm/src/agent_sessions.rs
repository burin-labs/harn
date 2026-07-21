//! First-class session storage.
//!
//! A session owns three things:
//!
//! 1. A transcript dict (messages, events, summary, metadata, …).
//! 2. Closure subscribers that fire on agent-loop events for this session.
//! 3. Its own lifecycle (open, reset, fork, trim, compact, close).
//!
//! Storage is thread-local because session lifecycle and subscriber dispatch
//! are owned by the current-thread agent-loop worker. The subscribers register,
//! fire, and unregister on that same thread, keeping ordering deterministic.
//!
//! Lifecycle is explicit. Builtins (`agent_session_open`,
//! `_reset`, `_fork`, `_fork_at`, `_close`, `_trim`, `_compact`,
//! `_inject`, `_exists`, `_length`, `_snapshot`, `_ancestry`) drive
//! the store directly — there is no "policy" config dict that
//! performs lifecycle as a side effect.

use crate::value::VmDictExt;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::actor_chain::ActorChain;
use crate::agent_events::{
    AgentEvent, AttachmentFlavor, AttachmentRendering, HostInjectionProvenance, InjectionDelivery,
    SanitizationAction, SanitizationVerdict, ToolCallStatus,
};
use crate::agent_transcript_budget::{
    apply_transcript_with_budget, transcript_budget_policy_json, transcript_budget_usage_json,
    transcript_message_count, transcript_usage,
};
use crate::runtime_limits::RuntimeLimits;
use crate::security::TrustLevel;
use crate::tool_annotations::ToolKind;
use crate::value::VmValue;
use crate::workspace_anchor::{
    MountMode, MountedRoot, WorkspaceAnchor, WorkspacePolicy, WORKSPACE_ANCHOR_METADATA_KEY,
};

mod changed_paths;
pub use changed_paths::{
    clear_all_session_changed_paths, clear_session_changed_paths, record_session_changed_path,
    session_changed_paths, take_session_changed_paths,
};
mod journal;
pub(crate) use journal::has_journal;
pub(crate) use journal::{clear_journal, install_journal, next_journal_event, pop_journal_event};
const LIVE_CLIENT_EVENT_KIND: &str = "live_session_client";
const LIVE_CLIENT_PERMISSION_EVENT_KIND: &str = "live_session_permission_route";

/// Default cap on concurrent sessions per VM thread. Beyond this the
/// least-recently-accessed session is evicted on the next `open`.
pub const DEFAULT_SESSION_CAP: usize = RuntimeLimits::DEFAULT.max_agent_sessions;

/// Default cap on retained prompt-visible messages per session. The
/// limit is intentionally high enough for normal long-running agents
/// while still bounding accidental unbounded growth.
pub const DEFAULT_TRANSCRIPT_MESSAGE_CAP: usize = 4096;

/// Default cap on retained transcript audit events per session. Events
/// include message-derived entries plus orchestration lifecycle records.
pub const DEFAULT_TRANSCRIPT_EVENT_CAP: usize = 32768;
pub const MAX_SCRATCHPAD_BYTES: usize = 16 * 1024;
#[cfg(debug_assertions)]
const CACHE_STABLE_SYSTEM_PROMPT_DIAGNOSTIC: &str = "HARN-CACHE-001";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptBudgetRecovery {
    Reject,
    Trim { keep_last: usize },
    Compact { keep_last: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTranscriptBudgetPolicy {
    pub max_messages: usize,
    pub max_events: usize,
    pub max_approx_bytes: Option<usize>,
    pub recovery: TranscriptBudgetRecovery,
}

impl SessionTranscriptBudgetPolicy {
    pub fn reject(max_messages: usize, max_events: usize) -> Self {
        Self {
            max_messages: max_messages.max(1),
            max_events: max_events.max(1),
            max_approx_bytes: None,
            recovery: TranscriptBudgetRecovery::Reject,
        }
    }

    pub fn trim(max_messages: usize, max_events: usize, keep_last: usize) -> Self {
        Self {
            max_messages: max_messages.max(1),
            max_events: max_events.max(1),
            max_approx_bytes: None,
            recovery: TranscriptBudgetRecovery::Trim { keep_last },
        }
    }

    pub fn compact(max_messages: usize, max_events: usize, keep_last: usize) -> Self {
        Self {
            max_messages: max_messages.max(1),
            max_events: max_events.max(1),
            max_approx_bytes: None,
            recovery: TranscriptBudgetRecovery::Compact { keep_last },
        }
    }

    pub fn with_max_approx_bytes(mut self, max_approx_bytes: Option<usize>) -> Self {
        self.max_approx_bytes = max_approx_bytes.map(|limit| limit.max(1));
        self
    }

    pub(crate) fn normalized(&self) -> Self {
        Self {
            max_messages: self.max_messages.max(1),
            max_events: self.max_events.max(1),
            max_approx_bytes: self.max_approx_bytes.map(|limit| limit.max(1)),
            recovery: self.recovery.clone(),
        }
    }
}

impl Default for SessionTranscriptBudgetPolicy {
    fn default() -> Self {
        Self::reject(DEFAULT_TRANSCRIPT_MESSAGE_CAP, DEFAULT_TRANSCRIPT_EVENT_CAP)
    }
}

pub struct SessionState {
    pub id: String,
    pub transcript: VmValue,
    pub subscribers: Vec<VmValue>,
    pub created_at: String,
    pub last_accessed: Instant,
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub branched_at_event_index: Option<usize>,
    pub actor_chain: Option<ActorChain>,
    /// Names of skills that were active at the end of the most recent
    /// `agent_loop` run on this session. Empty when no skills were
    /// matched, when the skill system wasn't used, or when the
    /// deactivation phase cleared them. Re-entering the session
    /// restores these as the initial active set before matching runs.
    pub active_skills: Vec<String>,
    /// Tool-calling protocol claimed by the first agent loop that uses
    /// this session. A transcript is only replayable under the same
    /// contract that produced its prompt/history.
    pub tool_format: Option<String>,
    /// Stable session-level system prompt material. This is transcript
    /// metadata, not a replay message: providers receive it through
    /// their system/developer instruction channel on each call.
    pub system_prompt: Option<String>,
    /// Session-pinned model selector. When set, `llm_call` invocations
    /// that do not pass an explicit `model:` option resolve to this
    /// selector instead of `HARN_LLM_MODEL` / provider defaults. Mid-
    /// session swap is exposed over ACP via `session/set_config_option`
    /// (configId="model").
    pub pinned_model: Option<String>,
    /// Session-pinned high-level reasoning policy. When set, `llm_call`
    /// invocations that do not pass explicit `thinking` or
    /// `reasoning_effort` options resolve this provider-aware policy into
    /// the route's native thinking shape. Exposed over ACP as
    /// `session/set_config_option(configId="thought_level")`.
    pub pinned_reasoning_policy: Option<String>,
    /// Session-local workspace defaults. Persona and host policy layers
    /// can update this without rewriting the current anchor.
    pub workspace_policy: WorkspacePolicy,
    /// Typed workspace anchor for the session. Primary path plus any
    /// additional mounted roots; consumed by permission matchers, the
    /// bundle exporter, and host-side cross-project handoff flows
    /// (epic #2208). `None` until a host opens the session with one or
    /// the ACP `reanchor` / `add_root` primitives populate it.
    pub workspace_anchor: Option<WorkspaceAnchor>,
    /// Small session-local working memory rendered into agent prompts by the
    /// Harn stdlib agent loop. This is live state, not a replayed message.
    pub scratchpad: Option<VmValue>,
    pub scratchpad_version: u64,
    pub transcript_budget_policy: SessionTranscriptBudgetPolicy,
    pub last_transcript_budget_action: Option<serde_json::Value>,
    pub live_clients: BTreeMap<String, LiveSessionClient>,
    pub live_controller_id: Option<String>,
    pub completed_turn_checkpoints: Vec<SessionTurnCheckpoint>,
    pub redo_stack: Vec<SessionRedoEntry>,
    pub text_tool_call_seq: u64,
    /// Durable trust-boundary evidence consulted by the agent dispatch gate.
    /// It lives with the transcript so pre-loop injections and resumed loops
    /// cannot lose security provenance.
    pub taint: Vec<crate::security::TaintRecord>,
    /// Pending canonical transcript mutations for the active agent run. This
    /// shares the session record's lifecycle so reset and eviction cannot
    /// leave a second ambient journal behind.
    pub(crate) transcript_journal: Option<crate::agent_session_journal::JournalState>,
}

impl SessionState {
    fn new(id: String) -> Self {
        let now = Instant::now();
        let transcript = empty_transcript(&id);
        Self {
            id,
            transcript,
            subscribers: Vec::new(),
            created_at: crate::orchestration::now_unix_seconds_text(),
            last_accessed: now,
            parent_id: None,
            child_ids: Vec::new(),
            branched_at_event_index: None,
            actor_chain: None,
            active_skills: Vec::new(),
            tool_format: None,
            system_prompt: None,
            pinned_model: None,
            pinned_reasoning_policy: None,
            workspace_policy: WorkspacePolicy::default(),
            workspace_anchor: None,
            scratchpad: None,
            scratchpad_version: 0,
            transcript_budget_policy: default_transcript_budget_policy(),
            last_transcript_budget_action: None,
            live_clients: BTreeMap::new(),
            live_controller_id: None,
            completed_turn_checkpoints: Vec::new(),
            redo_stack: Vec::new(),
            text_tool_call_seq: 0,
            taint: Vec::new(),
            transcript_journal: None,
        }
    }

    fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub(crate) fn replace_transcript(&mut self, transcript: VmValue) {
        if !crate::values_equal(&self.transcript, &transcript) {
            self.redo_stack.clear();
        }
        self.transcript = transcript;
        self.touch();
    }
}

pub(crate) fn push_session_taint(id: &str, record: crate::security::TaintRecord) {
    SESSIONS.with(|sessions| {
        if let Some(state) = sessions.borrow_mut().get_mut(id) {
            state.taint.push(record);
            state.touch();
        }
    });
}

pub(crate) fn session_taint_snapshot(id: &str) -> Vec<crate::security::TaintRecord> {
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .map(|state| state.taint.clone())
            .unwrap_or_default()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveClientMode {
    Observer,
    Controller,
}

mod host_injection;
mod live_clients;
mod metadata;
mod transcript_lifecycle;
mod types;

pub use host_injection::*;
pub use live_clients::*;
pub use metadata::*;
use metadata::{
    branch_event_index, clone_transcript_with_id, empty_transcript, session_snapshot,
    transcript_with_session_metadata, update_lineage,
};
pub use transcript_lifecycle::*;
pub use types::*;

thread_local! {
    static SESSIONS: RefCell<HashMap<String, SessionState>> = RefCell::new(HashMap::new());
    static SESSION_CAP: Cell<usize> = const { Cell::new(DEFAULT_SESSION_CAP) };
    static DEFAULT_TRANSCRIPT_BUDGET_POLICY: RefCell<SessionTranscriptBudgetPolicy> =
        RefCell::new(SessionTranscriptBudgetPolicy::default());
    static CURRENT_SESSION_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CURRENT_TOOL_CALL_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

tokio::task_local! {
    static CURRENT_TOOL_CALL_TASK: String;
}
pub struct CurrentSessionGuard {
    active: bool,
}

impl Drop for CurrentSessionGuard {
    fn drop(&mut self) {
        if self.active {
            pop_current_session();
        }
    }
}

/// RAII guard that scopes the active tool-call id for the running thread.
///
/// Set on entry to a tool dispatch and dropped on exit, so any hostlib
/// builtin invoked under it (e.g. `tools/write_file`) can resolve the
/// owning tool call without threading the id through every parameter.
pub struct CurrentToolCallGuard {
    active: bool,
}

impl Drop for CurrentToolCallGuard {
    fn drop(&mut self) {
        if self.active {
            pop_current_tool_call();
        }
    }
}

/// Set the per-thread session cap. Primarily for tests; production VMs
/// inherit the default.
pub fn set_session_cap(cap: usize) {
    SESSION_CAP.with(|c| c.set(cap.max(1)));
}

pub fn session_cap() -> usize {
    SESSION_CAP.with(|c| c.get())
}

pub fn set_default_transcript_budget_policy(policy: SessionTranscriptBudgetPolicy) {
    DEFAULT_TRANSCRIPT_BUDGET_POLICY.with(|cell| {
        *cell.borrow_mut() = policy.normalized();
    });
}

pub fn reset_default_transcript_budget_policy() {
    set_default_transcript_budget_policy(SessionTranscriptBudgetPolicy::default());
}

pub fn default_transcript_budget_policy() -> SessionTranscriptBudgetPolicy {
    DEFAULT_TRANSCRIPT_BUDGET_POLICY.with(|cell| cell.borrow().clone())
}

pub fn transcript_budget_policy(id: &str) -> Option<SessionTranscriptBudgetPolicy> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.transcript_budget_policy.clone())
    })
}

pub fn set_transcript_budget_policy(
    id: &str,
    policy: SessionTranscriptBudgetPolicy,
) -> Result<(), String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let previous = state.transcript_budget_policy.clone();
        let previous_action = state.last_transcript_budget_action.clone();
        state.transcript_budget_policy = policy.normalized();
        let candidate = state.transcript.clone();
        if let Err(error) = apply_transcript_with_budget(state, candidate, "policy_update") {
            state.transcript_budget_policy = previous;
            state.last_transcript_budget_action = previous_action;
            return Err(error);
        }
        Ok(())
    })
}

/// Clear the session store. Wired into `reset_llm_state` for test isolation.
pub fn reset_session_store() {
    SESSIONS.with(|s| s.borrow_mut().clear());
    CURRENT_SESSION_STACK.with(|stack| stack.borrow_mut().clear());
    CURRENT_TOOL_CALL_STACK.with(|stack| stack.borrow_mut().clear());
    clear_all_session_changed_paths();
    reset_default_transcript_budget_policy();
}

pub(crate) fn push_current_session(id: String) {
    if id.is_empty() {
        return;
    }
    CURRENT_SESSION_STACK.with(|stack| stack.borrow_mut().push(id));
}

pub(crate) fn swap_current_session_stack(replacement: Vec<String>) -> Vec<String> {
    CURRENT_SESSION_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), replacement))
}

pub(crate) fn pop_current_session() {
    CURRENT_SESSION_STACK.with(|stack| {
        let _ = stack.borrow_mut().pop();
    });
}

pub fn current_session_id() -> Option<String> {
    CURRENT_SESSION_STACK.with(|stack| stack.borrow().last().cloned())
}

pub(crate) fn next_text_tool_call_seq(id: &str) -> Option<u64> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let state = map.get_mut(id)?;
        let seq = state.text_tool_call_seq;
        state.text_tool_call_seq = state.text_tool_call_seq.checked_add(1).unwrap_or(0);
        state.touch();
        Some(seq)
    })
}

fn parse_text_tool_call_seq(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("tc_")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn update_next_text_tool_call_seq(value: &serde_json::Value, next_seq: &mut u64) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                update_next_text_tool_call_seq(item, next_seq);
            }
        }
        serde_json::Value::Object(map) => {
            for key in ["id", "tool_call_id"] {
                if let Some(seq) = map
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_text_tool_call_seq)
                {
                    *next_seq = (*next_seq).max(seq.saturating_add(1));
                }
            }
            for item in map.values() {
                update_next_text_tool_call_seq(item, next_seq);
            }
        }
        _ => {}
    }
}

fn next_text_tool_call_seq_from_json_messages(messages: &[serde_json::Value]) -> u64 {
    let mut next_seq = 0;
    for message in messages {
        update_next_text_tool_call_seq(message, &mut next_seq);
    }
    next_seq
}

fn next_text_tool_call_seq_from_transcript(transcript: &VmValue) -> u64 {
    let Some(dict) = transcript.as_dict() else {
        return 0;
    };
    let Some(VmValue::List(messages)) = dict.get("messages") else {
        return 0;
    };
    next_text_tool_call_seq_from_json_messages(
        &messages
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
    )
}

pub fn current_actor_chain() -> Option<ActorChain> {
    current_session_id().as_deref().and_then(actor_chain)
}

pub fn enter_current_session(id: impl Into<String>) -> CurrentSessionGuard {
    let id = id.into();
    if id.trim().is_empty() {
        return CurrentSessionGuard { active: false };
    }
    push_current_session(id);
    CurrentSessionGuard { active: true }
}

pub fn actor_chain(id: &str) -> Option<ActorChain> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.actor_chain.clone())
    })
}

pub fn set_actor_chain(id: &str, actor_chain: Option<ActorChain>) -> Result<bool, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.actor_chain != actor_chain;
        state.actor_chain = actor_chain;
        state.touch();
        Ok(changed)
    })
}

fn push_current_tool_call(id: String) {
    if id.is_empty() {
        return;
    }
    CURRENT_TOOL_CALL_STACK.with(|stack| stack.borrow_mut().push(id));
}

fn pop_current_tool_call() {
    CURRENT_TOOL_CALL_STACK.with(|stack| {
        let _ = stack.borrow_mut().pop();
    });
}

/// Return the active tool-call id for the current thread, if any.
///
/// Hostlib builtins consult this to attribute side-effect snapshots to
/// the owning ACP `toolCallId` without callers passing it explicitly.
pub fn current_tool_call_id() -> Option<String> {
    if let Ok(id) = CURRENT_TOOL_CALL_TASK.try_with(Clone::clone) {
        if !id.trim().is_empty() {
            return Some(id);
        }
    }
    CURRENT_TOOL_CALL_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Scope the active tool-call id to one async task.
///
/// Parallel tool dispatch runs sibling calls on the same OS thread, so
/// thread-local guards alone cannot preserve attribution across `.await`
/// points. Tokio task-local scoping follows the future instead.
pub async fn scope_current_tool_call<F, T>(id: impl Into<String>, future: F) -> T
where
    F: Future<Output = T>,
{
    let id = id.into();
    if id.trim().is_empty() {
        future.await
    } else {
        CURRENT_TOOL_CALL_TASK.scope(id, future).await
    }
}

/// Scope the active tool-call id for the duration of the returned guard.
pub fn enter_current_tool_call(id: impl Into<String>) -> CurrentToolCallGuard {
    let id = id.into();
    if id.trim().is_empty() {
        return CurrentToolCallGuard { active: false };
    }
    push_current_tool_call(id);
    CurrentToolCallGuard { active: true }
}

pub fn exists(id: &str) -> bool {
    SESSIONS.with(|s| s.borrow().contains_key(id))
}

pub fn length(id: &str) -> Option<usize> {
    SESSIONS.with(|s| {
        s.borrow().get(id).map(|state| {
            state
                .transcript
                .as_dict()
                .and_then(|d| d.get("messages"))
                .and_then(|v| match v {
                    VmValue::List(list) => Some(list.len()),
                    _ => None,
                })
                .unwrap_or(0)
        })
    })
}

pub fn scratchpad(id: &str) -> Option<VmValue> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.scratchpad.clone())
    })
}

pub fn scratchpad_version(id: &str) -> Option<u64> {
    SESSIONS.with(|s| s.borrow().get(id).map(|state| state.scratchpad_version))
}

pub fn set_scratchpad(
    id: &str,
    scratchpad: VmValue,
    source: impl Into<String>,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> Result<u64, String> {
    validate_scratchpad_value(&scratchpad)?;
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let version = state.scratchpad_version.saturating_add(1);
        let event = scratchpad_transcript_event(
            "set",
            version,
            Some(&scratchpad),
            source.into(),
            reason,
            metadata,
        );
        append_event_to_state(state, event, "set_scratchpad")?;
        state.scratchpad = Some(scratchpad);
        state.scratchpad_version = version;
        state.touch();
        Ok(version)
    })
}

pub fn clear_scratchpad(
    id: &str,
    source: impl Into<String>,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> Result<u64, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let version = state.scratchpad_version.saturating_add(1);
        let event =
            scratchpad_transcript_event("clear", version, None, source.into(), reason, metadata);
        append_event_to_state(state, event, "clear_scratchpad")?;
        state.scratchpad = None;
        state.scratchpad_version = version;
        state.touch();
        Ok(version)
    })
}

fn validate_scratchpad_value(value: &VmValue) -> Result<(), String> {
    if !matches!(value, VmValue::Dict(_)) {
        return Err("agent session scratchpad must be a dict".to_string());
    }
    let json = crate::llm::helpers::vm_value_to_json(value);
    let approx_bytes = serde_json::to_vec(&json)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if approx_bytes > MAX_SCRATCHPAD_BYTES {
        return Err(format!(
            "agent session scratchpad is {approx_bytes} bytes; max is {MAX_SCRATCHPAD_BYTES}"
        ));
    }
    Ok(())
}

fn scratchpad_transcript_event(
    action: &str,
    version: u64,
    scratchpad: Option<&VmValue>,
    source: String,
    reason: Option<String>,
    metadata: serde_json::Value,
) -> VmValue {
    let scratchpad_json = scratchpad.map(crate::llm::helpers::vm_value_to_json);
    let approx_bytes = scratchpad_json
        .as_ref()
        .and_then(|value| serde_json::to_vec(value).ok().map(|bytes| bytes.len()))
        .unwrap_or(0);
    let event_metadata = serde_json::json!({
        "action": action,
        "version": version,
        "source": normalize_scratchpad_source(source),
        "reason": reason.unwrap_or_default(),
        "approx_bytes": approx_bytes,
        "counts": scratchpad_json
            .as_ref()
            .map(scratchpad_counts_json)
            .unwrap_or_else(|| serde_json::json!({})),
        "metadata": metadata,
    });
    let content = format!("Agent scratchpad {action}");
    crate::llm::helpers::transcript_event(
        "agent_scratchpad",
        "system",
        "internal",
        &content,
        Some(event_metadata),
    )
}

fn normalize_scratchpad_source(source: String) -> String {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        "harn.agent_scratchpad".to_string()
    } else {
        trimmed.to_string()
    }
}

fn scratchpad_counts_json(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "goals": scratchpad_array_len(value, "goals"),
        "open_items": scratchpad_array_len(value, "open_items"),
        "facts": scratchpad_array_len(value, "facts"),
        "refs": scratchpad_array_len(value, "refs"),
    })
}

fn scratchpad_array_len(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub fn snapshot(id: &str) -> Option<VmValue> {
    SESSIONS.with(|s| s.borrow().get(id).map(session_snapshot))
}

/// Session-only fields stay on `agent_session_snapshot`.
pub fn transcript(id: &str) -> Option<VmValue> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| transcript_with_session_metadata(state.transcript.clone(), state))
    })
}

/// Open a session, or create it if missing. Returns the resolved id.
///
/// Newly-created sessions auto-register an event-log-backed sink when a
/// generalized [`crate::event_log::EventLog`] has been installed for the
/// current VM thread. For legacy env-driven workflows that still point
/// `HARN_EVENT_LOG_DIR` at a directory, we preserve the older JSONL sink
/// as a compatibility fallback. Re-opening an existing session does not
/// re-register — sinks are per-session, owned by the first opener.
pub fn open_or_create(id: Option<String>) -> String {
    open_or_create_with_actor_chain(id, None)
}

pub fn open_or_create_with_actor_chain(
    id: Option<String>,
    requested_actor_chain: Option<ActorChain>,
) -> String {
    let resolved = id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let parent_session = current_session_id();
    let inherited_actor_chain = requested_actor_chain
        .clone()
        .or_else(|| parent_session.as_deref().and_then(actor_chain));
    let mut was_new = false;
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(state) = map.get_mut(&resolved) {
            if let Some(actor_chain) = requested_actor_chain.clone() {
                state.actor_chain = Some(actor_chain);
            }
            state.touch();
            return;
        }
        was_new = true;
        let cap = SESSION_CAP.with(|c| c.get());
        if map.len() >= cap {
            if let Some(victim) = map
                .iter()
                .min_by_key(|(_, state)| state.last_accessed)
                .map(|(id, _)| id.clone())
            {
                map.remove(&victim);
            }
        }
        let mut state = SessionState::new(resolved.clone());
        state.actor_chain = inherited_actor_chain.clone();
        map.insert(resolved.clone(), state);
    });
    if was_new {
        if let Some(parent) = parent_session.as_deref() {
            crate::agent_events::mirror_session_sinks(parent, &resolved);
        }
        try_register_event_log(&resolved);
    }
    resolved
}

pub fn open_child_session(parent_id: &str, id: Option<String>) -> String {
    open_child_session_with_actor(parent_id, id, None)
}

pub fn open_child_session_with_actor(
    parent_id: &str,
    id: Option<String>,
    actor: Option<&str>,
) -> String {
    let actor_chain = actor_chain(parent_id).map(|chain| match actor {
        Some(actor) if !actor.trim().is_empty() => chain.pushed(actor.trim()),
        _ => chain,
    });
    let resolved = open_or_create_with_actor_chain(id, actor_chain);
    link_child_session(parent_id, &resolved);
    resolved
}

pub fn link_child_session(parent_id: &str, child_id: &str) {
    link_child_session_with_branch(parent_id, child_id, None);
}

pub fn link_child_session_with_branch(
    parent_id: &str,
    child_id: &str,
    branched_at_event_index: Option<usize>,
) {
    if parent_id == child_id {
        return;
    }
    open_or_create(Some(parent_id.to_string()));
    open_or_create(Some(child_id.to_string()));
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        update_lineage(&mut map, parent_id, child_id, branched_at_event_index);
    });
}

pub fn parent_id(id: &str) -> Option<String> {
    SESSIONS.with(|s| s.borrow().get(id).and_then(|state| state.parent_id.clone()))
}

pub fn child_ids(id: &str) -> Vec<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.child_ids.clone())
            .unwrap_or_default()
    })
}

pub fn ancestry(id: &str) -> Option<SessionAncestry> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let state = map.get(id)?;
        let mut root_id = state.id.clone();
        let mut cursor = state.parent_id.clone();
        let mut seen = HashSet::from([state.id.clone()]);
        while let Some(parent_id) = cursor {
            if !seen.insert(parent_id.clone()) {
                break;
            }
            root_id = parent_id.clone();
            cursor = map
                .get(&parent_id)
                .and_then(|parent| parent.parent_id.clone());
        }
        Some(SessionAncestry {
            parent_id: state.parent_id.clone(),
            child_ids: state.child_ids.clone(),
            root_id,
        })
    })
}

/// Auto-register a persistent sink for a newly-created session.
/// Silent no-op on failure — a broken observability sink must never
/// prevent a session from starting.
fn try_register_event_log(session_id: &str) {
    if let Some(log) = crate::event_log::active_event_log() {
        crate::agent_events::register_sink(
            session_id,
            crate::agent_events::EventLogSink::new(log, session_id),
        );
        return;
    }
    let Ok(dir) = std::env::var("HARN_EVENT_LOG_DIR") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let path = std::path::PathBuf::from(dir).join(format!("event_log-{session_id}.jsonl"));
    if let Ok(sink) = crate::agent_events::JsonlEventSink::open(&path) {
        crate::agent_events::register_sink(session_id, sink);
    }
}

pub fn register_event_log_sink(session_id: &str) {
    try_register_event_log(session_id);
}

pub fn close(id: &str) {
    SESSIONS.with(|s| {
        s.borrow_mut().remove(id);
    });
    // Cross-thread per-session state must be released too, otherwise
    // pending inbox entries can be delivered to a future session that
    // happens to reuse the same id.
    crate::orchestration::agent_inbox::clear_session(id);
    crate::agent_events::clear_session_sinks(id);
}

pub fn close_with_status(
    id: &str,
    reason: impl Into<String>,
    status: impl Into<String>,
    metadata: serde_json::Value,
) -> bool {
    if !exists(id) {
        return false;
    }
    let reason = reason.into();
    let status = status.into();
    let event_metadata = serde_json::json!({
        "reason": reason,
        "status": status,
        "metadata": metadata,
    });
    let transcript_event = crate::llm::helpers::transcript_event(
        "agent_session_closed",
        "system",
        "internal",
        "Agent session closed",
        Some(event_metadata),
    );
    let _ = append_event(id, transcript_event);
    crate::llm::emit_live_agent_event_sync(&crate::agent_events::AgentEvent::SessionClosed {
        session_id: id.to_string(),
        reason,
        status,
        metadata,
    });
    close(id);
    true
}

pub fn reset_transcript(id: &str) -> bool {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return false;
        };
        state.transcript = empty_transcript(id);
        state.tool_format = None;
        state.system_prompt = None;
        state.scratchpad = None;
        state.scratchpad_version = 0;
        state.last_transcript_budget_action = None;
        state.completed_turn_checkpoints.clear();
        state.redo_stack.clear();
        state.text_tool_call_seq = 0;
        state.touch();
        true
    })
}

/// Copy `src`'s transcript into a new session id. Subscribers are NOT
/// copied — a fork is a conversation branch, not an event fanout.
///
/// Touches `src`'s `last_accessed` before evicting, so the fork
/// operation itself can't make `src` look stale and kick it out of
/// the LRU just to make room for the new fork.
pub fn fork(src_id: &str, dst_id: Option<String>) -> Option<String> {
    let (
        src_transcript,
        src_tool_format,
        src_system_prompt,
        src_pinned_model,
        src_pinned_reasoning_policy,
        src_actor_chain,
        src_workspace_anchor,
        src_workspace_policy,
        src_scratchpad,
        src_scratchpad_version,
        src_transcript_budget_policy,
        src_last_transcript_budget_action,
        src_text_tool_call_seq,
        src_taint,
        dst,
    ) = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let src = map.get_mut(src_id)?;
        src.touch();
        let dst = dst_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let forked_transcript = clone_transcript_with_id(&src.transcript, &dst);
        Some((
            forked_transcript,
            src.tool_format.clone(),
            src.system_prompt.clone(),
            src.pinned_model.clone(),
            src.pinned_reasoning_policy.clone(),
            src.actor_chain.clone(),
            src.workspace_anchor.clone(),
            src.workspace_policy.clone(),
            src.scratchpad.clone(),
            src.scratchpad_version,
            src.transcript_budget_policy.clone(),
            src.last_transcript_budget_action.clone(),
            src.text_tool_call_seq,
            src.taint.clone(),
            dst,
        ))
    })?;
    // Ensure cap is respected when inserting the fork.
    open_or_create(Some(dst.clone()));
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(state) = map.get_mut(&dst) {
            state.transcript = src_transcript;
            state.tool_format = src_tool_format;
            state.system_prompt = src_system_prompt;
            state.pinned_model = src_pinned_model;
            state.pinned_reasoning_policy = src_pinned_reasoning_policy;
            state.actor_chain = src_actor_chain;
            state.workspace_anchor = src_workspace_anchor;
            state.workspace_policy = src_workspace_policy;
            state.scratchpad = src_scratchpad;
            state.scratchpad_version = src_scratchpad_version;
            state.transcript_budget_policy = src_transcript_budget_policy;
            state.last_transcript_budget_action = src_last_transcript_budget_action;
            state.text_tool_call_seq = src_text_tool_call_seq;
            state.taint = src_taint;
            state.touch();
        }
        update_lineage(&mut map, src_id, &dst, None);
    });
    let budget_ok = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(&dst) else {
            return false;
        };
        let candidate = state.transcript.clone();
        apply_transcript_with_budget(state, candidate, "fork").is_ok()
    });
    if !budget_ok {
        // The lineage edge was established before the budget check ran
        // (update_lineage rewrites dst's transcript with the parent
        // metadata that the check budgets). Undo it so a rejected fork
        // never leaves a dangling child_id pointing at the session we
        // are about to close.
        SESSIONS.with(|s| {
            let mut map = s.borrow_mut();
            if let Some(parent) = map.get_mut(src_id) {
                parent.child_ids.retain(|id| id != &dst);
            }
        });
        close(&dst);
        return None;
    }
    // open_or_create evicts BEFORE inserting, so the dst slot is
    // guaranteed once we get here. The existence check is cheap
    // insurance against a future refactor that breaks that invariant.
    if exists(&dst) {
        Some(dst)
    } else {
        None
    }
}

/// Fork `src_id` and truncate the destination transcript to the
/// first `keep_first` messages (#105 — branch-replay). Pairs with the
/// scrubber: the host picks an event index, rebuilds a message count,
/// and calls this to spawn a live sibling session that resumes from
/// the rebuilt state. Subscribers are not carried over (same as
/// `fork`), so sibling events don't double-fan into the parent's
/// consumers.
///
/// Returns the new session id on success, `None` if `src_id` doesn't
/// exist.
pub fn fork_at(src_id: &str, keep_first: usize, dst_id: Option<String>) -> Option<String> {
    let branched_at_event_index = SESSIONS.with(|s| {
        let map = s.borrow();
        let src = map.get(src_id)?;
        Some(branch_event_index(&src.transcript, keep_first))
    })?;
    let new_id = fork(src_id, dst_id)?;
    link_child_session_with_branch(src_id, &new_id, Some(branched_at_event_index));
    let _ = truncate(&new_id, keep_first);
    Some(new_id)
}

/// Truncate the session transcript to the first `keep_first`
/// messages (opposite of `trim`, which keeps the last N). Returns
/// counts and the retained tip event id when the session exists.
pub fn truncate(id: &str, keep_first: usize) -> Option<SessionTruncateResult> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let state = map.get_mut(id)?;
        let result = truncate_state(state, keep_first)?;
        Some(result)
    })
}

fn truncate_state(state: &mut SessionState, keep_first: usize) -> Option<SessionTruncateResult> {
    let dict = state
        .transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(crate::value::DictMap::new);
    let messages: Vec<VmValue> = match dict.get("messages") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    };
    let existing_events = match dict.get("events") {
        Some(VmValue::List(list)) => Some(list.iter().cloned().collect::<Vec<_>>()),
        _ => None,
    };
    let kept_turn_count = keep_first.min(messages.len());
    let removed_turn_count = messages.len().saturating_sub(kept_turn_count);
    let mut new_tip_turn_id = existing_events
        .as_ref()
        .map(|events| turn_event_id_for_count(events, kept_turn_count))
        .unwrap_or_else(|| {
            let events = crate::llm::helpers::transcript_events_from_messages(&messages);
            turn_event_id_for_count(&events, kept_turn_count)
        });

    if removed_turn_count > 0 {
        let retained: Vec<VmValue> = messages.into_iter().take(kept_turn_count).collect();
        let retained_events = match existing_events {
            Some(events) => {
                let keep_event_count = event_prefix_len_for_messages(&events, kept_turn_count);
                events.into_iter().take(keep_event_count).collect()
            }
            None => crate::llm::helpers::transcript_events_from_messages(&retained),
        };
        new_tip_turn_id = turn_event_id_for_count(&retained_events, kept_turn_count);
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(retained_events)),
        );
        next.insert(
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(retained)),
        );
        next.remove("summary");
        apply_transcript_with_budget(state, VmValue::dict(next), "truncate").ok()?;
    }
    state.touch();
    Some(SessionTruncateResult {
        kept_turn_count,
        removed_turn_count,
        new_tip_turn_id,
    })
}

mod pop_last_assistant;
pub use pop_last_assistant::pop_last_if_assistant;
mod restore_message_event_ids;
pub(crate) use restore_message_event_ids::restore_message_event_ids;

pub fn trim(id: &str, keep_last: usize) -> Option<usize> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let state = map.get_mut(id)?;
        let dict = state.transcript.as_dict()?.clone();
        let messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let start = messages.len().saturating_sub(keep_last);
        let retained: Vec<VmValue> = messages.into_iter().skip(start).collect();
        let kept = retained.len();
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(
                crate::llm::helpers::transcript_events_from_messages(&retained),
            )),
        );
        next.insert(
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(retained)),
        );
        apply_transcript_with_budget(state, VmValue::dict(next), "trim").ok()?;
        Some(kept)
    })
}

/// Append a message dict to the session transcript. The message must
/// have at least a string `role`; anything else is merged verbatim.
pub fn inject_message(id: &str, message: VmValue) -> Result<(), String> {
    let Some(msg_dict) = message.as_dict().cloned() else {
        return Err("agent_session_inject: message must be a dict".into());
    };
    let role_ok = matches!(msg_dict.get("role"), Some(VmValue::String(_)));
    if !role_ok {
        return Err(
            "agent_session_inject: message must have a string `role` (user|assistant|tool_result|system)"
                .into(),
        );
    }
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent_session_inject: unknown session id '{id}'"));
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(crate::value::DictMap::new);
        let mut messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let mut events: Vec<VmValue> = match dict.get("events") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => crate::llm::helpers::transcript_events_from_messages(&messages),
        };
        let new_message = VmValue::dict(msg_dict);
        let message_index = messages.len();
        let transcript_event = crate::llm::helpers::transcript_event_from_message(&new_message);
        events.push(transcript_event.clone());
        messages.push(new_message);
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(events)),
        );
        next.insert(
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(messages)),
        );
        let persisted_message = next
            .get("messages")
            .and_then(|value| match value {
                VmValue::List(list) => list.get(message_index).cloned(),
                _ => None,
            })
            .unwrap_or(VmValue::Nil);
        apply_transcript_with_budget(state, VmValue::dict(next), "inject_message")?;
        crate::agent_session_journal::enqueue_message(
            &mut state.transcript_journal,
            crate::llm::helpers::vm_value_to_json(&transcript_event),
            crate::llm::helpers::vm_value_to_json(&persisted_message),
        );
        emit_identified_user_message_event(id, &persisted_message);
        emit_llm_message_event(id, message_index, &persisted_message);
        Ok(())
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostInjectionKind {
    HostToolResult,
    HostAttachment,
}

/// Create a new session from a reconstructed message list.
///
/// This is intentionally an all-at-once write instead of repeated
/// `inject_message` calls: importing a transcript should not re-emit
/// each historic turn into the active observability sidecar.
pub fn seed_from_messages(
    id: Option<String>,
    messages: &[serde_json::Value],
    metadata: serde_json::Value,
    system_prompt: Option<String>,
    tool_format: Option<String>,
) -> Result<String, String> {
    let resolved = id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    if exists(&resolved) {
        return Err(format!("agent session '{resolved}' already exists"));
    }
    open_or_create(Some(resolved.clone()));
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(&resolved) else {
            return Err(format!("failed to create agent session '{resolved}'"));
        };
        state.tool_format = tool_format.filter(|value| !value.trim().is_empty());
        state.system_prompt = system_prompt.filter(|value| !value.trim().is_empty());

        let mut metadata = metadata
            .as_object()
            .cloned()
            .unwrap_or_else(serde_json::Map::new);
        if let Some(tool_format) = state.tool_format.as_ref() {
            metadata.insert(
                "tool_format".to_string(),
                serde_json::Value::String(tool_format.clone()),
            );
            metadata.insert(
                "tool_mode_locked".to_string(),
                serde_json::Value::Bool(true),
            );
        }
        if let Some(system_prompt) = state.system_prompt.as_ref() {
            metadata.insert(
                "system_prompt".to_string(),
                crate::llm::helpers::system_prompt_metadata(system_prompt),
            );
        }
        let text_tool_call_seq = next_text_tool_call_seq_from_json_messages(messages);
        let vm_messages = crate::llm::helpers::json_messages_to_vm(messages);
        let candidate = crate::llm::helpers::new_transcript_with(
            Some(resolved.clone()),
            vm_messages,
            None,
            Some(crate::stdlib::json_to_vm_value(&serde_json::Value::Object(
                metadata,
            ))),
        );
        apply_transcript_with_budget(state, candidate, "seed_from_messages")?;
        state.text_tool_call_seq = text_tool_call_seq;
        Ok(resolved)
    })
}

/// Load the messages vec (as JSON) for this session, for use as prefix
/// to an agent_loop run. Returns an empty vec if the session doesn't
/// exist or has no messages.
/// Append a transcript event to the session without mutating its
/// message list. Used for orchestration-side lineage events (sub-agent
/// spawn/completion, workflow hooks, etc.) that should survive
/// persistence/replay without being replayed back into the model as
/// conversational messages.
pub fn append_event(id: &str, event: VmValue) -> Result<(), String> {
    let Some(event_dict) = event.as_dict() else {
        return Err("agent_session_append_event: event must be a dict".into());
    };
    let kind_ok = matches!(event_dict.get("kind"), Some(VmValue::String(_)));
    if !kind_ok {
        return Err("agent_session_append_event: event must have a string `kind`".into());
    }
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_append_event: unknown session id '{id}'"
            ));
        };
        append_event_to_state(state, event, "append_event")?;
        Ok(())
    })
}

fn append_event_to_state(
    state: &mut SessionState,
    event: VmValue,
    action: &str,
) -> Result<(), String> {
    let journal_event = crate::llm::helpers::vm_value_to_json(&event);
    let dict = state
        .transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(crate::value::DictMap::new);
    let mut events: Vec<VmValue> = match dict.get("events") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => dict
            .get("messages")
            .and_then(|value| match value {
                VmValue::List(list) => Some(list.iter().cloned().collect::<Vec<_>>()),
                _ => None,
            })
            .map(|messages| crate::llm::helpers::transcript_events_from_messages(&messages))
            .unwrap_or_default(),
    };
    events.push(event);
    let mut next = dict;
    next.insert(
        crate::value::intern_key("events"),
        VmValue::List(std::sync::Arc::new(events)),
    );
    apply_transcript_with_budget(state, VmValue::dict(next), action)?;
    crate::agent_session_journal::enqueue_audit_event(&mut state.transcript_journal, journal_event);
    Ok(())
}

/// Replace the transcript's message list wholesale. Used by the
/// in-loop compaction path, which operates on JSON messages.
pub fn replace_messages(id: &str, messages: &[serde_json::Value]) -> Result<(), String> {
    replace_messages_with_summary(id, messages, None)
}

/// Replace the transcript's message list and optionally update the
/// `summary` field on the persisted transcript. The compaction path
/// uses this to publish the human-readable rollup line that
/// `transcript_summary(transcript)` exposes to host code.
pub fn replace_messages_with_summary(
    id: &str,
    messages: &[serde_json::Value],
    summary: Option<&str>,
) -> Result<(), String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_replace_messages: unknown session id '{id}'"
            ));
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(crate::value::DictMap::new);
        let vm_messages: Vec<VmValue> = messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect();
        let replacement_events = crate::llm::helpers::transcript_events_from_messages(&vm_messages);
        let source_event_ids = replacement_events
            .iter()
            .map(|event| {
                event
                    .as_dict()
                    .and_then(|event| event.get("id"))
                    .map(VmValue::display)
            })
            .collect();
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(replacement_events)),
        );
        next.insert(
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vm_messages)),
        );
        if let Some(summary) = summary {
            next.put_str("summary", summary);
        } else {
            next.remove("summary");
        }
        apply_transcript_with_budget(state, VmValue::dict(next), "replace_messages")?;
        crate::agent_session_journal::enqueue_messages_replaced(
            &mut state.transcript_journal,
            messages.to_vec(),
            summary.map(str::to_string),
            source_event_ids,
        );
        Ok(())
    })
}

pub fn append_subscriber(id: &str, callback: VmValue) {
    open_or_create(Some(id.to_string()));
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.subscribers.push(callback);
            state.touch();
        }
    });
}

pub fn subscribers_for(id: &str) -> Vec<VmValue> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.subscribers.clone())
            .unwrap_or_default()
    })
}

pub fn subscriber_count(id: &str) -> usize {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.subscribers.len())
            .unwrap_or(0)
    })
}

/// Persist the set of active skill names for session resume. Called at
/// the end of an agent_loop run; the next `open_or_create` for this id
/// reads them back via [`active_skills`].
#[cfg(test)]
#[path = "agent_sessions_tests.rs"]
mod tests;
