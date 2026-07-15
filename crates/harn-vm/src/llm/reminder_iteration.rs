//! Per-iteration reminder budgeting, delta emission, and receipts.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::llm::api::ReminderLifecycleEmission;
use crate::orchestration::ReminderSpec;
use crate::value::VmError;

const DEFAULT_REMINDER_ITERATION_BUDGET_BYTES: usize = 16 * 1024;
const MIN_REMINDER_ITERATION_BUDGET_BYTES: usize = 256;
pub(crate) const REMINDER_BODY_HASH_TAG_PREFIX: &str = "body_sha256:";

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

#[derive(Default)]
struct ReminderSummaryBucket {
    count: usize,
    body_bytes: usize,
    rendered_bytes: usize,
}

impl ReminderSummaryBucket {
    fn observe(&mut self, reminder: &ReminderLifecycleEmission) {
        self.count += 1;
        self.body_bytes += reminder.body_bytes;
        self.rendered_bytes += reminder.rendered_bytes;
    }

    fn to_json(&self, key_name: &str, key: &str) -> JsonValue {
        serde_json::json!({
            key_name: key,
            "count": self.count,
            "body_bytes": self.body_bytes,
            "rendered_bytes": self.rendered_bytes,
        })
    }
}

pub(crate) fn emit_reminder_iteration_summary(reminders: &[ReminderLifecycleEmission]) {
    let Some(payload) = reminder_iteration_summary_payload(reminders) else {
        return;
    };
    crate::llm::helpers::emit_reminder_lifecycle_event(
        crate::llm::helpers::REMINDER_ITERATION_SUMMARY_EVENT_KIND,
        payload,
    );
}

fn reminder_iteration_summary_payload(
    reminders: &[ReminderLifecycleEmission],
) -> Option<JsonValue> {
    if reminders.is_empty() {
        return None;
    }
    let mut by_tag: BTreeMap<String, ReminderSummaryBucket> = BTreeMap::new();
    let mut by_source: BTreeMap<String, ReminderSummaryBucket> = BTreeMap::new();
    let mut by_rendered_role: BTreeMap<String, ReminderSummaryBucket> = BTreeMap::new();
    let mut total = ReminderSummaryBucket::default();
    let mut untagged_count = 0usize;
    let mut max_body_bytes = 0usize;
    let mut max_rendered_bytes = 0usize;
    for reminder in reminders {
        total.observe(reminder);
        max_body_bytes = max_body_bytes.max(reminder.body_bytes);
        max_rendered_bytes = max_rendered_bytes.max(reminder.rendered_bytes);
        if reminder.tags.is_empty() {
            untagged_count += 1;
        }
        for tag in &reminder.tags {
            if tag.starts_with(REMINDER_BODY_HASH_TAG_PREFIX) {
                continue;
            }
            by_tag.entry(tag.clone()).or_default().observe(reminder);
        }
        by_source
            .entry(reminder.source.clone())
            .or_default()
            .observe(reminder);
        by_rendered_role
            .entry(reminder.rendered_role.clone())
            .or_default()
            .observe(reminder);
    }
    Some(serde_json::json!({
        "session_id": reminders.iter().find_map(|reminder| reminder.session_id.clone()),
        "turn_number": reminders[0].turn_number,
        "count": total.count,
        "body_bytes": total.body_bytes,
        "rendered_bytes": total.rendered_bytes,
        "max_body_bytes": max_body_bytes,
        "max_rendered_bytes": max_rendered_bytes,
        "untagged_count": untagged_count,
        "by_tag": summary_map_to_json(by_tag, "tag"),
        "by_source": summary_map_to_json(by_source, "source"),
        "by_rendered_role": summary_map_to_json(by_rendered_role, "rendered_role"),
    }))
}

fn summary_map_to_json(
    buckets: BTreeMap<String, ReminderSummaryBucket>,
    key_name: &str,
) -> JsonValue {
    JsonValue::Array(
        buckets
            .into_iter()
            .map(|(key, bucket)| bucket.to_json(key_name, &key))
            .collect(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reminder_emission(
        tags: &[&str],
        body_bytes: usize,
        rendered_bytes: usize,
        source: &str,
        rendered_role: &str,
    ) -> ReminderLifecycleEmission {
        ReminderLifecycleEmission {
            session_id: Some("session-1".to_string()),
            turn_number: 7,
            reminder_id: format!("reminder-{body_bytes}-{rendered_bytes}"),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            body: "x".repeat(body_bytes),
            dedupe_key: None,
            source: source.to_string(),
            role_hint: "system".to_string(),
            rendered_role: rendered_role.to_string(),
            body_bytes,
            rendered_bytes,
            ttl_turns: Some(1),
            propagate: "session".to_string(),
            originating_agent_id: None,
        }
    }

    #[test]
    fn reminder_iteration_summary_rolls_up_bytes_by_tag_source_and_role() {
        let payload = reminder_iteration_summary_payload(&[
            reminder_emission(
                &["workspace", "standing", "body_sha256:ignored"],
                10,
                30,
                "stdlib_provider",
                "user",
            ),
            reminder_emission(&["workspace"], 5, 20, "stdlib_provider", "user"),
            reminder_emission(&[], 7, 9, "bridge", "developer"),
        ])
        .expect("summary payload");

        assert_eq!(payload["session_id"], "session-1");
        assert_eq!(payload["turn_number"], 7);
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["body_bytes"], 22);
        assert_eq!(payload["rendered_bytes"], 59);
        assert_eq!(payload["max_body_bytes"], 10);
        assert_eq!(payload["max_rendered_bytes"], 30);
        assert_eq!(payload["untagged_count"], 1);
        assert_eq!(
            payload["by_tag"],
            serde_json::json!([
                {"tag": "standing", "count": 1, "body_bytes": 10, "rendered_bytes": 30},
                {"tag": "workspace", "count": 2, "body_bytes": 15, "rendered_bytes": 50},
            ])
        );
        assert_eq!(
            payload["by_source"],
            serde_json::json!([
                {"source": "bridge", "count": 1, "body_bytes": 7, "rendered_bytes": 9},
                {"source": "stdlib_provider", "count": 2, "body_bytes": 15, "rendered_bytes": 50},
            ])
        );
        assert_eq!(
            payload["by_rendered_role"],
            serde_json::json!([
                {"rendered_role": "developer", "count": 1, "body_bytes": 7, "rendered_bytes": 9},
                {"rendered_role": "user", "count": 2, "body_bytes": 15, "rendered_bytes": 50},
            ])
        );
    }
}
