//! Admission of actor prose. Provider history remains canonical and private;
//! the existing terminal turn phase owns the accepted user-facing reply.

use super::agent_session_host::{dict_get, list_items};
use crate::schema::json_to_vm_value as json_to_vm;
use crate::value::VmValue;

const KEY: &str = "harn_assistant_publication";
const SCHEMA: &str = "harn.assistant_publication.v1";

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    Pending,
    Withheld,
    Published,
}

pub(crate) fn diagnostic_disposition(message: &serde_json::Value) -> (bool, Option<Publication>) {
    match message.get(KEY) {
        Some(value) => (true, serde_json::from_value(value.clone()).ok()),
        None => (false, None),
    }
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
pub(crate) fn defer(message: &mut VmValue, result: &VmValue) {
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

pub(crate) struct PublicationSettlement {
    pub reply: Option<String>,
    pub mutation: Option<(VmValue, VmValue)>,
}

/// Compute the source-bound patch without reaching into live session state.
/// Only the trailing assistant answer can be accepted; tool-batch prose never
/// becomes a completion report after its effects execute.
pub(crate) fn settle(snapshot: &VmValue, admitted: bool) -> PublicationSettlement {
    let mut messages: Vec<serde_json::Value> = dict_get(snapshot, "messages")
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
    let mutation = if !changes.is_empty() {
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
        let mut next = super::agent_session_host::vm_to_json(snapshot);
        next["messages"] = serde_json::Value::Array(messages);
        Some((json_to_vm(&next), event))
    } else {
        None
    };
    PublicationSettlement {
        reply: accepted,
        mutation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
