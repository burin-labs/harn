use serde::{Deserialize, Serialize};

/// Ordered ceiling on the actions a callable may perform autonomously.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(ts_rs::TS)]
pub enum AutonomyTier {
    Shadow,
    Suggest,
    ActWithApproval,
    #[default]
    ActAuto,
}

impl AutonomyTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Suggest => "suggest",
            Self::ActWithApproval => "act_with_approval",
            Self::ActAuto => "act_auto",
        }
    }
}
