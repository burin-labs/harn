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
//! `_reset`, `_fork`, `_close`, `_trim`, `_compact`, `_inject`,
//! `_exists`, `_length`, `_snapshot`) drive the store directly — there
//! is no "policy" config dict that performs lifecycle as a side effect.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
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
            active_skills: Vec::new(),
        }
    }
}

thread_local! {
    static SESSIONS: RefCell<HashMap<String, SessionState>> = RefCell::new(HashMap::new());
    static SESSION_CAP: Cell<usize> = const { Cell::new(DEFAULT_SESSION_CAP) };
    static CURRENT_SESSION_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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

pub(crate) fn current_session_id() -> Option<String> {
    CURRENT_SESSION_STACK.with(|stack| stack.borrow().last().cloned())
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
    SESSIONS.with(|s| s.borrow().get(id).map(|state| state.transcript.clone()))
}

/// Open a session, or create it if missing. Returns the resolved id.
///
/// When the `HARN_EVENT_LOG_DIR` environment variable points at an
/// existing (or creatable) directory, a [`JsonlEventSink`] is
/// auto-registered against the newly-created session so the agent
/// loop's AgentEvent stream persists to `event_log-<session_id>.jsonl`.
/// Re-opening an existing session does not re-register — sinks are
/// per-session, owned by the first opener.
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
        try_register_jsonl_event_log(&resolved);
    }
    resolved
}

/// Auto-register a [`JsonlEventSink`] for a newly-created session
/// when `HARN_EVENT_LOG_DIR` is set. Silent no-op when the env var
/// is missing or the file can't be opened — a broken log sink must
/// never prevent a session from starting.
fn try_register_jsonl_event_log(session_id: &str) {
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
        if let Some(state) = s.borrow_mut().get_mut(&dst) {
            state.transcript = src_transcript;
            state.last_accessed = Instant::now();
        }
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
/// scrubber: the IDE picks an event index, rebuilds a message count,
/// and calls this to spawn a live sibling session that resumes from
/// the rebuilt state. Subscribers are not carried over (same as
/// `fork`), so sibling events don't double-fan into the parent's
/// consumers.
///
/// Returns the new session id on success, `None` if `src_id` doesn't
/// exist.
pub fn fork_at(src_id: &str, keep_first: usize, dst_id: Option<String>) -> Option<String> {
    let new_id = fork(src_id, dst_id)?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
