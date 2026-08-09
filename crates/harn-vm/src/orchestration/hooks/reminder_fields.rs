//! Typed parsing for reminder fields shared by hook registration paths.

use crate::llm::helpers::{DirectiveAuthority, ReminderRoleHint};

pub(super) fn role_hint(value: Option<&str>) -> Result<Option<ReminderRoleHint>, &'static str> {
    match value {
        None => Ok(None),
        Some("system") => Ok(Some(ReminderRoleHint::System)),
        Some("developer") => Ok(Some(ReminderRoleHint::Developer)),
        Some("user_block") => Ok(Some(ReminderRoleHint::UserBlock)),
        Some("ephemeral_cache") => Ok(Some(ReminderRoleHint::EphemeralCache)),
        Some(_) => {
            Err("`role_hint` must be one of system, developer, user_block, or ephemeral_cache")
        }
    }
}

pub(super) fn authority(value: Option<&str>) -> Result<Option<DirectiveAuthority>, &'static str> {
    match value {
        None => Ok(None),
        Some("contract") => Ok(Some(DirectiveAuthority::Contract)),
        Some("corrective") => Ok(Some(DirectiveAuthority::Corrective)),
        Some("advisory") => Ok(Some(DirectiveAuthority::Advisory)),
        Some(_) => Err("`authority` must be one of contract, corrective, or advisory"),
    }
}
