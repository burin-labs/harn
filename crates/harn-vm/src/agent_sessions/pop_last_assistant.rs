//! Step-judge replacement of the trailing assistant turn.

use crate::value::VmValue;

/// Pop the trailing message iff it is an assistant message. Used by
/// `agent_step_judge` to remove a vetoed turn before regeneration.
pub fn pop_last_if_assistant(id: &str) -> Result<bool, String> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(id) else {
            return Err(format!(
                "pop_last_if_assistant: unknown session id '{id}'"
            ));
        };
        let messages = state
            .transcript
            .as_dict()
            .and_then(|dict| dict.get("messages"))
            .and_then(|messages| match messages {
                VmValue::List(messages) => Some(messages.iter().cloned().collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap_or_default();
        let Some(trailing) = messages.last() else {
            return Ok(false);
        };
        let trailing_role = trailing
            .as_dict()
            .and_then(|message| message.get("role"))
            .map(VmValue::display)
            .unwrap_or_default();
        if trailing_role != "assistant" {
            return Err(format!(
                "pop_last_if_assistant: trailing message role is '{trailing_role}', expected 'assistant'"
            ));
        }
        let source_event_id = state
            .transcript
            .as_dict()
            .and_then(|dict| dict.get("events"))
            .and_then(|events| match events {
                VmValue::List(events) => events.iter().rev().find(|event| {
                    event.as_dict().is_some_and(|event| {
                        event.get("kind").map(VmValue::display).as_deref() == Some("message")
                            && event.get("role").map(VmValue::display).as_deref()
                                == Some("assistant")
                    })
                }),
                _ => None,
            })
            .and_then(VmValue::as_dict)
            .and_then(|event| event.get("id"))
            .map(VmValue::display)
            .filter(|id| !id.is_empty());
        super::truncate_state(state, messages.len() - 1);
        crate::agent_session_journal::enqueue_message_removed(
            &mut state.transcript_journal,
            source_event_id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            crate::llm::helpers::vm_value_to_json(trailing),
        );
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_session_reports_that_nothing_was_removed() {
        super::super::reset_session_store();
        let session_id = super::super::open_or_create(Some("empty-pop".to_string()));

        assert!(!super::pop_last_if_assistant(&session_id).expect("pop empty session"));

        super::super::reset_session_store();
    }
}
