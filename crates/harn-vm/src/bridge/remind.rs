//! Validation for `session/remind` payloads: the reminder shape a host may
//! queue, with typed error codes for unknown options and malformed fields.

use harn_parser::diagnostic_codes::Code;

use super::{authority, QueuedReminder, QueuedUserMessageMode};

fn reminder_unknown_option_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderUnknownOption.as_str(),
        message.as_ref()
    )
}

fn session_remind_shape_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderInvalidShape.as_str(),
        message.as_ref()
    )
}

fn reminder_unknown_propagate_error(message: impl AsRef<str>) -> String {
    format!(
        "{}: {}",
        Code::ReminderUnknownPropagate.as_str(),
        message.as_ref()
    )
}

fn string_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    required: bool,
) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) if required => Err(session_remind_shape_error(
            format!("`{key}` must be a non-empty string"),
        )),
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if required && value.trim().is_empty() => Err(
            session_remind_shape_error(format!("`{key}` must be a non-empty string")),
        ),
        Some(serde_json::Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be a string, got {other}"
        ))),
    }
}

fn bool_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<bool>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be a bool, got {other}"
        ))),
    }
}

fn int_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>, String> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => {
            let Some(value) = value.as_i64() else {
                return Err(session_remind_shape_error(format!(
                    "`{key}` must be an integer"
                )));
            };
            Ok(Some(value))
        }
        Some(other) => Err(session_remind_shape_error(format!(
            "`{key}` must be an int, got {other}"
        ))),
    }
}

fn tags_field(map: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<String>, String> {
    let Some(value) = map.get("tags") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(values) = value.as_array() else {
        return Err(session_remind_shape_error("`tags` must be a list"));
    };
    let mut tags = Vec::new();
    for value in values {
        let Some(tag) = value.as_str() else {
            return Err(session_remind_shape_error(format!(
                "`tags` entries must be strings, got {value}"
            )));
        };
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(session_remind_shape_error(
                "`tags` entries must be non-empty strings",
            ));
        }
        if !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    Ok(tags)
}

fn session_remind_payload_from_value(
    value: &serde_json::Value,
) -> Result<crate::llm::helpers::SystemReminder, String> {
    let Some(map) = value.as_object() else {
        return Err(session_remind_shape_error(
            "session/remind payload must be a reminder object",
        ));
    };
    const ALLOWED: &[&str] = &[
        "_meta",
        "body",
        "dedupe_key",
        "fired_at_turn",
        "id",
        "preserve_on_compact",
        "propagate",
        "role_hint",
        "authority",
        "source",
        "tags",
        "ttl_turns",
    ];
    let unknown = map
        .keys()
        .filter(|key| !ALLOWED.contains(&key.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        if unknown.contains(&"content") {
            return Err(session_remind_shape_error(
                "session/remind expects reminder `body`, not user-message `content`",
            ));
        }
        return Err(reminder_unknown_option_error(format!(
            "unknown reminder option(s): {}",
            unknown.join(", ")
        )));
    }
    if let Some(meta) = map.get("_meta") {
        if !meta.is_null() && !meta.is_object() {
            return Err(session_remind_shape_error("`_meta` must be an object"));
        }
    }
    let ttl_turns = int_field(map, "ttl_turns")?;
    if let Some(value) = ttl_turns {
        if value <= 0 {
            return Err(session_remind_shape_error("`ttl_turns` must be > 0"));
        }
    }
    let fired_at_turn = int_field(map, "fired_at_turn")?.unwrap_or(0);
    if fired_at_turn < 0 {
        return Err(session_remind_shape_error(
            "`fired_at_turn` must be >= 0 when provided",
        ));
    }
    match string_field(map, "source", false)?.as_deref() {
        None | Some("bridge") => {}
        Some(_) => {
            return Err(session_remind_shape_error(
                "`source` for session/remind must be bridge when provided",
            ))
        }
    }
    let propagate = match string_field(map, "propagate", false)?.as_deref() {
        None => crate::llm::helpers::ReminderPropagate::Session,
        Some("all") => crate::llm::helpers::ReminderPropagate::All,
        Some("session") => crate::llm::helpers::ReminderPropagate::Session,
        Some("none") => crate::llm::helpers::ReminderPropagate::None,
        Some(_) => {
            return Err(reminder_unknown_propagate_error(
                "`propagate` must be one of all, session, or none",
            ))
        }
    };
    let role_hint =
        authority::reminder_role_hint(string_field(map, "role_hint", false)?.as_deref())
            .map_err(session_remind_shape_error)?;
    let authority =
        authority::directive_authority(string_field(map, "authority", false)?.as_deref())
            .map_err(session_remind_shape_error)?;
    Ok(crate::llm::helpers::SystemReminder {
        id: string_field(map, "id", false)?.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        tags: tags_field(map)?,
        dedupe_key: string_field(map, "dedupe_key", false)?,
        ttl_turns,
        preserve_on_compact: bool_field(map, "preserve_on_compact")?.unwrap_or(false),
        propagate,
        role_hint,
        authority,
        source: crate::llm::helpers::ReminderSource::Bridge,
        body: string_field(map, "body", true)?.unwrap_or_default(),
        fired_at_turn,
        originating_agent_id: None,
    })
}

pub(super) fn queued_session_remind_from_params(
    params: &serde_json::Value,
) -> Result<QueuedReminder, String> {
    let mode = QueuedUserMessageMode::from_str(
        params
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("audit_only"),
    );
    let reminder_value = if let Some(reminder) = params.get("reminder") {
        reminder.clone()
    } else {
        let Some(params) = params.as_object() else {
            return Err(session_remind_shape_error(
                "session/remind params must be an object",
            ));
        };
        let mut reminder = params.clone();
        reminder.remove("mode");
        reminder.remove("sessionId");
        reminder.remove("session_id");
        serde_json::Value::Object(reminder)
    };
    Ok(QueuedReminder {
        reminder: session_remind_payload_from_value(&reminder_value)?,
        mode,
    })
}
