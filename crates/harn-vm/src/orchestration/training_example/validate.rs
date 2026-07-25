//! The `harn.agent_training_example.v1` call/result pairing invariant.
//!
//! One owner, two callers: the projector checks its own output before handing
//! it out, and `harn models lora export` checks every projected example it
//! consumes. Keeping the rule here means a trainer-facing consumer can never
//! drift into a laxer reading of it than the producer used.
//!
//! The invariant: every tool call an assistant turn makes is answered by
//! exactly one `role: "tool"` message carrying that call's id, the answers
//! arrive in request order, and they all arrive before the next non-tool turn.

use std::collections::BTreeSet;

use super::TrainingMessage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingPairingError {
    pub kind: String,
    pub message: String,
    /// Index into the message list where the violation was detected.
    pub message_index: usize,
}

impl TrainingPairingError {
    fn new(kind: &str, message_index: usize, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            message_index,
        }
    }
}

impl std::fmt::Display for TrainingPairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (message {}): {}",
            self.kind, self.message_index, self.message
        )
    }
}

impl std::error::Error for TrainingPairingError {}

pub fn validate_training_example_pairing(
    messages: &[TrainingMessage],
) -> Result<(), TrainingPairingError> {
    let mut expected: Vec<(String, String)> = Vec::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    let mut opened_by: usize = 0;

    for (index, message) in messages.iter().enumerate() {
        match message.role.as_str() {
            "tool" => {
                let Some(call_id) = message.tool_call_id.as_deref().filter(|id| !id.is_empty())
                else {
                    return Err(TrainingPairingError::new(
                        "orphaned_tool_result",
                        index,
                        "tool message carries no tool_call_id, so it names no call",
                    ));
                };
                if expected.is_empty() {
                    return Err(TrainingPairingError::new(
                        "orphaned_tool_result",
                        index,
                        format!("tool message answers {call_id} with no call outstanding"),
                    ));
                }
                let (head_id, head_name) = &expected[0];
                if head_id != call_id {
                    let kind = if expected.iter().any(|(id, _)| id == call_id) {
                        "out_of_order_tool_result"
                    } else if answered.contains(call_id) {
                        "duplicate_tool_result"
                    } else {
                        "orphaned_tool_result"
                    };
                    return Err(TrainingPairingError::new(
                        kind,
                        index,
                        format!("expected the result for {head_id} ({head_name}), got {call_id}"),
                    ));
                }
                answered.insert(call_id.to_string());
                expected.remove(0);
            }
            "assistant" => {
                if let Some((id, name)) = expected.first() {
                    return Err(TrainingPairingError::new(
                        "unpaired_tool_call",
                        index,
                        format!(
                            "call {id} ({name}) from message {opened_by} is unanswered at the \
                             next assistant turn"
                        ),
                    ));
                }
                let mut seen = BTreeSet::new();
                for call in &message.tool_calls {
                    if call.id.is_empty() {
                        return Err(TrainingPairingError::new(
                            "malformed_tool_call",
                            index,
                            format!("call to `{}` has no id", call.function.name),
                        ));
                    }
                    if !seen.insert(call.id.clone()) {
                        return Err(TrainingPairingError::new(
                            "duplicate_tool_call_id",
                            index,
                            format!("assistant turn reuses call id {}", call.id),
                        ));
                    }
                    if answered.contains(&call.id) {
                        return Err(TrainingPairingError::new(
                            "duplicate_tool_call_id",
                            index,
                            format!(
                                "call id {} was already used earlier in this example",
                                call.id
                            ),
                        ));
                    }
                    expected.push((call.id.clone(), call.function.name.clone()));
                }
                opened_by = index;
            }
            _ => {
                if let Some((id, name)) = expected.first() {
                    return Err(TrainingPairingError::new(
                        "unpaired_tool_call",
                        index,
                        format!(
                            "call {id} ({name}) from message {opened_by} is unanswered at the \
                             next `{}` turn; a generic placeholder turn is not a tool result",
                            message.role
                        ),
                    ));
                }
                if message.tool_call_id.is_some() {
                    return Err(TrainingPairingError::new(
                        "orphaned_tool_result",
                        index,
                        format!(
                            "`{}` message carries a tool_call_id but is not a tool result",
                            message.role
                        ),
                    ));
                }
            }
        }
    }
    if let Some((id, name)) = expected.first() {
        return Err(TrainingPairingError::new(
            "unpaired_tool_call",
            messages.len(),
            format!("example ends with call {id} ({name}) unanswered"),
        ));
    }
    Ok(())
}
