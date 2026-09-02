//! First-class session storage.
//!
//! A session owns three things:
//!
//! 1. A transcript dict (messages, events, summary, metadata, …).
//! 2. Closure subscribers that fire on agent-loop events for this session.
//! 3. Its own lifecycle (open, reset, fork, trim, compact, close).
//!
//! Storage is owned by the VM execution tree. Agent-loop workers can migrate
//! between runtime threads, so the ambient slot carries an `Arc` address while
//! the typed owner serializes transcript and subscriber mutations.
//!
//! Lifecycle is explicit. Builtins (`agent_session_open`,
//! `_reset`, `_fork`, `_fork_at`, `_close`, `_trim`, `_compact`,
//! `_inject`, `_exists`, `_length`, `_snapshot`, `_ancestry`) drive
//! the store directly — there is no "policy" config dict that
//! performs lifecycle as a side effect.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::actor_chain::ActorChain;
use crate::agent_events::{
    AgentEvent, AttachmentFlavor, AttachmentRendering, HostInjectionProvenance, InjectionDelivery,
    SanitizationAction, SanitizationVerdict, ToolCallStatus,
};
use crate::agent_transcript_budget::{
    apply_transcript_with_budget, apply_transcript_with_budget_deferred_event,
    publish_transcript_budget_event, transcript_budget_policy_json, transcript_budget_usage_json,
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
pub(crate) use journal::{active_run_id, has_journal, journal_first_event_id, journal_store};
pub(crate) use journal::{
    claim_journal_task, clear_journal, install_journal, journal_owns_session,
    journal_sessions_for_task, next_journal_event, record_persisted_journal_event,
};
const LIVE_CLIENT_EVENT_KIND: &str = "live_session_client";
const LIVE_CLIENT_PERMISSION_EVENT_KIND: &str = "live_session_permission_route";

/// Default cap on concurrent sessions per VM execution tree. At the ceiling,
/// the least-recently-accessed idle session is evicted on the next `open`.
/// Sessions with live journals are protected; creation fails if preserving
/// them leaves no room.
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

/// A new session could not be admitted without discarding a live transcript
/// journal. Existing sessions remain addressable at the ceiling; only creation
/// can fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionOpenError {
    CapacityExhausted {
        limit: usize,
        active: usize,
        protected: usize,
    },
    LineageRejected {
        session_id: String,
        reason: String,
    },
}

impl std::fmt::Display for SessionOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityExhausted {
                limit,
                active,
                protected,
            } => write!(
                formatter,
                "agent session capacity exhausted: limit={limit} active={active} protected={protected}"
            ),
            Self::LineageRejected { session_id, reason } => {
                write!(formatter, "agent session lineage rejected for `{session_id}`: {reason}")
            }
        }
    }
}

impl std::error::Error for SessionOpenError {}

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
    pub(crate) revoked_reminder_ids: HashSet<String>,
    pub(crate) expired_reminder_ids: HashSet<String>,
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
            revoked_reminder_ids: HashSet::new(),
            expired_reminder_ids: HashSet::new(),
        }
    }

    fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub(crate) fn replace_transcript(&mut self, transcript: VmValue) -> Result<(), String> {
        self.ensure_run_accepts_mutation("replace_transcript")?;
        if !crate::values_equal(&self.transcript, &transcript) {
            self.redo_stack.clear();
        }
        self.transcript = transcript;
        self.touch();
        Ok(())
    }

    /// Reject mutations once this run has queued its terminal boundary.
    /// The journal remains installed through recap projection so same-session
    /// admission stays closed, but it is sealed against work that could be
    /// queued behind the already-persisted terminal and then discarded.
    pub(crate) fn ensure_run_accepts_mutation(&self, action: &str) -> Result<(), String> {
        if self
            .transcript_journal
            .as_ref()
            .is_some_and(crate::agent_session_journal::JournalState::terminal_queued)
        {
            return Err(format!(
                "session '{}' is terminal; {action} cannot mutate it",
                self.id
            ));
        }
        Ok(())
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

pub(crate) mod event_facts;
mod host_injection;
mod live_clients;
mod metadata;
mod runtime_store;
mod scratchpad;
mod text_tool_call_seq;
mod transcript_lifecycle;
mod types;

pub(crate) use text_tool_call_seq::next_text_tool_call_seq_for_parse;
use text_tool_call_seq::{
    next_text_tool_call_seq_from_json_messages, next_text_tool_call_seq_from_transcript,
};

pub use host_injection::*;
pub use live_clients::*;
pub use metadata::*;
use metadata::{
    branch_event_index, clone_transcript_with_id, clone_transcript_with_parent, empty_transcript,
    prepare_lineage_update, session_snapshot, transcript_with_session_metadata, update_lineage,
};
#[cfg(test)]
pub(crate) use runtime_store::fresh_session_runtime;
pub(crate) use runtime_store::{
    active_session_runtime, mark_unknown_host_event_warning, swap_active_session_runtime,
    AgentSessionRuntime,
};
use runtime_store::{
    clear_unknown_host_event_warnings, DEFAULT_TRANSCRIPT_BUDGET_POLICY, SESSIONS, SESSION_CAP,
};
pub use scratchpad::*;
pub(crate) use transcript_lifecycle::append_event_to_state;
pub use transcript_lifecycle::*;
pub use types::*;

thread_local! {
    static CURRENT_SESSION_STACK: RefCell<Vec<CurrentSessionFrame>> = const { RefCell::new(Vec::new()) };
    static CURRENT_TOOL_CALL_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

static NEXT_CURRENT_SESSION_FRAME_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Opaque ownership receipt for one ambient current-session push.
///
/// Session ids are deliberately reusable by nested and resumed loops. Cleanup
/// therefore consumes this unique frame receipt rather than searching by the
/// duplicate-capable public id and accidentally removing a sibling owner.
#[derive(Clone, Debug)]
pub(crate) struct CurrentSessionFrame {
    frame_id: u64,
    session_id: String,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl CurrentSessionFrame {
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }

    fn revoke(&self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl PartialEq for CurrentSessionFrame {
    fn eq(&self, other: &Self) -> bool {
        self.frame_id == other.frame_id
    }
}

impl Eq for CurrentSessionFrame {}

tokio::task_local! {
    static CURRENT_TOOL_CALL_TASK: String;
}
pub struct CurrentSessionGuard {
    frame: Option<CurrentSessionFrame>,
}

impl Drop for CurrentSessionGuard {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            remove_current_session(frame);
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

/// Clear the calling thread's session store and its process-global sidecars.
pub fn reset_session_store() {
    let mut owned_session_ids: HashSet<String> =
        SESSIONS.with(|s| s.borrow_mut().drain().map(|(id, _)| id).collect());
    CURRENT_SESSION_STACK.with(|stack| {
        owned_session_ids.extend(stack.borrow_mut().drain(..).map(|frame| frame.session_id));
    });
    CURRENT_TOOL_CALL_STACK.with(|stack| stack.borrow_mut().clear());
    for session_id in owned_session_ids {
        clear_session_changed_paths(&session_id);
    }
    active_session_runtime()
        .unknown_host_event_warnings
        .borrow_mut()
        .clear();
    reset_default_transcript_budget_policy();
}

pub(crate) fn new_current_session_frame(id: String) -> Option<CurrentSessionFrame> {
    if id.is_empty() {
        return None;
    }
    Some(CurrentSessionFrame {
        frame_id: NEXT_CURRENT_SESSION_FRAME_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        session_id: id,
        active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    })
}

pub(crate) fn push_current_session(id: String) -> Option<CurrentSessionFrame> {
    let frame = new_current_session_frame(id)?;
    CURRENT_SESSION_STACK.with(|stack| stack.borrow_mut().push(frame.clone()));
    Some(frame)
}

pub(crate) fn swap_current_session_stack(
    replacement: Vec<CurrentSessionFrame>,
) -> Vec<CurrentSessionFrame> {
    CURRENT_SESSION_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), replacement))
}

#[cfg(test)]
pub(crate) fn pop_current_session() {
    CURRENT_SESSION_STACK.with(|stack| {
        if let Some(frame) = stack.borrow_mut().pop() {
            frame.revoke();
        }
    });
}

/// Remove one exact ambient session without disturbing newer nested work.
///
/// Async session setup and teardown may yield while another session is active
/// on the same worker thread. A blind stack pop can then remove the wrong
/// owner. Exact removal makes rollback and finalization idempotent and preserves
/// unrelated ambient state.
pub(crate) fn remove_current_session(frame: CurrentSessionFrame) -> bool {
    frame.revoke();
    CURRENT_SESSION_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let Some(index) = stack
            .iter()
            .position(|candidate| candidate.frame_id == frame.frame_id)
        else {
            return false;
        };
        stack.remove(index);
        true
    })
}

pub fn current_session_id() -> Option<String> {
    CURRENT_SESSION_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find(|frame| frame.is_active())
            .map(|frame| frame.session_id.clone())
    })
}

pub fn current_actor_chain() -> Option<ActorChain> {
    current_session_id().as_deref().and_then(actor_chain)
}

pub fn enter_current_session(id: impl Into<String>) -> CurrentSessionGuard {
    let id = id.into();
    if id.trim().is_empty() {
        return CurrentSessionGuard { frame: None };
    }
    CurrentSessionGuard {
        frame: push_current_session(id),
    }
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

/// Index the next message injected into this session will take.
///
/// Used to stamp a dispatch-time receipt with the assistant turn it belongs
/// to: the calls being dispatched were parsed from the message immediately
/// before this index. Returns `None` for an unknown session.
pub fn next_message_index(id: &str) -> Option<usize> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let state = map.get(id)?;
        let messages = state
            .transcript
            .as_dict()
            .and_then(|dict| dict.get("messages"))
            .and_then(|value| match value {
                VmValue::List(list) => Some(list.len()),
                _ => None,
            })
            .unwrap_or(0);
        Some(messages)
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
pub fn open_or_create(id: Option<String>) -> Result<String, SessionOpenError> {
    open_or_create_with_actor_chain(id, None)
}

#[cfg(test)]
pub(crate) fn open_or_create_for_test(id: Option<String>) -> String {
    open_or_create(id).expect("open fixture session")
}

#[cfg(test)]
pub(crate) fn open_or_create_with_actor_chain_for_test(
    id: Option<String>,
    requested_actor_chain: Option<ActorChain>,
) -> String {
    open_or_create_with_actor_chain(id, requested_actor_chain).expect("open fixture session")
}

pub fn open_or_create_with_actor_chain(
    id: Option<String>,
    requested_actor_chain: Option<ActorChain>,
) -> Result<String, SessionOpenError> {
    let resolved = id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let parent_session = current_session_id();
    let inherited_actor_chain = requested_actor_chain
        .clone()
        .or_else(|| parent_session.as_deref().and_then(actor_chain));
    let mut was_new = false;
    let mut evicted = Vec::new();
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(state) = map.get_mut(&resolved) {
            if let Some(actor_chain) = requested_actor_chain.clone() {
                state.actor_chain = Some(actor_chain);
            }
            state.touch();
            return Ok(());
        }
        let cap = SESSION_CAP.with(|c| c.get());
        let required_evictions = map.len().saturating_add(1).saturating_sub(cap);
        if required_evictions > 0 {
            let mut victims = map
                .iter()
                .filter(|(_, state)| state.transcript_journal.is_none())
                .map(|(id, state)| (id.clone(), state.last_accessed))
                .collect::<Vec<_>>();
            if victims.len() < required_evictions {
                return Err(SessionOpenError::CapacityExhausted {
                    limit: cap,
                    active: map.len(),
                    protected: map
                        .values()
                        .filter(|state| state.transcript_journal.is_some())
                        .count(),
                });
            }
            victims.sort_by_key(|(_, last_accessed)| *last_accessed);
            for (victim, _) in victims.into_iter().take(required_evictions) {
                map.remove(&victim);
                evicted.push(victim);
            }
        }
        was_new = true;
        let mut state = SessionState::new(resolved.clone());
        state.actor_chain = inherited_actor_chain.clone();
        map.insert(resolved.clone(), state);
        Ok(())
    })?;
    for evicted in evicted {
        clear_session_changed_paths(&evicted);
    }
    if was_new {
        // A prior owner may have been abandoned before its receipt drained.
        // Opening a fresh session with the same id starts a fresh receipt.
        clear_session_changed_paths(&resolved);
        if let Some(parent) = parent_session.as_deref() {
            crate::agent_events::mirror_session_sinks(parent, &resolved);
        }
        try_register_event_log(&resolved);
    }
    Ok(resolved)
}

pub fn open_child_session(parent_id: &str, id: Option<String>) -> Result<String, SessionOpenError> {
    open_child_session_with_actor(parent_id, id, None)
}

pub fn open_child_session_with_actor(
    parent_id: &str,
    id: Option<String>,
    actor: Option<&str>,
) -> Result<String, SessionOpenError> {
    let actor_chain = actor_chain(parent_id).map(|chain| match actor {
        Some(actor) if !actor.trim().is_empty() => chain.pushed(actor.trim()),
        _ => chain,
    });
    let resolved = id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    admit_linked_sessions(parent_id, &resolved, actor_chain, None)?;
    Ok(resolved)
}

pub fn link_child_session(parent_id: &str, child_id: &str) -> Result<(), SessionOpenError> {
    link_child_session_with_branch(parent_id, child_id, None)
}

pub fn link_child_session_with_branch(
    parent_id: &str,
    child_id: &str,
    branched_at_event_index: Option<usize>,
) -> Result<(), SessionOpenError> {
    if parent_id == child_id {
        return Ok(());
    }
    admit_linked_sessions(parent_id, child_id, None, branched_at_event_index)
}

fn admit_linked_sessions(
    parent_id: &str,
    child_id: &str,
    requested_child_actor_chain: Option<ActorChain>,
    branched_at_event_index: Option<usize>,
) -> Result<(), SessionOpenError> {
    let ambient_parent = current_session_id();
    let inherited_actor_chain = ambient_parent.as_deref().and_then(actor_chain);
    let child_actor_chain = requested_child_actor_chain
        .clone()
        .or_else(|| inherited_actor_chain.clone());
    let mut evicted = Vec::new();
    let mut created = Vec::new();
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let lineage = prepare_lineage_update(&map, parent_id, child_id)?;
        let missing =
            usize::from(!map.contains_key(parent_id)) + usize::from(!map.contains_key(child_id));
        let cap = SESSION_CAP.with(|limit| limit.get());
        let required_evictions = if missing == 0 {
            0
        } else {
            map.len().saturating_add(missing).saturating_sub(cap)
        };
        if required_evictions > 0 {
            let mut victims = map
                .iter()
                .filter(|(id, state)| {
                    id.as_str() != parent_id
                        && id.as_str() != child_id
                        && state.transcript_journal.is_none()
                })
                .map(|(id, state)| (id.clone(), state.last_accessed))
                .collect::<Vec<_>>();
            if victims.len() < required_evictions {
                let protected = map
                    .iter()
                    .filter(|(id, state)| {
                        id.as_str() == parent_id
                            || id.as_str() == child_id
                            || state.transcript_journal.is_some()
                    })
                    .count();
                return Err(SessionOpenError::CapacityExhausted {
                    limit: cap,
                    active: map.len(),
                    protected,
                });
            }
            victims.sort_by_key(|(_, last_accessed)| *last_accessed);
            for (victim, _) in victims.into_iter().take(required_evictions) {
                map.remove(&victim);
                evicted.push(victim);
            }
        }
        if !map.contains_key(parent_id) {
            let mut parent = SessionState::new(parent_id.to_string());
            parent.actor_chain = inherited_actor_chain.clone();
            map.insert(parent_id.to_string(), parent);
            created.push(parent_id.to_string());
        }
        if !map.contains_key(child_id) {
            let mut child = SessionState::new(child_id.to_string());
            child.actor_chain = child_actor_chain.clone();
            map.insert(child_id.to_string(), child);
            created.push(child_id.to_string());
        } else if let Some(actor_chain) = requested_child_actor_chain {
            if let Some(child) = map.get_mut(child_id) {
                child.actor_chain = Some(actor_chain);
            }
        }
        lineage.commit(&mut map, parent_id, child_id, branched_at_event_index);
        Ok(())
    })?;
    for id in evicted {
        clear_session_changed_paths(&id);
    }
    for id in created {
        clear_session_changed_paths(&id);
        if let Some(parent) = ambient_parent.as_deref() {
            crate::agent_events::mirror_session_sinks(parent, &id);
        }
        try_register_event_log(&id);
    }
    Ok(())
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

/// Close an idle session.
///
/// A live journal owns an unfinished persistence obligation. Dropping that
/// state here would also drop its writer lease and lifecycle reservation, so
/// callers must finalize or explicitly clear the journal before closing.
pub fn close(id: &str) -> bool {
    let (removed, active_run) = SESSIONS.with(|s| {
        let mut sessions = s.borrow_mut();
        if sessions
            .get(id)
            .is_some_and(|state| state.transcript_journal.is_some())
        {
            return (false, true);
        }
        (sessions.remove(id).is_some(), false)
    });
    if active_run {
        crate::events::log_warn(
            "agent.session_close_refused",
            &format!("session={id} active journal must be finalized before close"),
        );
        return false;
    }
    if removed {
        clear_session_changed_paths(id);
    }
    // Cross-thread per-session state must be released too, otherwise
    // pending inbox entries can be delivered to a future session that
    // happens to reuse the same id.
    crate::orchestration::agent_inbox::clear_session(id);
    crate::agent_events::clear_session_sinks(id);
    clear_unknown_host_event_warnings(id);
    removed
}

pub fn close_with_status(
    id: &str,
    reason: impl Into<String>,
    status: impl Into<String>,
    metadata: serde_json::Value,
) -> Result<bool, String> {
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
    validate_session_event(&transcript_event, "agent_session_close")?;
    let removed = SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(id) else {
            return Ok(false);
        };
        if state.transcript_journal.is_some() {
            return Err(format!(
                "agent session '{id}' has an active run; finalize it before closing"
            ));
        }
        append_event_to_state(state, transcript_event, "close_with_status")?;
        sessions.remove(id);
        Ok(true)
    })?;
    if !removed {
        return Ok(false);
    }
    clear_session_changed_paths(id);
    crate::orchestration::agent_inbox::clear_session(id);
    clear_unknown_host_event_warnings(id);
    crate::llm::emit_live_agent_event_sync(&crate::agent_events::AgentEvent::SessionClosed {
        session_id: id.to_string(),
        reason,
        status,
        metadata,
    });
    crate::agent_events::clear_session_sinks(id);
    Ok(true)
}

pub fn reset_transcript(id: &str) -> bool {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return false;
        };
        if state.replace_transcript(empty_transcript(id)).is_err() {
            return false;
        }
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
pub fn fork(src_id: &str, dst_id: Option<String>) -> Result<Option<String>, SessionOpenError> {
    let dst = dst_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    if dst == src_id {
        return Ok(None);
    }
    let ambient_parent = current_session_id();
    let mut evicted = Vec::new();
    let mut budget_event = None;
    let forked = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(src) = map.get(src_id) else {
            return Ok(false);
        };
        // A fork destination is a new session. Refusing an occupied destination
        // prevents a compound admission from overwriting an unrelated live run.
        if map.contains_key(&dst) {
            return Ok(false);
        }

        let cap = SESSION_CAP.with(|limit| limit.get());
        let required_evictions = map.len().saturating_add(1).saturating_sub(cap);
        let mut victims = map
            .iter()
            .filter(|(id, state)| id.as_str() != src_id && state.transcript_journal.is_none())
            .map(|(id, state)| (id.clone(), state.last_accessed))
            .collect::<Vec<_>>();
        if victims.len() < required_evictions {
            let protected = map
                .iter()
                .filter(|(id, state)| id.as_str() == src_id || state.transcript_journal.is_some())
                .count();
            return Err(SessionOpenError::CapacityExhausted {
                limit: cap,
                active: map.len(),
                protected,
            });
        }

        let mut state = SessionState::new(dst.clone());
        state.transcript =
            clone_transcript_with_parent(&clone_transcript_with_id(&src.transcript, &dst), src_id);
        state.tool_format = src.tool_format.clone();
        state.system_prompt = src.system_prompt.clone();
        state.pinned_model = src.pinned_model.clone();
        state.pinned_reasoning_policy = src.pinned_reasoning_policy.clone();
        state.actor_chain = src.actor_chain.clone();
        state.workspace_anchor = src.workspace_anchor.clone();
        state.workspace_policy = src.workspace_policy.clone();
        state.scratchpad = src.scratchpad.clone();
        state.scratchpad_version = src.scratchpad_version;
        state.transcript_budget_policy = src.transcript_budget_policy.clone();
        state.last_transcript_budget_action = src.last_transcript_budget_action.clone();
        state.text_tool_call_seq = src.text_tool_call_seq;
        state.taint = src.taint.clone();
        state.parent_id = Some(src_id.to_string());
        let candidate = state.transcript.clone();
        budget_event =
            match apply_transcript_with_budget_deferred_event(&mut state, candidate, "fork") {
                Ok(event) => event,
                Err(_) => return Ok(false),
            };

        victims.sort_by_key(|(_, last_accessed)| *last_accessed);
        for (victim, _) in victims.into_iter().take(required_evictions) {
            map.remove(&victim);
            evicted.push(victim);
        }
        map.insert(dst.clone(), state);
        map.get_mut(src_id)
            .expect("fork source remains protected during atomic admission")
            .touch();
        update_lineage(&mut map, src_id, &dst, None)?;
        Ok(true)
    })?;
    if !forked {
        return Ok(None);
    }
    for id in evicted {
        clear_session_changed_paths(&id);
    }
    clear_session_changed_paths(&dst);
    if let Some(parent) = ambient_parent.as_deref() {
        crate::agent_events::mirror_session_sinks(parent, &dst);
    }
    try_register_event_log(&dst);
    publish_transcript_budget_event(budget_event);
    Ok(Some(dst))
}

/// Fork `src_id` and truncate the destination transcript to the
/// first `keep_first` messages (#105 — branch-replay). Pairs with the
/// scrubber: the host picks an event index, rebuilds a message count,
/// and calls this to spawn a live sibling session that resumes from
/// the rebuilt state. Subscribers are not carried over (same as
/// `fork`), so sibling events don't double-fan into the parent's
/// consumers.
///
/// Returns the new session id on success, `Ok(None)` if `src_id` doesn't
/// exist, and an admission error if no idle session can make room.
pub fn fork_at(
    src_id: &str,
    keep_first: usize,
    dst_id: Option<String>,
) -> Result<Option<String>, SessionOpenError> {
    let Some(branched_at_event_index) = SESSIONS.with(|s| {
        let map = s.borrow();
        let src = map.get(src_id)?;
        Some(branch_event_index(&src.transcript, keep_first))
    }) else {
        return Ok(None);
    };
    let Some(new_id) = fork(src_id, dst_id)? else {
        return Ok(None);
    };
    link_child_session_with_branch(src_id, &new_id, Some(branched_at_event_index))?;
    if truncate(&new_id, keep_first).ok().flatten().is_none() {
        return Ok(None);
    }
    Ok(Some(new_id))
}

mod truncation;
use truncation::truncate_state;
pub use truncation::{trim, truncate};

mod pop_last_assistant;
pub use pop_last_assistant::pop_last_if_assistant;
mod restore_message_event_ids;
pub(crate) use restore_message_event_ids::restore_message_event_ids;

/// Append a message dict to the session transcript. The message must
/// have at least a string `role`; anything else is merged verbatim.
///
/// Returns the index the message took in the session's message list. Callers
/// that record a typed receipt for the message they just injected (tool
/// results, which must name the call they answer) use it to bind the receipt
/// to the exact message rather than to "whatever came next".
pub fn inject_message(id: &str, message: VmValue) -> Result<usize, String> {
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
        Ok(message_index)
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
    open_or_create(Some(resolved.clone())).map_err(|error| error.to_string())?;
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

pub fn append_subscriber(id: &str, callback: VmValue) -> Result<(), SessionOpenError> {
    open_or_create(Some(id.to_string()))?;
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.subscribers.push(callback);
            state.touch();
        }
    });
    Ok(())
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
