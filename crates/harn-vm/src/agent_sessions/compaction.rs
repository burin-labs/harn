//! Apply message-only loop compaction to an active session.

use crate::llm::helpers::{
    compact_reminder_events, emit_reminder_lifecycle_records, reminder_from_event,
    transcript_compactable_events, ReminderCompactReport,
};

pub(crate) fn apply_compacted_session_messages(
    session_id: &str,
    messages: &[serde_json::Value],
    summary: Option<&str>,
) -> Result<ReminderCompactReport, String> {
    let reminder_events = super::transcript(session_id)
        .and_then(|transcript| transcript.as_dict().map(transcript_compactable_events))
        .unwrap_or_default()
        .into_iter()
        .filter(|event| reminder_from_event(event).is_some())
        .collect();
    let report = compact_reminder_events(reminder_events);
    super::replace_messages_with_summary(session_id, messages, summary)?;
    for event in &report.preserved_events {
        super::append_event(session_id, event.clone())?;
    }
    emit_reminder_lifecycle_records(Some(session_id), &report);
    Ok(report)
}
