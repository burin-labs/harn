mod collect;
mod components;
mod contract_prompt;
mod handle_local;
mod json_schema;
mod messages;
mod native;
mod params;
mod parse;
mod ts_value_parser;
mod type_expr;

#[cfg(test)]
pub(crate) use collect::collect_tool_schemas_with_registry;
pub(crate) use collect::{collect_tool_schemas, validate_tool_args, ToolSchema};
#[cfg(test)]
pub(crate) use components::ComponentRegistry;
pub(crate) use contract_prompt::build_tool_calling_contract_prompt;
#[cfg(test)]
pub(crate) use contract_prompt::TEXT_RESPONSE_PROTOCOL_HELP;
pub(crate) use handle_local::handle_tool_locally;
#[cfg(test)]
pub(crate) use json_schema::json_schema_to_type_expr;
pub(crate) use messages::{
    build_assistant_response_message, build_assistant_tool_message, build_tool_result_message,
    normalize_tool_args,
};
#[cfg(test)]
pub(crate) use native::apply_tool_search_native_injection;
pub(crate) use native::{
    apply_tool_search_client_injection, apply_tool_search_native_injection_typed,
    build_client_search_tool_schema, build_load_skill_tool_schema, extract_deferred_tool_names,
    vm_tools_to_native,
};
pub(crate) use parse::parse_text_tool_calls_with_tools;
pub(crate) use parse::StreamingToolCallDetector;
#[cfg(test)]
pub(crate) use parse::{parse_bare_calls_in_body, parse_native_json_tool_calls};

#[cfg(test)]
mod tests;
