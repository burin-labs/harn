//! Per-stage narrowing of execution authority and named exit transitions.

use crate::CapabilityPolicy;
use serde::{Deserialize, Serialize};

/// Names an annotated step and narrows its inherited tool and effect ceilings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageDecl {
    pub name: String,
    /// Missing inherits the persona allowlist; an empty list denies every tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Optional tighter side-effect ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect_level: Option<String>,
    /// Optional agent-loop iteration cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<StageExit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageExit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_override: Option<CapabilityPolicy>,
}
