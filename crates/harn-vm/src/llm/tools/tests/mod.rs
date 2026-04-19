//! Unit tests for `crate::llm::tools`, split by subject.
//!
//! Shared imports and fixtures live here so each submodule can stay on
//! `use super::*;` while `tools/mod.rs` continues to declare only
//! `#[cfg(test)] mod tests;`.

pub(super) use super::{
    apply_tool_search_native_injection, build_assistant_response_message,
    build_assistant_tool_message, build_tool_calling_contract_prompt, build_tool_result_message,
    collect_tool_schemas, collect_tool_schemas_with_registry, extract_deferred_tool_names,
    handle_tool_locally, json_schema_to_type_expr, normalize_tool_args, parse_bare_calls_in_body,
    parse_native_json_tool_calls, parse_text_tool_calls_with_tools, validate_tool_args,
    vm_tools_to_native, ComponentRegistry, TEXT_RESPONSE_PROTOCOL_HELP,
};
pub(super) use crate::value::VmValue;
pub(super) use serde_json::json;

mod bind;
mod execute;
mod format;
mod mock_fixtures;
mod parse;
mod registry;

pub(super) use mock_fixtures::{
    defer_loading_registry, known_tools_set, sample_tool_registry, vm_bool, vm_dict, vm_list,
    vm_str,
};
