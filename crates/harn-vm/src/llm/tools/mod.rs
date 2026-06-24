mod collect;
mod compat;
mod components;
mod handle_local;
mod json_schema;
mod messages;
mod native;
mod params;
mod parse;
mod protocol;
mod ts_value_parser;
mod type_expr;

pub(crate) use collect::{collect_tool_schemas, validate_tool_args, ToolSchema};
pub(crate) use compat::normalize_tool_call_shape;
pub(crate) use handle_local::{handle_tool_locally, is_vm_stdlib_short_circuit};
#[cfg(test)]
pub(crate) use messages::build_assistant_tool_message;
pub(crate) use messages::{build_assistant_response_message, normalize_tool_args};
pub(crate) use native::{
    apply_tool_search_native_injection_typed, extract_deferred_tool_names, vm_tools_to_native,
};
pub(crate) use parse::ident_length;
pub(crate) use parse::parse_fenced_json_tool_calls;
pub(crate) use parse::parse_text_tool_argument_payload;
pub(crate) use parse::parse_text_tool_calls_in_format;
pub(crate) use parse::parse_text_tool_calls_with_tools;
pub(crate) use parse::StreamingToolCallDetector;
pub(crate) use parse::TextToolFormat;
#[cfg(test)]
pub(crate) use parse::{parse_bare_calls_in_body, parse_native_json_tool_calls};
pub(crate) use protocol::{
    text_tool_call_block, text_tool_call_tag_pairs, TEXT_TOOL_CALL_CLOSE,
    TEXT_TOOL_CALL_CLOSE_COMPACT, TEXT_TOOL_CALL_OPEN, TEXT_TOOL_CALL_OPEN_COMPACT,
    TEXT_TOOL_CALL_TAG, TEXT_TOOL_CALL_TAG_COMPACT,
};

#[cfg(test)]
mod tests;
