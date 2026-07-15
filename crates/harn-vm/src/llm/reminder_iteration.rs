//! Per-iteration reminder budgeting, delta emission, and receipts.

use serde_json::Value as JsonValue;

use crate::orchestration::ReminderSpec;
use crate::value::VmError;

const DEFAULT_REMINDER_ITERATION_BUDGET_BYTES: usize = 16 * 1024;
const MIN_REMINDER_ITERATION_BUDGET_BYTES: usize = 256;
const REMINDER_BODY_HASH_TAG_PREFIX: &str = "body_sha256:";

pub(crate) struct ReminderIterationState {
    reports: Vec<JsonValue>,
    skipped_reports: Vec<JsonValue>,
    used_body_bytes: usize,
    budget_bytes: usize,
}

impl ReminderIterationState {
    pub(crate) fn new(options: &JsonValue) -> Self {
        Self {
            reports: Vec::new(),
            skipped_reports: Vec::new(),
            used_body_bytes: 0,
            budget_bytes: reminder_iteration_budget_bytes(options),
        }
    }

    pub(crate) fn inject(
        &mut self,
        session_id: &str,
        provider_id: &str,
        reminder: ReminderSpec,
    ) -> Result<(), VmError> {
        let body_bytes = prepare_and_inject_reminder(
            session_id,
            provider_id,
            reminder,
            self.budget_bytes.saturating_sub(self.used_body_bytes),
            &mut self.reports,
            &mut self.skipped_reports,
        )?;
        self.used_body_bytes = self.used_body_bytes.saturating_add(body_bytes);
        Ok(())
    }

    pub(crate) fn into_json(self) -> JsonValue {
        serde_json::json!({
            "fired_count": self.reports.len(),
            "reports": self.reports,
            "skipped_count": self.skipped_reports.len(),
            "skipped_reports": self.skipped_reports,
            "budget_bytes": self.budget_bytes,
            "used_body_bytes": self.used_body_bytes,
        })
    }
}

struct PreparedReminder {
    reminder: ReminderSpec,
    original_body_bytes: usize,
    body_bytes: usize,
    delta_compressed: bool,
    body_hash: String,
}

impl PreparedReminder {
    fn body_bytes(&self) -> usize {
        self.body_bytes
    }
}

enum ReminderIterationDecision {
    Inject(PreparedReminder),
    Skip(JsonValue),
}

fn prepare_and_inject_reminder(
    session_id: &str,
    provider_id: &str,
    reminder: ReminderSpec,
    remaining_budget_bytes: usize,
    reports: &mut Vec<JsonValue>,
    skipped_reports: &mut Vec<JsonValue>,
) -> Result<usize, VmError> {
    match prepare_reminder_for_iteration(session_id, provider_id, reminder, remaining_budget_bytes)
    {
        ReminderIterationDecision::Inject(prepared) => {
            let body_bytes = prepared.body_bytes();
            reports.push(inject_report(session_id, provider_id, prepared)?);
            Ok(body_bytes)
        }
        ReminderIterationDecision::Skip(skipped) => {
            skipped_reports.push(skipped);
            Ok(0)
        }
    }
}

fn inject_report(
    session_id: &str,
    provider_id: &str,
    prepared: PreparedReminder,
) -> Result<JsonValue, VmError> {
    let reminder = prepared.reminder;
    let report =
        crate::agent_sessions::inject_reminder(session_id, reminder).map_err(VmError::Runtime)?;
    Ok(serde_json::json!({
        "provider": provider_id,
        "reminder_id": report.reminder_id,
        "deduped_count": report.deduped_count,
        "body_bytes": prepared.body_bytes,
        "original_body_bytes": prepared.original_body_bytes,
        "delta_compressed": prepared.delta_compressed,
        "body_sha256": prepared.body_hash,
    }))
}

fn prepare_reminder_for_iteration(
    session_id: &str,
    provider_id: &str,
    mut reminder: ReminderSpec,
    remaining_budget_bytes: usize,
) -> ReminderIterationDecision {
    let original_body = reminder.body.clone();
    let body_hash = reminder_body_hash(&original_body);
    attach_body_hash_tag(&mut reminder, &body_hash);
    let original_body_bytes = original_body.len();
    let mut delta_compressed = false;
    if !reminder.preserve_on_compact {
        if let Some(dedupe_key) = reminder.dedupe_key.as_deref() {
            if previous_reminder_has_body_hash(session_id, dedupe_key, &body_hash) {
                let delta_body = unchanged_reminder_body(dedupe_key, &body_hash);
                if delta_body.len() < reminder.body.len() {
                    reminder.body = delta_body;
                    delta_compressed = true;
                }
            }
        }
    }
    let body_bytes = reminder.body.len();
    if body_bytes > remaining_budget_bytes {
        return ReminderIterationDecision::Skip(serde_json::json!({
            "provider": provider_id,
            "reason": "reminder_iteration_budget_exceeded",
            "body_bytes": body_bytes,
            "original_body_bytes": original_body_bytes,
            "remaining_budget_bytes": remaining_budget_bytes,
            "delta_compressed": delta_compressed,
            "body_sha256": body_hash,
            "dedupe_key": &reminder.dedupe_key,
            "tags": &reminder.tags,
        }));
    }
    ReminderIterationDecision::Inject(PreparedReminder {
        reminder,
        original_body_bytes,
        body_bytes,
        delta_compressed,
        body_hash,
    })
}

fn reminder_iteration_budget_bytes(options: &JsonValue) -> usize {
    json_i64(options, "reminder_iteration_budget_bytes")
        .or_else(|| {
            options
                .get("reminders")
                .and_then(|reminders| json_i64(reminders, "max_iteration_bytes"))
        })
        .or_else(|| {
            options
                .get("reminders")
                .and_then(|reminders| reminders.get("config"))
                .and_then(|config| json_i64(config, "max_iteration_bytes"))
        })
        .and_then(|value| usize::try_from(value).ok())
        .map(|value| value.max(MIN_REMINDER_ITERATION_BUDGET_BYTES))
        .unwrap_or(DEFAULT_REMINDER_ITERATION_BUDGET_BYTES)
}

fn reminder_body_hash(body: &str) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(body.as_bytes());
    hex::encode(hasher.finalize())
}

fn attach_body_hash_tag(reminder: &mut ReminderSpec, body_hash: &str) {
    reminder
        .tags
        .retain(|tag| !tag.starts_with(REMINDER_BODY_HASH_TAG_PREFIX));
    reminder
        .tags
        .push(format!("{REMINDER_BODY_HASH_TAG_PREFIX}{body_hash}"));
}

fn previous_reminder_has_body_hash(session_id: &str, dedupe_key: &str, body_hash: &str) -> bool {
    let expected_tag = format!("{REMINDER_BODY_HASH_TAG_PREFIX}{body_hash}");
    let Some(snapshot) = crate::agent_sessions::snapshot(session_id) else {
        return false;
    };
    let Some(events) = snapshot
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|events| match events {
            crate::value::VmValue::List(events) => Some(events),
            _ => None,
        })
    else {
        return false;
    };
    events.iter().any(|event| {
        let Some(existing) = crate::llm::helpers::reminder_from_event(event) else {
            return false;
        };
        existing.dedupe_key.as_deref() == Some(dedupe_key)
            && existing.tags.iter().any(|tag| tag == &expected_tag)
    })
}

fn unchanged_reminder_body(dedupe_key: &str, body_hash: &str) -> String {
    format!(
        "Reminder `{dedupe_key}` is unchanged from its previous full emission; continue applying it. body_sha256={body_hash}"
    )
}

fn json_i64(value: &JsonValue, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| match value {
        JsonValue::Number(number) => number.as_i64(),
        JsonValue::String(value) => value.parse::<i64>().ok(),
        _ => None,
    })
}
