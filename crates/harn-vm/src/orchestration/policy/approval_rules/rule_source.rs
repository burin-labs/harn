use serde::{Deserialize, Serialize};

use super::PolicyAction;

/// Authority of a configured rule. Mode defaults yield to a person's
/// remembered choice, while authored policy constraints do not.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyRuleSource {
    Mode,
    User,
    #[default]
    Policy,
}

impl PolicyRuleSource {
    pub(super) fn rank(self, action: PolicyAction) -> u8 {
        match (self, action) {
            // An authored refusal is a constraint, not a mode default.
            (Self::Policy, PolicyAction::Deny) => 4,
            // A remembered refusal must not turn into an allow merely because
            // a broader authored allow also matched.
            (Self::User, PolicyAction::Deny) => 3,
            (Self::Policy | Self::User, PolicyAction::Ask) => 2,
            (Self::User, PolicyAction::Allow) => 1,
            // An authored allow is permissive, not a constraint. Keep the
            // prior deny > ask > allow order against mode defaults.
            (Self::Policy, PolicyAction::Allow) | (Self::Mode, _) => 0,
        }
    }

    pub(super) fn receipt_source(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::User => "user",
            Self::Policy => "rules",
        }
    }

    pub(super) fn is_policy(&self) -> bool {
        *self == Self::Policy
    }
}
