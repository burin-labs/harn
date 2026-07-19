//! Restore canonical transcript-event identifiers after a cold resume.

use crate::value::{VmDictExt, VmValue};

pub(crate) fn restore_message_event_ids(
    id: &str,
    source_event_ids: &[Option<String>],
) -> Result<(), String> {
    if !source_event_ids.iter().any(Option::is_some) {
        return Ok(());
    }
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(id) else {
            return Err(format!(
                "restore_message_event_ids: unknown session id '{id}'"
            ));
        };
        let Some(mut transcript) = state.transcript.as_dict().cloned() else {
            return Ok(());
        };
        let Some(VmValue::List(events)) = transcript.get("events") else {
            return Ok(());
        };
        let restored = events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let Some(Some(source_event_id)) = source_event_ids.get(index) else {
                    return event.clone();
                };
                let Some(mut event) = event.as_dict().cloned() else {
                    return event.clone();
                };
                event.put_str("id", source_event_id);
                VmValue::dict(event)
            })
            .collect();
        transcript.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(restored)),
        );
        super::apply_transcript_with_budget(
            state,
            VmValue::dict(transcript),
            "restore_message_event_ids",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_canonical_ids_without_truncating_unmapped_events() {
        super::super::reset_session_store();
        let session_id = super::super::seed_from_messages(
            Some("restore-event-ids".to_string()),
            &[
                serde_json::json!({"role": "user", "content": "first"}),
                serde_json::json!({"role": "assistant", "content": "second"}),
            ],
            serde_json::json!({}),
            None,
            None,
        )
        .expect("seed session");

        restore_message_event_ids(&session_id, &[Some("canonical-user".to_string()), None])
            .expect("restore canonical id");

        let transcript = super::super::transcript(&session_id).expect("transcript");
        let events = transcript
            .as_dict()
            .and_then(|transcript| transcript.get("events"))
            .and_then(|events| match events {
                VmValue::List(events) => Some(events),
                _ => None,
            })
            .expect("event list");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0]
                .as_dict()
                .and_then(|event| event.get("id"))
                .map(VmValue::display),
            Some("canonical-user".to_string())
        );
        assert!(
            events[1]
                .as_dict()
                .and_then(|event| event.get("id"))
                .is_some(),
            "an unmapped event must remain in the reconstructed transcript"
        );
        super::super::reset_session_store();
    }
}
