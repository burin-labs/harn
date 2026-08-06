//! Classification of OpenAI Responses `output[]` item types.
//!
//! Two questions the ingestion loop asks about an item before deciding what to
//! do with it: whether the provider executed it on its own behalf, and if so
//! what family of tool ran. Kept together and out of `response.rs` because the
//! answer is pure string classification with no dependency on parse state.

/// The tool family a hosted-tool item belongs to, as recorded on the block.
pub(super) fn openai_responses_tool_kind(item_type: &str) -> &'static str {
    match item_type {
        "web_search_call" => "web_search",
        "file_search_call" => "file_search",
        "code_interpreter_call" => "code_interpreter",
        "computer_call" => "computer_use",
        "image_generation_call" => "image_generation",
        "tool_search_call" | "tool_search_output" => "tool_search",
        _ if item_type.starts_with("mcp_") => "remote_mcp",
        _ => "hosted_tool",
    }
}

/// Whether an item is one the provider ran for us, rather than a message or a
/// tool call the model is asking the harness to execute.
pub(super) fn is_openai_responses_hosted_tool_item(item_type: &str) -> bool {
    item_type.ends_with("_call")
        || item_type == "tool_search_output"
        || item_type == "mcp_list_tools"
        || item_type == "mcp_approval_request"
}
