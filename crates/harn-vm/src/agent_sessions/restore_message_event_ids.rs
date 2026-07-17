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
