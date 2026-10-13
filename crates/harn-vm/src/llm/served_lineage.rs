//! Prefix-chained lineage of the messages served to each provider request.
//!
//! An agent loop re-sends its whole conversation on every call, so a receipt
//! that lists every served message grows with the transcript and a run's
//! receipts grow quadratically. A `harn.llm.message_lineage.v2` receipt lists
//! only the messages appended since the previous retained receipt and names
//! that receipt by `lineage_id`. When the served prefix is not a continuation
//! (compaction, an edited or branched history, a changed projection) or no
//! predecessor is retained, the receipt carries the full list.
//!
//! This module owns both sides of the format: the writer that decides between
//! a continuation and a full list, and [`LineageIndex`], the one reader that
//! resolves a chain back to the full ordered lineage. Readers must go through
//! it rather than walking `base.previous_lineage_id` themselves.
//!
//! `lineage_id` is a hash chain over `(previous_lineage_id, appended entries)`,
//! so a reader detects a tampered or mismatched link without the message
//! bodies. The receipt's `messages_content_hash` still covers the whole served
//! array, so [`reconstruct_messages`] can verify the resolved message bodies.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use crate::llm::message_lineage::{MessageSemanticKind, ProjectionLineage};

/// Schema name of the chained lineage carried in `served_context.message_lineage`.
pub const MESSAGE_LINEAGE_SCHEMA: &str = "harn.llm.message_lineage.v2";

/// One served message, by position in the provider-visible array.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServedMessageLineage {
    pub position: usize,
    pub role: String,
    pub semantic_kind: MessageSemanticKind,
    pub message_content_hash: String,
    /// Content hash of the `served_message` event holding the message body.
    pub definition_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_receipt_ref: Option<String>,
}

/// Why a receipt carries its full message list instead of a continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FullLineageReason {
    /// No earlier receipt for this session is retained in the current sinks.
    NoPredecessor,
    /// The served messages do not extend the previous receipt's messages:
    /// compaction, an edited or branched history, or changed lineage metadata.
    PrefixDiverged,
}

/// What a receipt's `messages` are relative to.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineageBase {
    /// `messages` is the full served list.
    Full { reason: FullLineageReason },
    /// `messages` extends the named receipt's full list, whose length is
    /// `previous_message_count`.
    Continuation {
        previous_lineage_id: String,
        previous_message_count: usize,
    },
}

/// The `served_context.message_lineage` object of one provider request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageLineageReceipt {
    pub schema: String,
    pub lineage_id: String,
    pub call_id: String,
    pub iteration: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub projection: ProjectionLineage,
    /// Length of the full served list this receipt describes.
    pub message_count: usize,
    pub base: LineageBase,
    /// Entries at positions `base count .. message_count`.
    pub messages: Vec<ServedMessageLineage>,
}

impl MessageLineageReceipt {
    fn previous(&self) -> Option<(&str, usize)> {
        match &self.base {
            LineageBase::Full { .. } => None,
            LineageBase::Continuation {
                previous_lineage_id,
                previous_message_count,
            } => Some((previous_lineage_id.as_str(), *previous_message_count)),
        }
    }

    /// Check the receipt is internally consistent: its id re-derives from its
    /// link and entries, and its entries fill exactly the positions after its base.
    fn verify_link(&self) -> Result<(), LineageChainError> {
        if self.schema != MESSAGE_LINEAGE_SCHEMA {
            return Err(LineageChainError::UnsupportedSchema(self.schema.clone()));
        }
        let previous = self.previous();
        let computed = lineage_id(previous.map(|(id, _)| id), &self.messages);
        if computed != self.lineage_id {
            return Err(LineageChainError::IdMismatch {
                lineage_id: self.lineage_id.clone(),
                computed,
            });
        }
        let base_count = previous.map_or(0, |(_, count)| count);
        let positions_match = self
            .messages
            .iter()
            .enumerate()
            .all(|(offset, entry)| entry.position == base_count + offset);
        if !positions_match || base_count + self.messages.len() != self.message_count {
            return Err(LineageChainError::CountMismatch {
                lineage_id: self.lineage_id.clone(),
                base_count,
                appended: self.messages.len(),
                message_count: self.message_count,
            });
        }
        Ok(())
    }
}

/// A chain that cannot be resolved to a complete, verified lineage. Readers
/// get an error, never a silently partial list.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LineageChainError {
    #[error("unsupported message lineage schema `{0}`")]
    UnsupportedSchema(String),
    #[error("malformed message lineage receipt: {0}")]
    Malformed(String),
    #[error(
        "lineage {lineage_id} continues {previous_lineage_id}, which no retained receipt defines"
    )]
    MissingPredecessor {
        lineage_id: String,
        previous_lineage_id: String,
    },
    #[error("lineage {lineage_id} does not match its contents (computed {computed})")]
    IdMismatch {
        lineage_id: String,
        computed: String,
    },
    #[error(
        "lineage {lineage_id} appends {appended} entries to {base_count} but claims {message_count}"
    )]
    CountMismatch {
        lineage_id: String,
        base_count: usize,
        appended: usize,
        message_count: usize,
    },
    #[error("served message {definition_ref} at position {position} has no retained definition")]
    MissingDefinition {
        position: usize,
        definition_ref: String,
    },
    #[error("reconstructed messages hash to {computed}, but the receipt served {expected}")]
    SequenceHashMismatch { expected: String, computed: String },
}

/// Hash-chain id of a lineage receipt: blake3 over the JSON encoding of the
/// predecessor id and the appended entries, in [`ServedMessageLineage`] field
/// order. A reader re-derives it from the deserialized receipt.
fn lineage_id(previous: Option<&str>, appended: &[ServedMessageLineage]) -> String {
    #[derive(Serialize)]
    struct Link<'a> {
        previous: Option<&'a str>,
        messages: &'a [ServedMessageLineage],
    }
    let encoded = serde_json::to_vec(&Link {
        previous,
        messages: appended,
    })
    .expect("lineage link is JSON representable");
    format!("blake3:{}", blake3::hash(&encoded).to_hex())
}

/// The writer's memory of the last retained receipt for one session.
#[derive(Clone, Debug)]
pub(crate) struct LineageHead {
    lineage_id: String,
    entries: Arc<[ServedMessageLineage]>,
}

/// Encode `entries` against the session's last retained receipt.
///
/// Returns the receipt and the head to record once the receipt is persisted.
pub(crate) fn encode_receipt(
    entries: Vec<ServedMessageLineage>,
    head: Option<&LineageHead>,
    projection: ProjectionLineage,
    iteration: usize,
    call_id: &str,
    session_id: Option<&str>,
) -> (MessageLineageReceipt, LineageHead) {
    let message_count = entries.len();
    let continued = head.filter(|head| {
        head.entries.len() <= entries.len() && entries[..head.entries.len()] == *head.entries
    });
    let (base, previous, appended) = match (continued, head) {
        (Some(head), _) => (
            LineageBase::Continuation {
                previous_lineage_id: head.lineage_id.clone(),
                previous_message_count: head.entries.len(),
            },
            Some(head.lineage_id.as_str()),
            entries[head.entries.len()..].to_vec(),
        ),
        (None, Some(_)) => (
            LineageBase::Full {
                reason: FullLineageReason::PrefixDiverged,
            },
            None,
            entries.clone(),
        ),
        (None, None) => (
            LineageBase::Full {
                reason: FullLineageReason::NoPredecessor,
            },
            None,
            entries.clone(),
        ),
    };
    let id = lineage_id(previous, &appended);
    let receipt = MessageLineageReceipt {
        schema: MESSAGE_LINEAGE_SCHEMA.to_string(),
        lineage_id: id.clone(),
        call_id: call_id.to_string(),
        iteration,
        session_id: session_id.map(str::to_string),
        projection,
        message_count,
        base,
        messages: appended,
    };
    let head = LineageHead {
        lineage_id: id,
        entries: entries.into(),
    };
    (receipt, head)
}

/// Every lineage receipt retained in one transcript, by `lineage_id`.
#[derive(Clone, Debug, Default)]
pub struct LineageIndex {
    receipts: HashMap<String, MessageLineageReceipt>,
}

impl LineageIndex {
    /// Index the lineage of every `provider_call_request` among `events`, the
    /// payloads of one transcript file or event-log topic.
    pub fn from_transcript_events<'a>(
        events: impl IntoIterator<Item = &'a serde_json::Value>,
    ) -> Result<Self, LineageChainError> {
        let mut index = Self::default();
        for event in events {
            if event.get("type").and_then(serde_json::Value::as_str)
                != Some("provider_call_request")
            {
                continue;
            }
            if let Some(receipt) = request_lineage(event)? {
                index.insert(receipt);
            }
        }
        Ok(index)
    }

    /// Add one receipt. Equal ids name equal chains, so the first is kept.
    pub fn insert(&mut self, receipt: MessageLineageReceipt) {
        self.receipts
            .entry(receipt.lineage_id.clone())
            .or_insert(receipt);
    }

    /// The full ordered lineage `receipt` describes, verifying every link.
    pub fn reconstruct(
        &self,
        receipt: &MessageLineageReceipt,
    ) -> Result<Vec<ServedMessageLineage>, LineageChainError> {
        let mut chain = vec![receipt];
        let mut current = receipt;
        while let Some((previous_id, _)) = current.previous() {
            let previous = self.receipts.get(previous_id).ok_or_else(|| {
                LineageChainError::MissingPredecessor {
                    lineage_id: current.lineage_id.clone(),
                    previous_lineage_id: previous_id.to_string(),
                }
            })?;
            // Ids are hashes over their predecessor, so a chain longer than the
            // index can only come from a forged id; refuse rather than loop.
            if chain.len() > self.receipts.len() {
                return Err(LineageChainError::Malformed(format!(
                    "lineage {} does not terminate in a full receipt",
                    receipt.lineage_id
                )));
            }
            chain.push(previous);
            current = previous;
        }
        let mut entries = Vec::with_capacity(receipt.message_count);
        for link in chain.into_iter().rev() {
            link.verify_link()?;
            let base_count = link.previous().map_or(0, |(_, count)| count);
            if base_count != entries.len() {
                return Err(LineageChainError::CountMismatch {
                    lineage_id: link.lineage_id.clone(),
                    base_count,
                    appended: link.messages.len(),
                    message_count: link.message_count,
                });
            }
            entries.extend(link.messages.iter().cloned());
        }
        Ok(entries)
    }

    /// The full ordered lineage of one `provider_call_request` event, or
    /// `None` when the request carries no lineage.
    pub fn reconstruct_request(
        &self,
        request_event: &serde_json::Value,
    ) -> Result<Option<Vec<ServedMessageLineage>>, LineageChainError> {
        request_lineage(request_event)?
            .map(|receipt| self.reconstruct(&receipt))
            .transpose()
    }
}

/// The lineage receipt of one `provider_call_request` event.
pub fn request_lineage(
    request_event: &serde_json::Value,
) -> Result<Option<MessageLineageReceipt>, LineageChainError> {
    let Some(value) = request_event.pointer("/served_context/message_lineage") else {
        return Ok(None);
    };
    if let Some(schema) = value.get("schema").and_then(serde_json::Value::as_str) {
        if schema != MESSAGE_LINEAGE_SCHEMA {
            return Err(LineageChainError::UnsupportedSchema(schema.to_string()));
        }
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| LineageChainError::Malformed(error.to_string()))
}

/// Resolve reconstructed entries to the retained `served_message` bodies and
/// verify them against the receipt's whole-array `messages_content_hash`.
pub fn reconstruct_messages(
    entries: &[ServedMessageLineage],
    definitions: &HashMap<String, serde_json::Value>,
    messages_content_hash: &str,
) -> Result<Vec<serde_json::Value>, LineageChainError> {
    let messages = entries
        .iter()
        .map(|entry| {
            definitions
                .get(&entry.definition_ref)
                .cloned()
                .ok_or_else(|| LineageChainError::MissingDefinition {
                    position: entry.position,
                    definition_ref: entry.definition_ref.clone(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let computed = crate::llm::content_hash::stable_redacted_json_hash(&serde_json::Value::Array(
        messages.clone(),
    ));
    if computed != messages_content_hash {
        return Err(LineageChainError::SequenceHashMismatch {
            expected: messages_content_hash.to_string(),
            computed,
        });
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(position: usize, hash: &str) -> ServedMessageLineage {
        ServedMessageLineage {
            position,
            role: "user".to_string(),
            semantic_kind: MessageSemanticKind::User,
            message_content_hash: hash.to_string(),
            definition_ref: hash.to_string(),
            source_message_index: Some(position),
            compaction_receipt_ref: None,
        }
    }

    fn encode(
        entries: &[ServedMessageLineage],
        head: Option<&LineageHead>,
        call: usize,
    ) -> (MessageLineageReceipt, LineageHead) {
        encode_receipt(
            entries.to_vec(),
            head,
            crate::llm::message_lineage::raw_projection(),
            call,
            &format!("call-{call}"),
            Some("session"),
        )
    }

    #[test]
    fn continuation_lists_only_appended_entries_and_reconstructs() {
        let first = vec![entry(0, "a")];
        let (r0, head) = encode(&first, None, 0);
        assert_eq!(
            r0.base,
            LineageBase::Full {
                reason: FullLineageReason::NoPredecessor
            }
        );
        let second = vec![entry(0, "a"), entry(1, "b"), entry(2, "c")];
        let (r1, _) = encode(&second, Some(&head), 1);
        assert_eq!(r1.messages, second[1..]);
        assert_eq!(r1.message_count, 3);

        let mut index = LineageIndex::default();
        index.insert(r0);
        index.insert(r1.clone());
        assert_eq!(index.reconstruct(&r1).expect("chain"), second);
    }

    #[test]
    fn diverged_prefix_carries_the_full_list() {
        let (_, head) = encode(&[entry(0, "a"), entry(1, "b")], None, 0);
        let rewritten = vec![entry(0, "summary"), entry(1, "b")];
        let (receipt, _) = encode(&rewritten, Some(&head), 1);
        assert_eq!(
            receipt.base,
            LineageBase::Full {
                reason: FullLineageReason::PrefixDiverged
            }
        );
        assert_eq!(receipt.messages, rewritten);
        // A shorter history is never a continuation either.
        let (shorter, _) = encode(&[entry(0, "a")], Some(&head), 2);
        assert!(matches!(shorter.base, LineageBase::Full { .. }));
    }

    #[test]
    fn tampered_or_unlinked_chains_fail_closed() {
        let (r0, head) = encode(&[entry(0, "a")], None, 0);
        let (r1, _) = encode(&[entry(0, "a"), entry(1, "b")], Some(&head), 1);

        let missing = LineageIndex::default();
        assert!(matches!(
            missing.reconstruct(&r1),
            Err(LineageChainError::MissingPredecessor { .. })
        ));

        let mut tampered = r1.clone();
        tampered.messages[0].message_content_hash = "forged".to_string();
        let mut index = LineageIndex::default();
        index.insert(r0);
        assert!(matches!(
            index.reconstruct(&tampered),
            Err(LineageChainError::IdMismatch { .. })
        ));

        let mut miscounted = r1;
        miscounted.message_count = 5;
        assert!(matches!(
            index.reconstruct(&miscounted),
            Err(LineageChainError::CountMismatch { .. })
        ));
    }

    #[test]
    fn v1_receipts_are_rejected_rather_than_misread() {
        let request = serde_json::json!({
            "type": "provider_call_request",
            "served_context": {"message_lineage": {"schema": "harn.llm.message_lineage.v1"}},
        });
        assert_eq!(
            LineageIndex::from_transcript_events([&request]).unwrap_err(),
            LineageChainError::UnsupportedSchema("harn.llm.message_lineage.v1".to_string())
        );
    }
}
