//! The transcript changes a recorded tool dispatch makes, and their replay.
//!
//! Every session transcript write ends in `SessionState::replace_transcript`,
//! so noting there tapes hooks, providers, host events, trims and compaction
//! alike. A served dispatch runs none of them; it replays the changes through
//! the same writer instead.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::testbench::tape::{FieldChange, TranscriptChange};
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
                .push(TranscriptChange {
                    session_id: session_id.to_string(),
                    fields,
                });
        }
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

/// Replay a served dispatch's recorded changes into the sessions that still
/// exist, and return them for the branch's own tape.
pub(super) fn replay(changes: Vec<TranscriptChange>) -> Result<Vec<TranscriptChange>, String> {
    for change in &changes {
        crate::agent_sessions::replay_transcript_change(&change.session_id, &change.fields)?;
    }
    Ok(changes)
}
