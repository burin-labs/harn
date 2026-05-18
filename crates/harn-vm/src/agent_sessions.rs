//! First-class session storage.
//!
//! A session owns three things:
//!
//! 1. A transcript dict (messages, events, summary, metadata, …).
//! 2. Closure subscribers that fire on agent-loop events for this session.
//! 3. Its own lifecycle (open, reset, fork, trim, compact, close).
//!
//! Storage is thread-local because `VmValue` contains `Rc`, which is
//! neither `Send` nor `Sync`. The agent loop runs on a tokio
//! current-thread worker, so all session reads and writes happen on the
//! same thread. The closure-subscribers register, fire, and unregister
//! on that same thread.
//!
//! Lifecycle is explicit. Builtins (`agent_session_open`,
//! `_reset`, `_fork`, `_fork_at`, `_close`, `_trim`, `_compact`,
//! `_inject`, `_exists`, `_length`, `_snapshot`, `_ancestry`) drive
//! the store directly — there is no "policy" config dict that
//! performs lifecycle as a side effect.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::time::Instant;

use crate::value::VmValue;

/// Default cap on concurrent sessions per VM thread. Beyond this the
/// least-recently-accessed session is evicted on the next `open`.
pub const DEFAULT_SESSION_CAP: usize = 128;

pub struct SessionState {
    pub id: String,
    pub transcript: VmValue,
    pub subscribers: Vec<VmValue>,
    pub created_at: String,
    pub last_accessed: Instant,
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub branched_at_event_index: Option<usize>,
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
}

impl SessionState {
    fn new(id: String) -> Self {
        let now = Instant::now();
        let transcript = empty_transcript(&id);
        Self {
            id,
            transcript,
            subscribers: Vec::new(),
            created_at: crate::orchestration::now_rfc3339(),
            last_accessed: now,
            parent_id: None,
            child_ids: Vec::new(),
            branched_at_event_index: None,
            active_skills: Vec::new(),
            tool_format: None,
            system_prompt: None,
            pinned_model: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAncestry {
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub root_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTruncateResult {
    pub kept_turn_count: usize,
    pub removed_turn_count: usize,
    pub new_tip_turn_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderInjectionReport {
    pub reminder_id: String,
    pub deduped_count: usize,
}

thread_local! {
    static SESSIONS: RefCell<HashMap<String, SessionState>> = RefCell::new(HashMap::new());
    static SESSION_CAP: Cell<usize> = const { Cell::new(DEFAULT_SESSION_CAP) };
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

/// Clear the session store. Wired into `reset_llm_state` for test isolation.
pub fn reset_session_store() {
    SESSIONS.with(|s| s.borrow_mut().clear());
    CURRENT_SESSION_STACK.with(|stack| stack.borrow_mut().clear());
    CURRENT_TOOL_CALL_STACK.with(|stack| stack.borrow_mut().clear());
}

pub(crate) fn push_current_session(id: String) {
    if id.is_empty() {
        return;
    }
    CURRENT_SESSION_STACK.with(|stack| stack.borrow_mut().push(id));
}

pub(crate) fn pop_current_session() {
    CURRENT_SESSION_STACK.with(|stack| {
        let _ = stack.borrow_mut().pop();
    });
}

pub fn current_session_id() -> Option<String> {
    CURRENT_SESSION_STACK.with(|stack| stack.borrow().last().cloned())
}

pub fn enter_current_session(id: impl Into<String>) -> CurrentSessionGuard {
    let id = id.into();
    if id.trim().is_empty() {
        return CurrentSessionGuard { active: false };
    }
    push_current_session(id);
    CurrentSessionGuard { active: true }
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

/// Return the canonical transcript surface for APIs whose result field is
/// named `transcript`. Session-only fields stay on `agent_session_snapshot`.
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
    let resolved = id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let parent_session = current_session_id();
    let mut was_new = false;
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(state) = map.get_mut(&resolved) {
            state.last_accessed = Instant::now();
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
        map.insert(resolved.clone(), SessionState::new(resolved.clone()));
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
    let resolved = open_or_create(id);
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
        Some(event_metadata.clone()),
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
        state.last_accessed = Instant::now();
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
    let (src_transcript, src_tool_format, src_system_prompt, src_pinned_model, dst) = SESSIONS
        .with(|s| {
            let mut map = s.borrow_mut();
            let src = map.get_mut(src_id)?;
            src.last_accessed = Instant::now();
            let dst = dst_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            let forked_transcript = clone_transcript_with_id(&src.transcript, &dst);
            Some((
                forked_transcript,
                src.tool_format.clone(),
                src.system_prompt.clone(),
                src.pinned_model.clone(),
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
            state.last_accessed = Instant::now();
        }
        update_lineage(&mut map, src_id, &dst, None);
    });
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
        Some(truncate_state(state, keep_first))
    })
}

fn truncate_state(state: &mut SessionState, keep_first: usize) -> SessionTruncateResult {
    let dict = state
        .transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(BTreeMap::new);
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
            "events".to_string(),
            VmValue::List(Rc::new(retained_events)),
        );
        next.insert("messages".to_string(), VmValue::List(Rc::new(retained)));
        next.remove("summary");
        state.transcript = VmValue::Dict(Rc::new(next));
    }
    state.last_accessed = Instant::now();
    SessionTruncateResult {
        kept_turn_count,
        removed_turn_count,
        new_tip_turn_id,
    }
}

/// Retain only the last `keep_last` messages in the session transcript.
/// Returns the kept count (<= keep_last).
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
            "events".to_string(),
            VmValue::List(Rc::new(
                crate::llm::helpers::transcript_events_from_messages(&retained),
            )),
        );
        next.insert("messages".to_string(), VmValue::List(Rc::new(retained)));
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
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
            .unwrap_or_else(BTreeMap::new);
        let mut messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let mut events: Vec<VmValue> = match dict.get("events") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => crate::llm::helpers::transcript_events_from_messages(&messages),
        };
        let new_message = VmValue::Dict(Rc::new(msg_dict));
        emit_llm_message_event(id, messages.len(), &new_message);
        events.push(crate::llm::helpers::transcript_event_from_message(
            &new_message,
        ));
        messages.push(new_message);
        let mut next = dict;
        next.insert("events".to_string(), VmValue::List(Rc::new(events)));
        next.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
        Ok(())
    })
}

fn emit_llm_message_event(session_id: &str, message_index: usize, message: &VmValue) {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );
    fields.insert(
        "message_index".to_string(),
        serde_json::json!(message_index),
    );
    let message_json = crate::llm::helpers::vm_value_to_json(message);
    if let Some(role) = message_json.get("role").and_then(|value| value.as_str()) {
        fields.insert(
            "role".to_string(),
            serde_json::Value::String(role.to_string()),
        );
    }
    if let Some(content) = message_json.get("content") {
        fields.insert("content".to_string(), content.clone());
    }
    fields.insert("message".to_string(), message_json);
    crate::llm::append_observability_sidecar_entry("message", fields);
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
        let vm_messages = crate::llm::helpers::json_messages_to_vm(messages);
        state.transcript = crate::llm::helpers::new_transcript_with(
            Some(resolved.clone()),
            vm_messages,
            None,
            Some(crate::stdlib::json_to_vm_value(&serde_json::Value::Object(
                metadata,
            ))),
        );
        state.last_accessed = Instant::now();
        Ok(resolved)
    })
}

/// Load the messages vec (as JSON) for this session, for use as prefix
/// to an agent_loop run. Returns an empty vec if the session doesn't
/// exist or has no messages.
pub fn messages_json(id: &str) -> Vec<serde_json::Value> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Vec::new();
        };
        let Some(dict) = state.transcript.as_dict() else {
            return Vec::new();
        };
        match dict.get("messages") {
            Some(VmValue::List(list)) => list
                .iter()
                .map(crate::llm::helpers::vm_value_to_json)
                .collect(),
            _ => Vec::new(),
        }
    })
}

#[derive(Clone, Debug, Default)]
pub struct SessionPromptState {
    pub messages: Vec<serde_json::Value>,
    pub summary: Option<String>,
}

fn summary_message_json(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": summary,
    })
}

fn messages_begin_with_summary(messages: &[serde_json::Value], summary: &str) -> bool {
    messages.first().is_some_and(|message| {
        message.get("role").and_then(|value| value.as_str()) == Some("user")
            && message.get("content").and_then(|value| value.as_str()) == Some(summary)
    })
}

/// Prompt-surface resume state for a persisted session.
///
/// Returns the compacted/rehydratable message list plus the transcript's
/// summary field. When the transcript carries a summary field but its
/// message list does not already begin with the compacted summary
/// message, this helper prepends one so session re-entry preserves the
/// same prompt surface the previous loop was actually using.
pub fn prompt_state_json(id: &str) -> SessionPromptState {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return SessionPromptState::default();
        };
        let Some(dict) = state.transcript.as_dict() else {
            return SessionPromptState::default();
        };
        let mut messages = match dict.get("messages") {
            Some(VmValue::List(list)) => list
                .iter()
                .map(crate::llm::helpers::vm_value_to_json)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let summary = dict.get("summary").and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        });
        if let Some(summary_text) = summary.as_deref() {
            if !messages_begin_with_summary(&messages, summary_text) {
                messages.insert(0, summary_message_json(summary_text));
            }
        }
        SessionPromptState { messages, summary }
    })
}

/// Overwrite the transcript for this session. Used by `agent_loop` on
/// exit to persist the synthesized transcript.
pub fn store_transcript(id: &str, transcript: VmValue) {
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.transcript = transcript_with_session_metadata(transcript, state);
            state.last_accessed = Instant::now();
        }
    });
}

/// Apply the reminder TTL lifecycle that runs once per completed agent
/// turn. Reminders with `ttl_turns = 1` expire and are removed; larger
/// finite TTLs are decremented in place. Expiry audit events are emitted
/// to the active EventLog when one is installed.
pub fn apply_reminder_post_turn(id: &str, turn: i64) -> Result<serde_json::Value, String> {
    let report = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_apply_reminder_post_turn: unknown session id '{id}'"
            ));
        };
        let report = crate::llm::helpers::apply_reminder_post_turn(&state.transcript, turn);
        if report.decremented_count > 0 || !report.expired.is_empty() {
            if let Some(next) = report.transcript.clone() {
                state.transcript = next;
            }
            state.last_accessed = Instant::now();
        }
        Ok(report)
    })?;

    for reminder in &report.expired {
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_EXPIRED_EVENT_KIND,
            serde_json::json!({
                "transcript_id": id,
                "reminder_id": &reminder.id,
                "tags": &reminder.tags,
                "dedupe_key": &reminder.dedupe_key,
                "ttl_turns_before": &reminder.ttl_turns,
                "expired_at_turn": turn,
            }),
        );
    }

    Ok(serde_json::json!({
        "expired_count": report.expired.len(),
        "decremented_count": report.decremented_count,
        "remaining_count": report.remaining_count,
    }))
}

/// Inject a typed system reminder into the session transcript's event
/// stream. This mirrors `transcript.inject_reminder` for live sessions:
/// reminders with the same `dedupe_key` are replaced before the new
/// reminder event is appended.
pub fn inject_reminder(
    id: &str,
    reminder: crate::llm::helpers::SystemReminder,
) -> Result<ReminderInjectionReport, String> {
    let reminder_id = reminder.id.clone();
    let dedupe_key = reminder.dedupe_key.clone();
    let mut deduped_reminder_ids = Vec::new();
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_session_inject_reminder: unknown session id '{id}'"
            ));
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
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
        if let Some(expected_key) = dedupe_key.as_deref() {
            events.retain(|event| {
                let Some(existing) = crate::llm::helpers::reminder_from_event(event) else {
                    return true;
                };
                if existing.dedupe_key.as_deref() == Some(expected_key) {
                    deduped_reminder_ids.push(existing.id);
                    false
                } else {
                    true
                }
            });
        }
        events.push(crate::llm::helpers::transcript_reminder_event(&reminder));
        let mut next = dict;
        next.insert("events".to_string(), VmValue::List(Rc::new(events)));
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
        Ok(())
    })?;

    if !deduped_reminder_ids.is_empty() {
        let dropped_count = deduped_reminder_ids.len();
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_DEDUPED_EVENT_KIND,
            serde_json::json!({
                "transcript_id": id,
                "reminder_id": &reminder_id,
                "dedupe_key": &dedupe_key,
                "dropped_reminder_ids": &deduped_reminder_ids,
                "dropped_count": dropped_count,
            }),
        );
    }

    Ok(ReminderInjectionReport {
        reminder_id,
        deduped_count: deduped_reminder_ids.len(),
    })
}

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
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
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
        next.insert("events".to_string(), VmValue::List(Rc::new(events)));
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
        Ok(())
    })
}

/// Replace the transcript's message list wholesale. Used by the
/// in-loop compaction path, which operates on JSON messages.
pub fn replace_messages(id: &str, messages: &[serde_json::Value]) {
    replace_messages_with_summary(id, messages, None);
}

/// Replace the transcript's message list and optionally update the
/// `summary` field on the persisted transcript. The compaction path
/// uses this to publish the human-readable rollup line that
/// `transcript_summary(transcript)` exposes to host code.
pub fn replace_messages_with_summary(
    id: &str,
    messages: &[serde_json::Value],
    summary: Option<&str>,
) {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return;
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
        let vm_messages: Vec<VmValue> = messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect();
        let mut next = dict;
        next.insert(
            "events".to_string(),
            VmValue::List(Rc::new(
                crate::llm::helpers::transcript_events_from_messages(&vm_messages),
            )),
        );
        next.insert("messages".to_string(), VmValue::List(Rc::new(vm_messages)));
        if let Some(summary) = summary {
            next.insert(
                "summary".to_string(),
                VmValue::String(Rc::from(summary.to_string())),
            );
        }
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
    });
}

pub fn append_subscriber(id: &str, callback: VmValue) {
    open_or_create(Some(id.to_string()));
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.subscribers.push(callback);
            state.last_accessed = Instant::now();
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
pub fn set_active_skills(id: &str, skills: Vec<String>) {
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.active_skills = skills;
            state.last_accessed = Instant::now();
        }
    });
}

/// Skills that were active at the end of the previous agent_loop run
/// against this session. Returns an empty vec when the session doesn't
/// exist or nothing was persisted.
pub fn active_skills(id: &str) -> Vec<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.active_skills.clone())
            .unwrap_or_default()
    })
}

/// Claim the tool-calling contract for a session.
///
/// The first loop against a named session records its `tool_format`.
/// Later re-entry must use the same format so prompt/history generated
/// under a text contract is never replayed as native, or vice versa.
pub fn claim_tool_format(id: &str, tool_format: &str) -> Result<(), String> {
    let tool_format = tool_format.trim();
    if tool_format.is_empty() {
        return Ok(());
    }
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        match state.tool_format.as_deref() {
            Some(existing) if existing != tool_format => Err(format!(
                "agent session '{id}' is locked to tool_format='{existing}', but this run requested tool_format='{tool_format}'. Start a new session or fork/reset the transcript before changing tool mode."
            )),
            Some(_) => {
                state.last_accessed = Instant::now();
                Ok(())
            }
            None => {
                state.tool_format = Some(tool_format.to_string());
                state.last_accessed = Instant::now();
                Ok(())
            }
        }
    })
}

pub fn tool_format(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.tool_format.clone())
    })
}

pub fn record_system_prompt(id: &str, system_prompt: &str) -> Result<(), String> {
    let system_prompt = system_prompt.trim();
    if system_prompt.is_empty() {
        return Ok(());
    }
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.system_prompt.as_deref() != Some(system_prompt);
        state.system_prompt = Some(system_prompt.to_string());
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(BTreeMap::new);
        let mut next = dict;
        apply_system_prompt_metadata(&mut next, system_prompt);
        if changed {
            let mut events: Vec<VmValue> = match next.get("events") {
                Some(VmValue::List(list)) => list.iter().cloned().collect(),
                _ => Vec::new(),
            };
            events.push(crate::llm::helpers::transcript_event(
                "system_prompt",
                "system",
                "internal",
                "",
                Some(crate::llm::helpers::system_prompt_event_metadata(
                    system_prompt,
                )),
            ));
            next.insert("events".to_string(), VmValue::List(Rc::new(events)));
        }
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
        Ok(())
    })
}

pub fn system_prompt(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.system_prompt.clone())
    })
}

/// Pin (or clear, with `None`) a model selector on a session. Returns
/// `Ok(true)` when the value actually changed so callers can decide
/// whether to broadcast a notification. The selector is stored verbatim
/// — alias / catalog resolution is the call-site's job.
pub fn set_pinned_model(id: &str, model: Option<String>) -> Result<bool, String> {
    let normalized = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.pinned_model != normalized;
        state.pinned_model = normalized;
        state.last_accessed = Instant::now();
        Ok(changed)
    })
}

/// Read the session's pinned model selector, if any. Consumed by
/// `vm_resolve_model` as the per-session default when a script-level
/// `llm_call` does not pass `model:` explicitly.
pub fn pinned_model(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.pinned_model.clone())
    })
}

fn empty_transcript(id: &str) -> VmValue {
    use crate::llm::helpers::new_transcript_with;
    new_transcript_with(Some(id.to_string()), Vec::new(), None, None)
}

fn clone_transcript_with_id(transcript: &VmValue, new_id: &str) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return empty_transcript(new_id);
    };
    let mut next = dict.clone();
    next.insert(
        "id".to_string(),
        VmValue::String(Rc::from(new_id.to_string())),
    );
    VmValue::Dict(Rc::new(next))
}

fn clone_transcript_with_parent(transcript: &VmValue, parent_id: &str) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript.clone();
    };
    let mut next = dict.clone();
    let metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => {
            let mut metadata = metadata.as_ref().clone();
            metadata.insert(
                "parent_session_id".to_string(),
                VmValue::String(Rc::from(parent_id.to_string())),
            );
            VmValue::Dict(Rc::new(metadata))
        }
        _ => VmValue::Dict(Rc::new(BTreeMap::from([(
            "parent_session_id".to_string(),
            VmValue::String(Rc::from(parent_id.to_string())),
        )]))),
    };
    next.insert("metadata".to_string(), metadata);
    VmValue::Dict(Rc::new(next))
}

fn apply_system_prompt_metadata(next: &mut BTreeMap<String, VmValue>, system_prompt: &str) {
    let mut metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => metadata.as_ref().clone(),
        _ => BTreeMap::new(),
    };
    metadata.insert(
        "system_prompt".to_string(),
        crate::stdlib::json_to_vm_value(&crate::llm::helpers::system_prompt_metadata(
            system_prompt,
        )),
    );
    next.insert("metadata".to_string(), VmValue::Dict(Rc::new(metadata)));
}

fn transcript_with_session_metadata(transcript: VmValue, state: &SessionState) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript;
    };
    let mut next = dict.clone();
    let mut metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => metadata.as_ref().clone(),
        _ => BTreeMap::new(),
    };
    if let Some(tool_format) = state.tool_format.as_ref() {
        metadata.insert(
            "tool_format".to_string(),
            VmValue::String(Rc::from(tool_format.clone())),
        );
        metadata.insert("tool_mode_locked".to_string(), VmValue::Bool(true));
    }
    if let Some(system_prompt) = state.system_prompt.as_ref() {
        metadata.insert(
            "system_prompt".to_string(),
            crate::stdlib::json_to_vm_value(&crate::llm::helpers::system_prompt_metadata(
                system_prompt,
            )),
        );
    }
    if !metadata.is_empty() {
        next.insert("metadata".to_string(), VmValue::Dict(Rc::new(metadata)));
    }
    VmValue::Dict(Rc::new(next))
}

fn session_snapshot(state: &SessionState) -> VmValue {
    let transcript = transcript_with_session_metadata(state.transcript.clone(), state);
    let Some(dict) = transcript.as_dict() else {
        return state.transcript.clone();
    };
    let mut next = dict.clone();
    let length = next
        .get("messages")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list.len() as i64),
            _ => None,
        })
        .unwrap_or(0);
    next.insert("length".to_string(), VmValue::Int(length));
    next.insert(
        "created_at".to_string(),
        VmValue::String(Rc::from(state.created_at.clone())),
    );
    next.insert(
        "parent_id".to_string(),
        state
            .parent_id
            .as_ref()
            .map(|id| VmValue::String(Rc::from(id.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        "child_ids".to_string(),
        VmValue::List(Rc::new(
            state
                .child_ids
                .iter()
                .cloned()
                .map(|id| VmValue::String(Rc::from(id)))
                .collect(),
        )),
    );
    next.insert(
        "branched_at_event_index".to_string(),
        state
            .branched_at_event_index
            .map(|index| VmValue::Int(index as i64))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        "system_prompt".to_string(),
        state
            .system_prompt
            .as_ref()
            .map(|prompt| VmValue::String(Rc::from(prompt.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        "tool_format".to_string(),
        state
            .tool_format
            .as_ref()
            .map(|format| VmValue::String(Rc::from(format.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        "pinned_model".to_string(),
        state
            .pinned_model
            .as_ref()
            .map(|model| VmValue::String(Rc::from(model.clone())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(next))
}

fn update_lineage(
    map: &mut HashMap<String, SessionState>,
    parent_id: &str,
    child_id: &str,
    branched_at_event_index: Option<usize>,
) {
    let old_parent_id = map.get(child_id).and_then(|child| child.parent_id.clone());
    if let Some(old_parent_id) = old_parent_id.filter(|old_parent_id| old_parent_id != parent_id) {
        if let Some(old_parent) = map.get_mut(&old_parent_id) {
            old_parent.child_ids.retain(|id| id != child_id);
            old_parent.last_accessed = Instant::now();
        }
    }
    if let Some(parent) = map.get_mut(parent_id) {
        parent.last_accessed = Instant::now();
        if !parent.child_ids.iter().any(|id| id == child_id) {
            parent.child_ids.push(child_id.to_string());
        }
    }
    if let Some(child) = map.get_mut(child_id) {
        child.last_accessed = Instant::now();
        child.parent_id = Some(parent_id.to_string());
        child.branched_at_event_index = branched_at_event_index;
        child.transcript = clone_transcript_with_parent(&child.transcript, parent_id);
    }
}

fn branch_event_index(transcript: &VmValue, keep_first: usize) -> usize {
    if keep_first == 0 {
        return 0;
    }
    let Some(dict) = transcript.as_dict() else {
        return keep_first;
    };
    let Some(VmValue::List(events)) = dict.get("events") else {
        return keep_first;
    };
    event_prefix_len_for_messages(events, keep_first)
}

fn event_kind(event: &VmValue) -> Option<String> {
    event
        .as_dict()
        .and_then(|dict| dict.get("kind"))
        .map(VmValue::display)
}

fn event_id(event: &VmValue) -> Option<String> {
    event
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(VmValue::display)
}

fn is_turn_event(event: &VmValue) -> bool {
    matches!(
        event_kind(event).as_deref(),
        Some("message" | "tool_result")
    )
}

fn event_prefix_len_for_messages(events: &[VmValue], keep_first: usize) -> usize {
    if keep_first == 0 {
        return 0;
    }
    let mut retained_messages = 0usize;
    for (index, event) in events.iter().enumerate() {
        if is_turn_event(event) {
            retained_messages += 1;
            if retained_messages == keep_first {
                return index + 1;
            }
        }
    }
    events.len()
}

fn turn_event_id_for_count(events: &[VmValue], keep_first: usize) -> Option<String> {
    if keep_first == 0 {
        return None;
    }
    let mut retained_messages = 0usize;
    for event in events {
        if is_turn_event(event) {
            retained_messages += 1;
            if retained_messages == keep_first {
                return event_id(event);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "agent_sessions_tests.rs"]
mod tests;
