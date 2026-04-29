//! Mock LLM provider — deterministic responses for testing without API keys.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::mock::mock_llm_response;
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

/// Zero-cost unit struct for the mock provider.
pub(crate) struct MockProvider;

impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn is_mock(&self) -> bool {
        true
    }

    fn requires_model(&self) -> bool {
        false
    }
}

impl LlmProviderChat for MockProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl MockProvider {
    pub(crate) fn should_intercept(provider: &str) -> bool {
        provider == "mock" || crate::llm::mock::cli_llm_mock_replay_active()
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        _delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        mock_llm_response(
            &request.messages,
            request.system.as_deref(),
            request.native_tools.as_deref(),
            &request.thinking,
        )
    }
}
