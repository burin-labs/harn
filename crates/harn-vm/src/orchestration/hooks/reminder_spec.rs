use super::{
    optional_reminder_spec_bool, optional_reminder_spec_propagate, optional_reminder_spec_string,
    optional_reminder_spec_ttl, reminder_code_error, reminder_error, reminder_fields,
    reminder_spec_tags, required_reminder_spec_string, ReminderSpec,
};
use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
use crate::value::{VmError, VmValue};
use harn_parser::diagnostic_codes::Code;

pub(super) fn parse_reminder_spec(value: &VmValue, context: &str) -> Result<ReminderSpec, VmError> {
    let Some(options) = value.as_dict() else {
        return Err(reminder_error(
            context,
            format!("reminder spec must be a dict, got {}", value.type_name()),
        ));
    };
    const ALLOWED: &[&str] = &[
        "body",
        "tags",
        "dedupe_key",
        "ttl_turns",
        "preserve_on_compact",
        "goal_pin",
        "propagate",
        "role_hint",
        "authority",
    ];
    let unknown = options
        .keys()
        .filter(|key| !ALLOWED.contains(&key.as_str()))
        .map(|key| key.as_str())
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(reminder_code_error(
            context,
            Code::ReminderUnknownOption,
            format!("unknown reminder option(s): {}", unknown.join(", ")),
        ));
    }
    let role_hint = optional_reminder_spec_string(options, "role_hint", context)?;
    let role_hint = reminder_fields::role_hint(role_hint.as_deref())
        .map_err(|message| reminder_error(context, message))?;
    let authority = optional_reminder_spec_string(options, "authority", context)?;
    let authority = reminder_fields::authority(authority.as_deref())
        .map_err(|message| reminder_error(context, message))?;
    Ok(SystemReminder {
        goal_pin: options
            .get("goal_pin")
            .filter(|value| !matches!(value, VmValue::Nil))
            .map(|value| {
                crate::llm::helpers::GoalPinProjection::from_json(crate::llm::vm_value_to_json(
                    value,
                ))
                .map_err(|error| reminder_error(context, error))
            })
            .transpose()?,
        id: uuid::Uuid::now_v7().to_string(),
        tags: reminder_spec_tags(options, context)?,
        dedupe_key: optional_reminder_spec_string(options, "dedupe_key", context)?,
        ttl_turns: optional_reminder_spec_ttl(options, context)?,
        preserve_on_compact: optional_reminder_spec_bool(options, "preserve_on_compact", context)?
            .unwrap_or(false),
        propagate: optional_reminder_spec_propagate(options, context)?
            .unwrap_or(ReminderPropagate::Session),
        role_hint: role_hint.unwrap_or(ReminderRoleHint::System),
        authority: authority.unwrap_or_default(),
        source: ReminderSource::Hook,
        body: required_reminder_spec_string(options, "body", context)?,
        fired_at_turn: 0,
        originating_agent_id: None,
    })
}
