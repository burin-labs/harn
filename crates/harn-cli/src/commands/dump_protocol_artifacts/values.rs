use std::collections::BTreeSet;

use harn_serve::adapters::acp::{ACP_SESSION_UPDATE_VARIANTS, HARN_SESSION_UPDATE_EXTENSIONS};
use harn_vm::agent_events::{
    ToolCallErrorCategory, ToolCallStatus, ToolMutationStatus, WorkerEvent,
};
use harn_vm::llm::AgentTerminalClass;
use harn_vm::tool_annotations::{SideEffectLevel, ToolKind};
use serde::Serialize;

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

pub(super) fn worker_status_values() -> Vec<String> {
    // Multiple lifecycle events can share a wire status (e.g.
    // `WorkerSpawned` and `WorkerResumed` both surface as `running`).
    // The published vocabulary is a set, not a multiset — dedupe while
    // preserving the first-seen order so downstream consumers see a
    // stable list.
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for event in WorkerEvent::ALL.iter() {
        let status = event.as_status().to_string();
        if seen.insert(status.clone()) {
            out.push(status);
        }
    }
    out
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
