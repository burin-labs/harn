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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_preserves_explicit_tool_and_exit_authority_denials() {
        let stage: StageDecl = serde_json::from_value(serde_json::json!({
            "name": "verify",
            "allowed_tools": [],
            "on_exit": {
                "on_complete": "done",
                "policy_override": {"capabilities": {}, "capabilities_restricted": true}
            }
        }))
        .unwrap();
        assert_eq!(stage.allowed_tools, Some(Vec::new()));
        let exit = stage.on_exit.as_ref().unwrap();
        assert_eq!(exit.on_complete.as_deref(), Some("done"));
        assert!(exit
            .policy_override
            .as_ref()
            .unwrap()
            .capabilities_deny_all());
        let restored: StageDecl =
            serde_json::from_value(serde_json::to_value(&stage).unwrap()).unwrap();
        assert_eq!(restored, stage);

        let inherited: StageDecl = serde_json::from_value(serde_json::json!({
            "name": "verify", "on_exit": {"policy_override": {"capabilities": {}}}
        }))
        .unwrap();
        assert!(inherited.allowed_tools.is_none());
        assert!(!inherited
            .on_exit
            .unwrap()
            .policy_override
            .unwrap()
            .capabilities_are_restricted());
    }
}
