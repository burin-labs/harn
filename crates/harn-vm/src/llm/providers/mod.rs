//! Built-in LLM provider implementations.
//!
//! Each provider implements the `LlmProvider` + `LlmProviderOps` traits from
//! `super::provider`. The three main families are:
//!
//! - **Anthropic** — Claude models via the Anthropic Messages API
//! - **OpenAI-compatible** — OpenAI, OpenRouter, Together, Groq, DeepSeek,
//!   Fireworks, HuggingFace, local vLLM/SGLang servers, etc.
//! - **Ollama** — local Ollama server with NDJSON streaming
//! - **Bedrock / Azure OpenAI / Vertex AI** — enterprise cloud shims
//! - **Mock** — deterministic test responses without any network I/O

mod acp;
pub(crate) mod anthropic;
pub(crate) mod azure_openai;
pub(crate) mod bedrock;
mod common;
mod gemini;
mod mock;
mod ollama;
pub(crate) mod openai_compat;
pub(crate) mod openai_responses;
mod schema_compat;
pub(crate) mod vertex;

pub(crate) use acp::AcpProvider;
pub(crate) use anthropic::AnthropicProvider;
pub(crate) use azure_openai::AzureOpenAiProvider;
pub(crate) use bedrock::BedrockProvider;
pub(crate) use gemini::GeminiProvider;
// Vertex delegates response parsing to the canonical Gemini parser (see
// `gemini::parse_response`), mirroring its request-building delegation.
pub(crate) use gemini::parse_response as parse_gemini_response;
pub(crate) use mock::MockProvider;
pub(crate) use ollama::OllamaProvider;
pub(crate) use openai_compat::OpenAiCompatibleProvider;
pub(crate) use openai_responses::OpenAiResponsesProvider;
pub(crate) use vertex::VertexProvider;

/// Deterministic in-process providers used by tests and replay. They are not
/// network routes and must not participate in provider-health recovery.
pub(crate) fn is_internal_simulator(provider: &str) -> bool {
    MockProvider::should_intercept(provider)
        || super::fake::FakeLlmProvider::should_intercept(provider)
}
