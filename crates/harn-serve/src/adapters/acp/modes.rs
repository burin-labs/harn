//! ACP session modes (<https://agentclientprotocol.com/protocol/session-modes>).
//!
//! A session mode caps the agent's tool access and side-effect ceiling.
//! The catalog is fixed: `default`, `architect`, `code`, `ask`. The
//! adapter advertises this catalog in `session/new` and `session/load`
//! results, accepts `session/set_mode` requests to switch the active
//! mode, and emits `current_mode_update` notifications when the mode
//! changes. While a prompt runs, the active mode's policy is pushed
//! onto the VM execution policy stack so destructive builtins are
//! rejected from `architect` mode and gated through approval flows from
//! `ask` mode.

use std::collections::BTreeMap;

use harn_vm::orchestration::CapabilityPolicy;

/// Default mode id assigned to newly created sessions. `default` is
/// chosen over `code` so existing clients that never call
/// `session/set_mode` observe the same unbounded behavior they had
/// before modes were introduced (no policy override pushed on the
/// execution stack).
pub(super) const DEFAULT_MODE_ID: &str = "default";

pub(super) struct ModeDefinition {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
}

/// The static catalog of modes Harn advertises over ACP. Order is
/// preserved on the wire so clients render a stable list. `default`
/// stays first so it remains the obvious initial selection.
pub(super) const MODE_CATALOG: &[ModeDefinition] = &[
    ModeDefinition {
        id: "default",
        name: "Default",
        description: "Execute Harn pipelines with no extra restrictions.",
    },
    ModeDefinition {
        id: "architect",
        name: "Architect",
        description: "Read-only planning. Workspace writes, process execution, and \
                      network requests are rejected; only reads, listings, and analysis \
                      are permitted.",
    },
    ModeDefinition {
        id: "code",
        name: "Code",
        description: "Full tool access for reading, writing, executing processes, and \
                      calling out to LLMs and connectors.",
    },
    ModeDefinition {
        id: "ask",
        name: "Ask",
        description: "Read-only by default. Destructive operations require host \
                      approval through the ACP permission flow before they run.",
    },
];

pub(super) fn is_known(mode_id: &str) -> bool {
    MODE_CATALOG.iter().any(|m| m.id == mode_id)
}

pub(super) fn known_mode_ids() -> Vec<&'static str> {
    MODE_CATALOG.iter().map(|m| m.id).collect()
}

/// Render the spec-shaped `SessionModeState`:
/// `{ currentModeId, availableModes: [{ id, name, description }] }`.
pub(super) fn session_mode_state(current_mode_id: &str) -> serde_json::Value {
    let available: Vec<serde_json::Value> = MODE_CATALOG
        .iter()
        .map(|mode| {
            serde_json::json!({
                "id": mode.id,
                "name": mode.name,
                "description": mode.description,
            })
        })
        .collect();
    serde_json::json!({
        "currentModeId": current_mode_id,
        "availableModes": available,
    })
}

/// Capability ceiling enforced while a prompt runs in this mode.
///
/// The returned policy is pushed onto the VM execution stack via
/// `harn_vm::orchestration::push_execution_policy` for the duration of
/// the prompt. `default` and `code` return `None` to preserve the
/// pre-modes behavior (no override). `architect` clamps the
/// side-effect ceiling to read-only. `ask` mirrors `architect`'s
/// ceiling for now: writes/exec/network are rejected by the same
/// builtin gate, which surfaces as an error the host can use to drive
/// an approval prompt. We deliberately do *not* layer
/// `ToolApprovalPolicy` here — the ACP `session/request_permission`
/// flow is the host-driven path, and the VM gate is the safety net for
/// scripts that bypass tool approval.
pub(super) fn policy_for_mode(mode_id: &str) -> Option<CapabilityPolicy> {
    match mode_id {
        "default" | "code" => None,
        "architect" | "ask" => Some(CapabilityPolicy {
            tools: Vec::new(),
            capabilities: BTreeMap::new(),
            workspace_roots: Vec::new(),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: BTreeMap::new(),
        }),
        _ => None,
    }
}

/// RAII guard that pushes a CapabilityPolicy on construction and pops
/// it on drop. For modes without an override, the guard does nothing
/// on enter and on drop, preserving the pre-modes baseline.
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
        assert!(ids.contains(&"default"));
        assert!(ids.contains(&"architect"));
        assert!(ids.contains(&"code"));
        assert!(ids.contains(&"ask"));
    }

    #[test]
    fn default_mode_is_first_in_catalog() {
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
    fn policy_for_default_and_code_is_none() {
        assert!(policy_for_mode("default").is_none());
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
    fn policy_for_unknown_mode_is_none() {
        assert!(policy_for_mode("not-a-real-mode").is_none());
    }

    #[test]
    fn is_known_rejects_unknown_mode() {
        assert!(is_known("default"));
        assert!(!is_known(""));
        assert!(!is_known("plan"));
    }
}
