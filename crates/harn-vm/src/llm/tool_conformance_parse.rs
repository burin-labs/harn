use crate::llm::tools::TextToolParseResult;
use crate::value::VmValue;

pub(super) async fn parse_text_tool_formats(
    content: &str,
    tools: &VmValue,
) -> Result<(TextToolParseResult, TextToolParseResult), String> {
    let tagged = crate::llm::api::parse_text_tools_with_harn(None, content, Some(tools), "")
        .await
        .map_err(|error| format!("text-tool parser failed: {error}"))?;
    let fenced = crate::llm::api::parse_text_tools_with_harn(None, content, Some(tools), "json")
        .await
        .map_err(|error| format!("JSON text-tool parser failed: {error}"))?;
    Ok((tagged, fenced))
}
