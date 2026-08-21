use std::collections::BTreeSet;

use harn_serve::adapters::acp::{ACP_SESSION_UPDATE_VARIANTS, HARN_SESSION_UPDATE_EXTENSIONS};
#[cfg(test)]
use harn_vm::agent_events::WorkerEvent;
use harn_vm::agent_events::{
    AgentLifecycleEvent, AgentLifecycleState, AgentTerminalKind, ToolCallErrorCategory,
    ToolCallStatus, ToolMutationStatus,
};
use harn_vm::llm::AgentTerminalClass;
use harn_vm::tool_annotations::{SideEffectLevel, ToolKind};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use super::constants::*;
use super::support::concat_unique_wire_values;

pub(super) struct AcpMethodVocabulary {
    pub(super) rust_const_prefix: &'static str,
    pub(super) rust_slice_name: &'static str,
    pub(super) rust_doc: &'static str,
    pub(super) swift_enum_name: &'static str,
    pub(super) values: Vec<String>,
    pub(super) deprecated_values: &'static [DeprecatedWireValue],
}

/// Method families projected into both the Rust and Swift artifacts.
///
/// Keeping the family list here makes adding a Rust routing vocabulary also
/// add the corresponding Swift enum. Other language bindings intentionally
/// expose only their stable host-facing subsets for now.
pub(super) fn acp_method_vocabularies() -> Vec<AcpMethodVocabulary> {
    vec![
        AcpMethodVocabulary {
            rust_const_prefix: "ACP_AGENT_METHOD",
            rust_slice_name: "ACP_AGENT_METHODS",
            rust_doc: "Stable host-facing ACP agent methods (matches the TypeScript/Swift/Python/Go bindings).",
            swift_enum_name: "HarnACPAgentMethod",
            values: strs_to_strings(ACP_AGENT_METHODS),
            deprecated_values: ACP_DEPRECATED_AGENT_METHODS,
        },
        AcpMethodVocabulary {
            rust_const_prefix: "ACP_DISPATCHED_METHOD",
            rust_slice_name: "ACP_DISPATCHED_METHODS",
            rust_doc: "Every JSON-RPC method the ACP adapter actually dispatches, including workspace-management, workflow-control, and HITL methods. Reconciled against the `match` arms in `harn-serve`'s ACP adapter.",
            swift_enum_name: "HarnACPDispatchedMethod",
            values: strs_to_strings(ACP_DISPATCHED_METHODS),
            deprecated_values: &[],
        },
        AcpMethodVocabulary {
            rust_const_prefix: "ACP_TRANSPORT_CONTROL_METHOD",
            rust_slice_name: "ACP_TRANSPORT_CONTROL_METHODS",
            rust_doc: "ACP control frames consumed by the transport before regular adapter dispatch.",
            swift_enum_name: "HarnACPTransportControlMethod",
            values: strs_to_strings(ACP_TRANSPORT_CONTROL_METHODS),
            deprecated_values: &[],
        },
        AcpMethodVocabulary {
            rust_const_prefix: "ACP_HANDLED_METHOD",
            rust_slice_name: "ACP_HANDLED_METHODS",
            rust_doc: "Every inbound ACP method Harn handles, whether by transport preemption or regular adapter dispatch.",
            swift_enum_name: "HarnACPHandledMethod",
            values: concat_unique_wire_values(&[
                ACP_TRANSPORT_CONTROL_METHODS,
                ACP_DISPATCHED_METHODS,
            ]),
            deprecated_values: &[],
        },
        AcpMethodVocabulary {
            rust_const_prefix: "ACP_CLIENT_METHOD",
            rust_slice_name: "ACP_CLIENT_METHODS",
            rust_doc: "ACP client methods the agent calls back into the host for.",
            swift_enum_name: "HarnACPClientMethod",
            values: strs_to_strings(ACP_CLIENT_METHODS),
            deprecated_values: &[],
        },
    ]
}

pub(super) fn all_acp_session_updates() -> Vec<String> {
    unique_ordered(
        ACP_SESSION_UPDATE_VARIANTS
            .iter()
            .chain(HARN_SESSION_UPDATE_EXTENSIONS.iter())
            .copied(),
    )
}

pub(super) fn tool_kind_values() -> Vec<String> {
    ToolKind::ALL.iter().map(serde_wire_string).collect()
}

pub(super) fn tool_call_status_values() -> Vec<String> {
    ToolCallStatus::ALL
        .iter()
        .map(|status| status.as_str().to_string())
        .collect()
}

pub(super) fn tool_call_error_category_values() -> Vec<String> {
    ToolCallErrorCategory::ALL
        .iter()
        .map(|category| category.as_str().to_string())
        .collect()
}

pub(super) fn tool_mutation_status_values() -> Vec<String> {
    ToolMutationStatus::ALL
        .iter()
        .map(|status| status.as_str().to_string())
        .collect()
}

pub(super) fn agent_terminal_class_values() -> Vec<String> {
    AgentTerminalClass::ALL
        .iter()
        .map(|class| class.as_str().to_string())
        .collect()
}

pub(super) fn agent_terminal_kind_values() -> Vec<String> {
    AgentTerminalKind::ALL
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect()
}

pub(super) fn agent_terminal_owner_values() -> Vec<String> {
    unique_ordered(AgentTerminalKind::ALL.iter().map(|kind| kind.owner()))
}

pub(super) fn worker_status_values() -> Vec<String> {
    // Worker wire statuses are a projection of the shared agent/run
    // lifecycle registry. Keep this list equal to
    // `AgentLifecycleState::ALL` so a new state cannot land without a
    // protocol-artifact update.
    agent_lifecycle_state_values()
}

pub(super) fn agent_lifecycle_state_values() -> Vec<String> {
    AgentLifecycleState::ALL
        .iter()
        .map(|state| state.wire_name().to_string())
        .collect()
}

pub(super) fn agent_lifecycle_event_values() -> Vec<String> {
    AgentLifecycleEvent::ALL
        .iter()
        .map(|event| event.as_str().to_string())
        .collect()
}

/// Rich projection table for schemas/docs. Aliases are listed per state
/// but never appear as independent canonical wire values.
pub(super) fn agent_lifecycle_state_projections() -> Vec<JsonValue> {
    AgentLifecycleState::ALL
        .iter()
        .map(|state| {
            let projection = state.projection();
            json!({
                "wire": projection.wire_name,
                "terminal": projection.terminal,
                "resumable": projection.resumable,
                "runRecordStatus": projection.run_record_status,
                "a2aTaskState": projection.a2a_task_state,
                "aliases": state.aliases(),
            })
        })
        .collect()
}

/// Exhaustive parity: every WorkerEvent status must resolve through the
/// lifecycle registry, and every lifecycle state must be reachable from
/// at least one worker event (Joined is event-only and excluded here).
#[cfg(test)]
pub(super) fn assert_worker_lifecycle_parity() {
    for event in WorkerEvent::ALL {
        let status = event.as_status();
        let state = AgentLifecycleState::from_wire(status)
            .unwrap_or_else(|| panic!("worker status `{status}` missing from lifecycle registry"));
        assert_eq!(state.wire_name(), status);
        assert_eq!(event.is_terminal(), state.is_terminal());
        assert_eq!(event.lifecycle_event().target_state(), Some(state));
    }
    for state in AgentLifecycleState::ALL {
        assert!(
            WorkerEvent::ALL
                .iter()
                .any(|event| event.as_status() == state.wire_name()),
            "lifecycle state `{}` has no WorkerEvent projection",
            state.wire_name()
        );
    }
}

pub(super) fn side_effect_level_values() -> Vec<String> {
    SideEffectLevel::ALL
        .iter()
        .map(|level| level.as_str().to_string())
        .collect()
}

pub(super) fn unique_ordered<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value) {
            out.push(value.to_string());
        }
    }
    out
}

pub(super) fn serde_wire_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("wire enum serializes")
        .as_str()
        .expect("wire enum serializes as string")
        .to_string()
}

pub(super) fn strs_to_strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
