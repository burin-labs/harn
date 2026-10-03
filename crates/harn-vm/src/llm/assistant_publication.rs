//! Admission of actor prose. Provider history remains canonical and private;
//! the existing terminal turn phase owns the accepted user-facing reply.

use super::agent_session_host::{dict_get, list_items};
use crate::stdlib::json_to_vm_value as json_to_vm;
use crate::value::{VmError, VmValue};

const KEY: &str = "harn_assistant_publication";
const SCHEMA: &str = "harn.assistant_publication.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Publication {
    Pending,
    Withheld,
    Published,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Change {
    message_index: usize,
    source_hash: String,
    disposition: Publication,
}

fn source_hash(message: &serde_json::Value) -> String {
    blake3::hash(&crate::canonical_json::to_vec(message))
        .to_hex()
        .to_string()
}

/// Apply the same source-bound receipt during live admission and hydration.
/// A corrupt or stale receipt leaves the original draft unpublished.
pub(crate) fn replay(
    messages: &mut [(Option<String>, serde_json::Value)],
    metadata: &serde_json::Value,
) {
    if metadata.get("schema").and_then(serde_json::Value::as_str) != Some(SCHEMA) {
        return;
    }
    let Ok(changes) = serde_json::from_value::<Vec<Change>>(metadata["changes"].clone()) else {
        return;
    };
    for change in changes {
        if let Some((_, message)) = messages.get_mut(change.message_index) {
            if message.get(KEY).and_then(serde_json::Value::as_str) == Some("pending")
                && source_hash(message) == change.source_hash
            {
                message[KEY] =
                    serde_json::to_value(change.disposition).expect("publication is serializable");
            }
        }
    }
}

/// The loop requests deferred visibility after parsing, rather than a provider
/// inventing a publication decision in its response text.
pub(super) fn defer(message: &mut VmValue, result: &VmValue) {
    if !matches!(
        dict_get(result, "_defer_visible"),
        Some(VmValue::Bool(true))
    ) {
        return;
    }
    let has_calls = dict_get(result, "tool_calls")
        .is_some_and(|calls| !list_items(calls).is_empty())
        || matches!(
            dict_get(result, "_publication_eligible"),
            Some(VmValue::Bool(false))
        );
    let disposition = if has_calls {
        Publication::Withheld
    } else {
        Publication::Pending
    };
    let mut value = super::agent_session_host::vm_to_json(message);
    value[KEY] = serde_json::to_value(disposition).expect("publication is serializable");
    *message = json_to_vm(&value);
}

/// Missing metadata denotes an ordinary, already-visible non-actor message.
/// Unknown metadata fails closed instead of making a damaged draft visible.
pub(crate) fn is_visible(message: &VmValue) -> bool {
    match dict_get(message, KEY) {
        None => true,
        Some(value) => value.display() == "published",
    }
}

/// Settle drafts in place without another assistant turn or another usage
/// record. Only the trailing assistant answer can be accepted; tool-batch
/// prose never becomes a completion report after its effects execute.
pub(super) fn settle(session_id: &str, admitted: bool) -> Result<Option<String>, VmError> {
    let Some(snapshot) = crate::agent_sessions::transcript(session_id) else {
        return Ok(None);
    };
    let mut messages: Vec<serde_json::Value> = dict_get(&snapshot, "messages")
        .map(list_items)
        .unwrap_or_default()
        .iter()
        .map(super::agent_session_host::vm_to_json)
        .collect();
    let mut candidate = None;
    for (index, message) in messages.iter().enumerate().rev() {
        if message["role"] != "assistant" {
            continue;
        }
        if message.get(KEY).and_then(serde_json::Value::as_str) != Some("pending") {
            break;
        }
        let mut projected = message.clone();
        projected[KEY] = serde_json::json!("published");
        if super::agent_result_projection::visible_assistant_text(&json_to_vm(&projected)).is_some()
        {
            candidate = Some(index);
            break;
        }
    }
    let mut changes = Vec::new();
    let mut accepted = None;
    for (index, message) in messages.iter_mut().enumerate() {
        if message.get(KEY).and_then(serde_json::Value::as_str) != Some("pending") {
            continue;
        }
        let disposition = if admitted && Some(index) == candidate {
            Publication::Published
        } else {
            Publication::Withheld
        };
        changes.push(Change {
            message_index: index,
            source_hash: source_hash(message),
            disposition,
        });
        message[KEY] = serde_json::to_value(disposition).expect("publication is serializable");
        if disposition == Publication::Published {
            accepted = super::agent_result_projection::visible_assistant_text(&json_to_vm(message));
        }
    }
    if !changes.is_empty() {
        let event = super::helpers::transcript_event(
            "assistant_publication",
            "assistant",
            if accepted.is_some() {
                "public"
            } else {
                "internal"
            },
            accepted.as_deref().unwrap_or("Actor drafts settled"),
            Some(serde_json::json!({"schema": SCHEMA, "changes": changes})),
        );
        let mut next = super::agent_session_host::vm_to_json(&snapshot);
        next["messages"] = serde_json::Value::Array(messages);
        crate::agent_sessions::store_transcript_with_audit(session_id, json_to_vm(&next), event)
            .map_err(VmError::Runtime)?;
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmDictExt;

    #[tokio::test]
    async fn admitted_publication_survives_canonical_journal_hydration() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("journal root");
        let session_id = "publication-journal-round-trip";
        let mut options = crate::value::DictMap::new();
        options.put_str("root", root.path().to_string_lossy().as_ref());
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-first".into(),
            "turn-first".into(),
        )
        .await
        .expect("prepare canonical journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.into()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        let mut message =
            json_to_vm(&serde_json::json!({"role":"assistant", "content":"Accepted answer"}));
        defer(
            &mut message,
            &json_to_vm(&serde_json::json!({"_defer_visible":true})),
        );
        crate::agent_sessions::inject_message(session_id, message).expect("record actor draft");
        assert_eq!(
            super::super::agent_result_projection::last_assistant_text(
                &crate::agent_sessions::transcript(session_id).expect("session")
            ),
            None
        );
        assert_eq!(
            settle(session_id, true).expect("admit"),
            Some("Accepted answer".into())
        );
        assert_eq!(settle(session_id, true).expect("idempotent settle"), None);
        crate::agent_session_journal::flush(session_id)
            .await
            .expect("persist admission");
        crate::agent_sessions::clear_journal(session_id);
        let hydrated = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-second".into(),
            "turn-second".into(),
        )
        .await
        .expect("hydrate canonical journal");
        assert_eq!(hydrated.transcript.messages.len(), 1);
        let message = json_to_vm(&hydrated.transcript.messages[0]);
        assert_eq!(
            super::super::agent_result_projection::visible_assistant_text(&message),
            Some("Accepted answer".into())
        );
        assert_eq!(
            dict_get(&message, "content").map(VmValue::display),
            Some("Accepted answer".into())
        );
        drop(hydrated);
        crate::agent_sessions::reset_session_store();
    }

    #[test]
    fn publication_receipts_are_source_bound_and_idempotent() {
        let draft =
            serde_json::json!({"role":"assistant", "content":"Accepted answer", KEY:"pending"});
        let receipt = serde_json::json!({"schema":SCHEMA,"changes":[Change {
            message_index:0, source_hash:source_hash(&draft), disposition:Publication::Published,
        }]});
        let mut messages = vec![(None, draft.clone())];
        replay(&mut messages, &receipt);
        assert_eq!(messages[0].1[KEY], "published");
        let once = messages.clone();
        replay(&mut messages, &receipt);
        assert_eq!(messages, once);

        let mut wrong_source = vec![(
            None,
            serde_json::json!({"role":"assistant","content":"Different draft",KEY:"pending"}),
        )];
        replay(&mut wrong_source, &receipt);
        assert_eq!(wrong_source[0].1[KEY], "pending");
        assert!(!is_visible(&json_to_vm(&wrong_source[0].1)));
        let mut malformed = vec![(None, draft)];
        replay(
            &mut malformed,
            &serde_json::json!({"schema":SCHEMA,"changes":"broken"}),
        );
        assert_eq!(malformed[0].1[KEY], "pending");
    }
}
