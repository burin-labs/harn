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
    pub created_at: Instant,
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
}

impl SessionState {
    fn new(id: String) -> Self {
        let now = Instant::now();
        let transcript = empty_transcript(&id);
        Self {
            id,
            transcript,
            subscribers: Vec::new(),
            created_at: now,
            last_accessed: now,
            parent_id: None,
            child_ids: Vec::new(),
            branched_at_event_index: None,
            active_skills: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAncestry {
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub root_id: String,
}

thread_local! {
    static SESSIONS: RefCell<HashMap<String, SessionState>> = RefCell::new(HashMap::new());
    static SESSION_CAP: Cell<usize> = const { Cell::new(DEFAULT_SESSION_CAP) };
    static CURRENT_SESSION_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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
        try_register_event_log(&resolved);
    }
    resolved
}

pub fn open_child_session(parent_id: &str, id: Option<String>) -> String {
    let resolved = fork(parent_id, id.clone()).unwrap_or_else(|| open_or_create(id));
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

pub fn close(id: &str) {
    SESSIONS.with(|s| {
        s.borrow_mut().remove(id);
    });
}

pub fn reset_transcript(id: &str) -> bool {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return false;
        };
        state.transcript = empty_transcript(id);
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
    let (src_transcript, dst) = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let src = map.get_mut(src_id)?;
        src.last_accessed = Instant::now();
        let dst = dst_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let forked_transcript = clone_transcript_with_id(&src.transcript, &dst);
        Some((forked_transcript, dst))
    })?;
    // Ensure cap is respected when inserting the fork.
    open_or_create(Some(dst.clone()));
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        if let Some(state) = map.get_mut(&dst) {
            state.transcript = src_transcript;
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
    retain_first(&new_id, keep_first);
    Some(new_id)
}

/// Truncate the session transcript to the first `keep_first`
/// messages (opposite of `trim`, which keeps the last N). Used by
/// `fork_at` to cut a branch at a scrubber position.
fn retain_first(id: &str, keep_first: usize) {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return;
        };
        let Some(dict) = state.transcript.as_dict() else {
            return;
        };
        let dict = dict.clone();
        let messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let retained: Vec<VmValue> = messages.into_iter().take(keep_first).collect();
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
    });
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
        messages.push(VmValue::Dict(Rc::new(msg_dict)));
        let mut next = dict;
        next.insert(
            "events".to_string(),
            VmValue::List(Rc::new(
                crate::llm::helpers::transcript_events_from_messages(&messages),
            )),
        );
        next.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
        state.transcript = VmValue::Dict(Rc::new(next));
        state.last_accessed = Instant::now();
        Ok(())
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
            state.transcript = transcript;
            state.last_accessed = Instant::now();
        }
    });
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

fn session_snapshot(state: &SessionState) -> VmValue {
    let Some(dict) = state.transcript.as_dict() else {
        return state.transcript.clone();
    };
    let mut next = dict.clone();
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
    let mut retained_messages = 0usize;
    for (index, event) in events.iter().enumerate() {
        let kind = event
            .as_dict()
            .and_then(|dict| dict.get("kind"))
            .map(VmValue::display);
        if matches!(kind.as_deref(), Some("message" | "tool_result")) {
            retained_messages += 1;
            if retained_messages == keep_first {
                return index + 1;
            }
        }
    }
    events.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{
        emit_event, reset_all_sinks, session_external_sink_count, AgentEvent,
    };
    use crate::event_log::{active_event_log, EventLog, Topic};
    use std::collections::BTreeMap;

    fn make_msg(role: &str, content: &str) -> VmValue {
        let mut m: BTreeMap<String, VmValue> = BTreeMap::new();
        m.insert("role".to_string(), VmValue::String(Rc::from(role)));
        m.insert("content".to_string(), VmValue::String(Rc::from(content)));
        VmValue::Dict(Rc::new(m))
    }

    fn message_count(id: &str) -> usize {
        SESSIONS.with(|s| {
            let map = s.borrow();
            let Some(state) = map.get(id) else { return 0 };
            let Some(dict) = state.transcript.as_dict() else {
                return 0;
            };
            match dict.get("messages") {
                Some(VmValue::List(list)) => list.len(),
                _ => 0,
            }
        })
    }

    #[test]
    fn fork_at_truncates_destination_to_keep_first() {
        reset_session_store();
        let src = open_or_create(Some("src-fork-at".into()));
        inject_message(&src, make_msg("user", "a")).unwrap();
        inject_message(&src, make_msg("assistant", "b")).unwrap();
        inject_message(&src, make_msg("user", "c")).unwrap();
        inject_message(&src, make_msg("assistant", "d")).unwrap();
        assert_eq!(message_count(&src), 4);

        let dst = fork_at(&src, 2, Some("dst-fork-at".into())).expect("fork_at");
        assert_ne!(dst, src);
        assert_eq!(message_count(&dst), 2, "branched at message index 2");
        assert_eq!(
            snapshot(&dst)
                .and_then(|value| value.as_dict().cloned())
                .and_then(|dict| dict
                    .get("branched_at_event_index")
                    .and_then(VmValue::as_int)),
            Some(2)
        );
        // Source untouched.
        assert_eq!(message_count(&src), 4);
        // Subscribers not carried — forks start with a clean fanout list.
        assert_eq!(subscriber_count(&dst), 0);
        reset_session_store();
    }

    #[test]
    fn fork_at_on_unknown_source_returns_none() {
        reset_session_store();
        assert!(fork_at("does-not-exist", 3, None).is_none());
    }

    #[test]
    fn child_sessions_record_parent_lineage() {
        reset_session_store();
        let parent = open_or_create(Some("parent-session".into()));
        let child = open_child_session(&parent, Some("child-session".into()));
        assert_eq!(parent_id(&child).as_deref(), Some("parent-session"));
        assert_eq!(child_ids(&parent), vec!["child-session".to_string()]);
        assert_eq!(
            ancestry(&child),
            Some(SessionAncestry {
                parent_id: Some("parent-session".to_string()),
                child_ids: Vec::new(),
                root_id: "parent-session".to_string(),
            })
        );

        let transcript = snapshot(&child).expect("child transcript");
        let transcript = transcript.as_dict().expect("child snapshot");
        let metadata = transcript
            .get("metadata")
            .and_then(|value| value.as_dict())
            .expect("child metadata");
        assert!(
            matches!(transcript.get("parent_id"), Some(VmValue::String(value)) if value.as_ref() == "parent-session")
        );
        assert!(
            matches!(transcript.get("child_ids"), Some(VmValue::List(children)) if children.is_empty())
        );
        assert!(matches!(
            transcript.get("branched_at_event_index"),
            Some(VmValue::Nil)
        ));
        assert!(matches!(
            metadata.get("parent_session_id"),
            Some(VmValue::String(value)) if value.as_ref() == "parent-session"
        ));
    }

    #[test]
    fn branch_event_index_counts_non_message_events() {
        reset_session_store();
        let src = open_or_create(Some("branch-event-index".into()));
        let transcript = VmValue::Dict(Rc::new(BTreeMap::from([
            ("id".to_string(), VmValue::String(Rc::from(src.clone()))),
            (
                "messages".to_string(),
                VmValue::List(Rc::new(vec![
                    make_msg("user", "a"),
                    make_msg("assistant", "b"),
                ])),
            ),
            (
                "events".to_string(),
                VmValue::List(Rc::new(vec![
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "kind".to_string(),
                        VmValue::String(Rc::from("message")),
                    )]))),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "kind".to_string(),
                        VmValue::String(Rc::from("sub_agent_start")),
                    )]))),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "kind".to_string(),
                        VmValue::String(Rc::from("message")),
                    )]))),
                ])),
            ),
        ])));
        store_transcript(&src, transcript);

        let dst = fork_at(&src, 2, Some("branch-event-index-child".into())).expect("fork_at");
        assert_eq!(
            snapshot(&dst)
                .and_then(|value| value.as_dict().cloned())
                .and_then(|dict| dict
                    .get("branched_at_event_index")
                    .and_then(VmValue::as_int)),
            Some(3)
        );
    }

    #[test]
    fn child_session_forks_parent_transcript() {
        reset_session_store();
        let parent = open_or_create(Some("parent-fork-parent".into()));
        inject_message(&parent, make_msg("user", "parent context")).unwrap();

        let child = open_child_session(&parent, Some("parent-fork-child".into()));
        assert_eq!(message_count(&parent), 1);
        assert_eq!(message_count(&child), 1);

        let child_messages = messages_json(&child);
        assert_eq!(
            child_messages[0]["content"].as_str(),
            Some("parent context"),
        );
    }

    #[test]
    fn prompt_state_prepends_summary_message_when_missing_from_messages() {
        reset_session_store();
        let session = open_or_create(Some("prompt-state-summary".into()));
        let transcript = crate::llm::helpers::new_transcript_with_events(
            Some(session.clone()),
            vec![make_msg("assistant", "latest answer")],
            Some("[auto-compacted 2 older messages]\nsummary".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Some("active"),
        );
        store_transcript(&session, transcript);

        let prompt = prompt_state_json(&session);
        assert_eq!(
            prompt.summary.as_deref(),
            Some("[auto-compacted 2 older messages]\nsummary")
        );
        assert_eq!(prompt.messages.len(), 2);
        assert_eq!(prompt.messages[0]["role"].as_str(), Some("user"));
        assert_eq!(
            prompt.messages[0]["content"].as_str(),
            Some("[auto-compacted 2 older messages]\nsummary"),
        );
        assert_eq!(prompt.messages[1]["role"].as_str(), Some("assistant"));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn open_or_create_registers_event_log_sink_when_active_log_is_installed() {
        reset_all_sinks();
        crate::event_log::reset_active_event_log();
        let dir = tempfile::tempdir().expect("tempdir");
        crate::event_log::install_default_for_base_dir(dir.path()).expect("install event log");

        let session = open_or_create(Some("event-log-session".into()));
        assert_eq!(session_external_sink_count(&session), 1);

        emit_event(&AgentEvent::TurnStart {
            session_id: session.clone(),
            iteration: 0,
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;

        let topic = Topic::new("observability.agent_events.event-log-session").unwrap();
        let log = active_event_log().expect("active event log");
        let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.kind, "turn_start");

        crate::event_log::reset_active_event_log();
        reset_all_sinks();
    }
}
