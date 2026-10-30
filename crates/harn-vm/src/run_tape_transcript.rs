//! The transcript changes a recorded tool dispatch makes, and their replay.
//!
//! Every session transcript write ends in `SessionState::replace_transcript`,
//! so noting there tapes hooks, providers, host events, trims and compaction
//! alike. A served dispatch runs none of them; it replays the changes through
//! the same writer instead.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::testbench::tape::{FieldChange, TranscriptChange, TranscriptLifecycle};
use crate::value::{DictMap, VmValue};

tokio::task_local! {
    /// The transcript changes made while the innermost recorded tool
    /// dispatch runs.
    static DISPATCH_CHANGES: Arc<Mutex<Vec<TranscriptChange>>>;
}

/// Run a live tool dispatch and return it with every transcript change it
/// made. An enclosing recorded dispatch keeps them too.
pub(super) async fn capture<F: std::future::Future>(live: F) -> (F::Output, Vec<TranscriptChange>) {
    let parent = DISPATCH_CHANGES.try_with(Arc::clone).ok();
    let frame = Arc::new(Mutex::new(Vec::new()));
    let output = DISPATCH_CHANGES.scope(frame.clone(), live).await;
    let captured = std::mem::take(&mut *frame.lock().expect("transcript frame poisoned"));
    if let Some(parent) = parent {
        parent
            .lock()
            .expect("transcript frame poisoned")
            .extend(captured.iter().cloned());
    }
    (output, captured)
}

/// Note that session `session_id`'s transcript went from `before` to `after`,
/// for the recorded dispatch running now, if any.
pub(crate) fn note_transcript_change(session_id: &str, before: &VmValue, after: &VmValue) {
    let _ = DISPATCH_CHANGES.try_with(|frame| {
        let fields = delta(before, after);
        if !fields.is_empty() {
            frame
                .lock()
                .expect("transcript frame poisoned")
                .push(TranscriptChange::Fields {
                    session_id: session_id.to_string(),
                    fields,
                });
        }
    });
}

pub(crate) fn note_session_opened(session_id: &str, transcript: &VmValue) {
    let _ = DISPATCH_CHANGES.try_with(|frame| {
        frame
            .lock()
            .expect("transcript frame poisoned")
            .push(TranscriptChange::Lifecycle {
                session_id: session_id.to_string(),
                lifecycle: TranscriptLifecycle::Opened {
                    transcript: crate::llm::helpers::vm_value_to_json(transcript),
                },
            });
    });
}

pub(crate) fn note_session_closed(session_id: &str) {
    let _ = DISPATCH_CHANGES.try_with(|frame| {
        frame
            .lock()
            .expect("transcript frame poisoned")
            .push(TranscriptChange::Lifecycle {
                session_id: session_id.to_string(),
                lifecycle: TranscriptLifecycle::Closed,
            });
    });
}

fn delta(before: &VmValue, after: &VmValue) -> BTreeMap<String, FieldChange> {
    let empty = DictMap::new();
    let before = before.as_dict().unwrap_or(&empty);
    let after = after.as_dict().unwrap_or(&empty);
    let json = crate::llm::helpers::vm_value_to_json;
    let mut fields = BTreeMap::new();
    for (key, value) in after.iter() {
        let change = match (before.get(key), value) {
            (Some(old), new) if crate::values_equal(old, new) => continue,
            (Some(VmValue::List(old)), VmValue::List(new))
                if new.len() > old.len()
                    && old
                        .iter()
                        .zip(new.iter())
                        .all(|(a, b)| crate::values_equal(a, b)) =>
            {
                FieldChange::Append(new[old.len()..].iter().map(json).collect())
            }
            _ => FieldChange::Set(json(value)),
        };
        fields.insert(key.to_string(), change);
    }
    for key in before.keys() {
        if !after.contains_key(key) {
            fields.insert(key.to_string(), FieldChange::Remove);
        }
    }
    fields
}

/// `transcript` with `fields` applied.
pub(crate) fn apply(
    transcript: &VmValue,
    fields: &BTreeMap<String, FieldChange>,
) -> Result<VmValue, String> {
    let mut next = transcript.as_dict().cloned().unwrap_or_default();
    let value = crate::schema::json_to_vm_value;
    for (key, change) in fields {
        let key_ref = crate::value::intern_key(key);
        match change {
            FieldChange::Set(json) => {
                next.insert(key_ref, value(json));
            }
            FieldChange::Append(items) => {
                let Some(VmValue::List(list)) = next.get(&key_ref) else {
                    return Err(format!("transcript field `{key}` is not a list to extend"));
                };
                let mut list = list.as_ref().clone();
                list.extend(items.iter().map(value));
                next.insert(key_ref, VmValue::list(list));
            }
            FieldChange::Remove => {
                next.remove(&key_ref);
            }
        }
    }
    Ok(VmValue::dict_map(next))
}

/// Closed ephemeral transcripts are reconstructed off-table. They never become
/// live sessions or run lifecycle hooks a second time. Surviving new sessions
/// need more than a transcript and therefore refuse instead of being fabricated.
pub(super) fn validate_lifetimes(changes: &[TranscriptChange]) -> Result<(), String> {
    let mut opened = BTreeSet::new();
    for change in changes {
        match change {
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Opened { transcript },
            } => {
                if !crate::llm::helpers::is_session_transcript_snapshot(transcript, session_id) {
                    return Err(format!("invalid initial transcript for recorded session `{session_id}`: expected a versioned transcript with matching identity and message, event and asset lists"));
                }
                if !opened.insert(session_id.as_str()) {
                    return Err(format!(
                        "recorded transcript `{session_id}` was opened twice without closing"
                    ));
                }
            }
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Closed,
            } => {
                if !opened.remove(session_id.as_str()) {
                    return Err(format!("recorded close of `{session_id}` has no dispatch-local opening; replay does not retire pre-existing sessions"));
                }
            }
            TranscriptChange::Fields { .. } => {}
        }
    }
    if !opened.is_empty() {
        return Err(format!(
            "{} recorded new session(s) remain open after the dispatch: {}",
            opened.len(),
            opened.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

/// Check all targets and deltas before any recorded file or transcript mutation.
/// Session state is thread-local and no await separates this from replay.
pub(super) fn preflight(changes: &[TranscriptChange]) -> Result<(), String> {
    validate_lifetimes(changes)?;
    let mut previews = BTreeMap::new();
    for change in changes {
        match change {
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Opened { transcript },
            } => {
                if crate::agent_sessions::exists(session_id) {
                    return Err(format!(
                        "recorded new transcript target `{session_id}` already exists"
                    ));
                }
                previews.insert(
                    session_id.clone(),
                    crate::schema::json_to_vm_value(transcript),
                );
            }
            TranscriptChange::Fields { session_id, fields } => {
                let transcript = match previews.entry(session_id.clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                        crate::agent_sessions::replay_transcript_snapshot(session_id)?,
                    ),
                };
                *transcript = apply(transcript, fields)?;
            }
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Closed,
            } => {
                previews.remove(session_id);
            }
        }
    }
    Ok(())
}

/// Apply changes validated against the live session table before file effects.
pub(super) fn replay(changes: Vec<TranscriptChange>) -> Result<Vec<TranscriptChange>, String> {
    let mut ephemeral = BTreeMap::new();
    for change in &changes {
        match change {
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Opened { transcript },
            } => {
                if crate::agent_sessions::exists(session_id) {
                    return Err(format!(
                        "recorded new transcript target `{session_id}` already exists"
                    ));
                }
                ephemeral.insert(
                    session_id.clone(),
                    crate::schema::json_to_vm_value(transcript),
                );
            }
            TranscriptChange::Fields { session_id, fields } => {
                if let Some(transcript) = ephemeral.get_mut(session_id) {
                    *transcript = apply(transcript, fields)?;
                } else {
                    crate::agent_sessions::replay_transcript_change(session_id, fields)?;
                }
            }
            TranscriptChange::Lifecycle {
                session_id,
                lifecycle: TranscriptLifecycle::Closed,
            } => {
                ephemeral
                    .remove(session_id)
                    .expect("validated local lifetime closes an opened transcript");
            }
        }
    }
    Ok(changes)
}
