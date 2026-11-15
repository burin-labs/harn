//! Reminder state transitions shared by transcript and live-session compaction.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use super::{
    emit_reminder_lifecycle_event, reminder_from_event, reminder_lifecycle_payload,
    replace_reminder_payload, SystemReminder, REMINDER_DEDUPED_EVENT_KIND,
    REMINDER_EXPIRED_EVENT_KIND,
};
use crate::value::VmValue;

/// The single reminder-policy result used by compaction persistence and receipts.
#[derive(Debug, Default)]
pub struct ReminderCompactReport {
    /// Non-reminder events plus reminders flagged `preserve_on_compact`.
    pub preserved_events: Vec<VmValue>,
    /// Reminder values supplied to custom summarization callbacks.
    pub custom_reminders: Vec<VmValue>,
    pub expired: Vec<SystemReminder>,
    pub compacted: Vec<SystemReminder>,
    pub deduped: Vec<ReminderDedupeRecord>,
    pub decremented_count: usize,
    pub preserved_count: usize,
}

impl ReminderCompactReport {
    pub fn count_fields(&self) -> serde_json::Map<String, JsonValue> {
        serde_json::Map::from_iter([
            (
                "reminders_decremented".into(),
                self.decremented_count.into(),
            ),
            ("reminders_expired".into(), self.expired.len().into()),
            ("reminders_deduped".into(), self.deduped.len().into()),
            ("reminders_preserved".into(), self.preserved_count.into()),
        ])
    }
}

#[derive(Clone, Debug)]
pub struct ReminderDedupeRecord {
    pub replaced_id: String,
    pub replacing_id: String,
    pub dedupe_key: String,
}

enum CompactEvent {
    Other(VmValue),
    Reminder {
        event: VmValue,
        reminder: SystemReminder,
        reminder_index: usize,
    },
}

/// Expire/decrement TTLs, dedupe, and retain `preserve_on_compact` reminders.
pub fn compact_reminder_events(extra_events: Vec<VmValue>) -> ReminderCompactReport {
    let mut events = Vec::with_capacity(extra_events.len());
    let mut reminders = Vec::new();
    let mut expired = Vec::new();
    let mut decremented_count = 0;

    for event in extra_events {
        let Some(reminder) = reminder_from_event(&event) else {
            events.push(CompactEvent::Other(event));
            continue;
        };
        let (event, reminder) = match reminder.ttl_turns {
            Some(ttl) if ttl <= 1 => {
                expired.push(reminder);
                continue;
            }
            Some(ttl) => {
                let mut updated = reminder;
                updated.ttl_turns = Some(ttl - 1);
                decremented_count += 1;
                (replace_reminder_payload(&event, &updated), updated)
            }
            None => (event, reminder),
        };
        let reminder_index = reminders.len();
        reminders.push(reminder.clone());
        events.push(CompactEvent::Reminder {
            event,
            reminder,
            reminder_index,
        });
    }

    let mut newest_by_dedupe_key = BTreeMap::new();
    for (index, reminder) in reminders.iter().enumerate() {
        if let Some(dedupe_key) = reminder.dedupe_key.as_deref() {
            newest_by_dedupe_key.insert(dedupe_key.to_string(), index);
        }
    }
    let mut kept_reminders = Vec::new();
    let mut preserved_events = Vec::new();
    let mut compacted = Vec::new();
    let mut deduped = Vec::new();
    let mut preserved_count = 0;
    for event in events {
        match event {
            CompactEvent::Other(event) => preserved_events.push(event),
            CompactEvent::Reminder {
                event,
                reminder,
                reminder_index,
            } => {
                let keep = reminder
                    .dedupe_key
                    .as_deref()
                    .and_then(|key| newest_by_dedupe_key.get(key))
                    .is_none_or(|newest| *newest == reminder_index);
                if !keep {
                    let replacing_id = reminder
                        .dedupe_key
                        .as_deref()
                        .and_then(|key| newest_by_dedupe_key.get(key))
                        .and_then(|index| reminders.get(*index))
                        .map(|newest| newest.id.clone())
                        .unwrap_or_default();
                    deduped.push(ReminderDedupeRecord {
                        replaced_id: reminder.id.clone(),
                        replacing_id,
                        dedupe_key: reminder.dedupe_key.clone().unwrap_or_default(),
                    });
                    continue;
                }
                kept_reminders.push(crate::value::json::json_to_vm_value(
                    &serde_json::to_value(&reminder).unwrap_or(JsonValue::Null),
                ));
                if reminder.preserve_on_compact {
                    preserved_count += 1;
                    preserved_events.push(event);
                } else {
                    compacted.push(reminder);
                }
            }
        }
    }
    ReminderCompactReport {
        preserved_events,
        custom_reminders: kept_reminders,
        expired,
        compacted,
        deduped,
        decremented_count,
        preserved_count,
    }
}

pub(crate) fn emit_reminder_lifecycle_records(
    transcript_id: Option<&str>,
    report: &ReminderCompactReport,
) {
    for reminder in &report.expired {
        let mut payload = reminder_lifecycle_payload(transcript_id, reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(transcript_id),
            );
            obj.insert("reason".to_string(), JsonValue::String("ttl".to_string()));
            obj.insert(
                "ttl_turns_before".to_string(),
                serde_json::json!(reminder.ttl_turns),
            );
            obj.insert("expired_at_turn".to_string(), JsonValue::Null);
            obj.insert(
                "expired_at_boundary".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }
    for reminder in &report.compacted {
        let mut payload = reminder_lifecycle_payload(transcript_id, reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(transcript_id),
            );
            obj.insert(
                "reason".to_string(),
                JsonValue::String("compaction".to_string()),
            );
            obj.insert(
                "expired_at_boundary".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }
    if !report.deduped.is_empty() {
        let dropped_reminder_ids = report
            .deduped
            .iter()
            .map(|record| record.replaced_id.clone())
            .collect::<Vec<_>>();
        emit_reminder_lifecycle_event(
            REMINDER_DEDUPED_EVENT_KIND,
            serde_json::json!({
                "transcript_id": transcript_id,
                "boundary": "pre_compact",
                "replaced_id": report.deduped.first().map(|record| &record.replaced_id),
                "replacing_id": report.deduped.first().map(|record| &record.replacing_id),
                "dedupe_key": report.deduped.first().map(|record| &record.dedupe_key),
                "replaced_ids": &dropped_reminder_ids,
                "dropped_reminder_ids": &dropped_reminder_ids,
                "dropped_count": dropped_reminder_ids.len(),
            }),
        );
    }
}

/// Canonical filter for live transcript compaction callers. Audit events are
/// returned too; callers replacing only messages must select reminders alone.
pub fn transcript_compactable_events(transcript: &crate::value::DictMap) -> Vec<VmValue> {
    transcript
        .get("events")
        .and_then(|events| match events {
            VmValue::List(list) => Some(
                list.iter()
                    .filter(|event| {
                        event
                            .as_dict()
                            .and_then(|dict| dict.get("kind"))
                            .map(|value| value.display())
                            .is_some_and(|kind| kind != "message" && kind != "tool_result")
                    })
                    .cloned()
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}
