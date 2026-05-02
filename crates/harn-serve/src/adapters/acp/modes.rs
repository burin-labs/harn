//! ACP session modes (<https://agentclientprotocol.com/protocol/session-modes>).
//!
//! A session mode is the ACP-facing name for Harn's runtime autonomy tier.
//! The catalog is fixed and is rendered both as legacy ACP `modes` and as
//! the newer `configOptions` mode selector.

use harn_vm::{orchestration::CapabilityPolicy, AutonomyTier};

/// Default mode id assigned to newly created sessions. `ask` is the
/// conservative ACP default: the agent can inspect context, but side effects
/// are held behind the approval-oriented autonomy tier until a client or user
/// explicitly switches to `code`.
pub(super) const DEFAULT_MODE_ID: &str = "ask";

pub(super) struct ModeDefinition {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    autonomy_tier: AutonomyTier,
}

/// The static catalog of modes Harn advertises over ACP. Order is preserved on
/// the wire so clients render a stable selector.
pub(super) const MODE_CATALOG: &[ModeDefinition] = &[
    ModeDefinition {
        id: "ask",
        name: "Ask",
        description: "Request permission before making changes.",
        autonomy_tier: AutonomyTier::ActWithApproval,
    },
    ModeDefinition {
        id: "architect",
        name: "Architect",
        description: "Design and plan without modifying the workspace.",
        autonomy_tier: AutonomyTier::Suggest,
    },
    ModeDefinition {
        id: "code",
        name: "Code",
        description: "Read, write, execute processes, and call external services.",
        autonomy_tier: AutonomyTier::ActAuto,
    },
    ModeDefinition {
        id: "shadow",
        name: "Shadow",
        description: "Evaluate the request and emit proposals without side effects.",
        autonomy_tier: AutonomyTier::Shadow,
    },
];

pub(super) fn is_known(mode_id: &str) -> bool {
    definition(mode_id).is_some()
}

pub(super) fn known_mode_ids() -> Vec<&'static str> {
    MODE_CATALOG.iter().map(|m| m.id).collect()
}

fn definition(mode_id: &str) -> Option<&'static ModeDefinition> {
    MODE_CATALOG.iter().find(|m| m.id == mode_id)
}

/// Render the spec-shaped `SessionModeState`:
/// `{ currentModeId, availableModes: [{ id, name, description }] }`.
pub(super) fn session_mode_state(current_mode_id: &str) -> serde_json::Value {
    serde_json::json!({
        "currentModeId": current_mode_id,
        "availableModes": mode_entries("id"),
    })
}

/// Render the preferred ACP `configOptions` representation for the same mode
/// catalog. ACP clients that understand this field should prefer it over
/// `modes`; Harn keeps both in sync while the protocol transitions.
pub(super) fn config_options_state(current_mode_id: &str) -> serde_json::Value {
    serde_json::json!([
        {
            "id": "mode",
            "name": "Session Mode",
            "description": "Controls Harn autonomy and side-effect policy.",
            "category": "mode",
            "type": "select",
            "currentValue": current_mode_id,
            "options": mode_entries("value"),
        }
    ])
}

fn mode_entries(id_key: &str) -> Vec<serde_json::Value> {
    MODE_CATALOG
        .iter()
        .map(|mode| {
            let mut entry = serde_json::Map::new();
            entry.insert(id_key.to_string(), serde_json::json!(mode.id));
            entry.insert("name".to_string(), serde_json::json!(mode.name));
            entry.insert(
                "description".to_string(),
                serde_json::json!(mode.description),
            );
            serde_json::Value::Object(entry)
        })
        .collect()
}

/// Capability ceiling enforced while a prompt runs in this mode. Harn's
/// autonomy-tier policy remains authoritative; ACP modes only select the tier.
pub(super) fn policy_for_mode(mode_id: &str) -> Option<CapabilityPolicy> {
    let mode = definition(mode_id)?;
    if mode.autonomy_tier == AutonomyTier::ActAuto {
        // Full-access mode leaves the ambient host/runtime policy as the
        // authority. Installing a no-op ceiling would still make legacy bridge
        // fallbacks look policy-governed and block them.
        return None;
    }
    Some(harn_vm::policy_for_autonomy_tier(mode.autonomy_tier))
}

/// RAII guard that pushes a CapabilityPolicy on construction and pops it on
/// drop. Full-access `code` mode has no extra policy to push.
pub(super) struct ModePolicyGuard {
    pushed: bool,
}

impl ModePolicyGuard {
    pub(super) fn enter(mode_id: &str) -> Self {
        match policy_for_mode(mode_id) {
            Some(policy) => {
                harn_vm::orchestration::push_execution_policy(policy);
                Self { pushed: true }
            }
            None => Self { pushed: false },
        }
    }
}

impl Drop for ModePolicyGuard {
    fn drop(&mut self) {
        if self.pushed {
            harn_vm::orchestration::pop_execution_policy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_expected_modes() {
        let ids = known_mode_ids();
        assert!(ids.contains(&"ask"));
        assert!(ids.contains(&"architect"));
        assert!(ids.contains(&"code"));
        assert!(ids.contains(&"shadow"));
    }

    #[test]
    fn default_mode_matches_first_catalog_entry() {
        assert_eq!(MODE_CATALOG.first().map(|m| m.id), Some(DEFAULT_MODE_ID));
    }

    #[test]
    fn session_mode_state_contains_current_and_available() {
        let state = session_mode_state("architect");
        assert_eq!(state["currentModeId"], "architect");
        let available = state["availableModes"].as_array().expect("array");
        assert_eq!(available.len(), MODE_CATALOG.len());
        assert!(available
            .iter()
            .any(|m| m["id"] == "architect" && m["name"] == "Architect"));
    }

    #[test]
    fn config_options_state_contains_mode_selector() {
        let state = config_options_state("code");
        let options = state.as_array().expect("config options array");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["id"], "mode");
        assert_eq!(options[0]["currentValue"], "code");
        assert!(options[0]["options"]
            .as_array()
            .expect("mode options")
            .iter()
            .any(|m| m["value"] == "ask"));
    }

    #[test]
    fn policy_for_code_is_none() {
        assert!(policy_for_mode("code").is_none());
    }

    #[test]
    fn policy_for_architect_clamps_to_read_only() {
        let policy = policy_for_mode("architect").expect("architect has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
    }

    #[test]
    fn policy_for_ask_clamps_to_read_only() {
        let policy = policy_for_mode("ask").expect("ask has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("read_only"));
    }

    #[test]
    fn policy_for_shadow_blocks_side_effects() {
        let policy = policy_for_mode("shadow").expect("shadow has policy");
        assert_eq!(policy.side_effect_level.as_deref(), Some("none"));
        assert_eq!(policy.recursion_limit, Some(0));
    }

    #[test]
    fn policy_for_unknown_mode_is_none() {
        assert!(policy_for_mode("not-a-real-mode").is_none());
    }

    #[test]
    fn is_known_rejects_unknown_mode() {
        assert!(is_known("ask"));
        assert!(!is_known(""));
        assert!(!is_known("plan"));
    }
}
