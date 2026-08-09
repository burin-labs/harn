use crate::llm::api::LlmResult;
use crate::llm::capabilities::WireDialect;
use crate::value::VmError;

pub(super) fn parse_llm_response(
    json: &serde_json::Value,
    provider: &str,
    model: &str,
    is_anthropic: bool,
    tools_offered: bool,
) -> Result<LlmResult, VmError> {
    let dialect = if is_anthropic {
        WireDialect::Anthropic
    } else {
        WireDialect::OpenAiCompat
    };
    super::parse_llm_response(json, provider, model, dialect, tools_offered)
}
