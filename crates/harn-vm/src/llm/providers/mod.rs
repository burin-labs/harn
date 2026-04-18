//! Built-in LLM provider implementations.
//!
//! Each provider implements the `LlmProvider` + `LlmProviderOps` traits from
//! `super::provider`. The three main families are:
//!
//! - **Anthropic** — Claude models via the Anthropic Messages API
//! - **OpenAI-compatible** — OpenAI, OpenRouter, Together, Groq, DeepSeek,
//!   Fireworks, HuggingFace, local vLLM/SGLang servers, etc.
//! - **Ollama** — local Ollama server with NDJSON streaming
//! - **Mock** — deterministic test responses without any network I/O

pub(crate) mod anthropic;
mod mock;
mod ollama;
pub(crate) mod openai_compat;

pub(crate) use anthropic::AnthropicProvider;
pub(crate) use mock::MockProvider;
pub(crate) use ollama::OllamaProvider;
pub(crate) use openai_compat::OpenAiCompatibleProvider;
