//! Typed provider-wire contract.
//!
//! A route resolves this value once, then uses the same value to build its
//! request, select its stream grammar, parse its terminal response, and
//! classify HTTP failures. Keeping those four decisions together prevents a
//! request from being lowered with one provider grammar and decoded with
//! another.

use crate::llm::capabilities::{LiveEndpointFamily, WireDialect};
use crate::value::VmError;

use super::{LlmErrorInfo, LlmRequestPayload, LlmResult};

/// Concrete stream grammar selected by a provider-wire contract.
///
/// Transport owns byte delivery and deadlines. This enum owns which grammar
/// interprets those bytes; it is deliberately not represented as independent
/// `is_anthropic` / `is_ollama` booleans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamProtocol {
    AnthropicSse,
    OpenAiSse,
    OllamaNdjson,
    GeminiJson,
    GeminiInteractionsSse,
}

/// The complete wire contract for one resolved route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DialectContract {
    wire: WireDialect,
    live_endpoint: Option<LiveEndpointFamily>,
}

impl DialectContract {
    fn provider_reports_stream_usage(provider: &str) -> bool {
        crate::llm_config::provider_config(provider)
            .is_some_and(|definition| definition.stream_usage_accounting == Some(true))
    }

    /// Resolve the contract from the same capability row that admitted the
    /// request. Gemini's endpoint family is part of the contract because its
    /// two live APIs use different request, event, and response envelopes.
    pub(crate) fn for_request(request: &LlmRequestPayload) -> Self {
        let caps = crate::llm::capabilities::lookup(&request.provider, &request.model);
        Self::new(caps.message_wire_format, caps.live_endpoint_family)
    }

    pub(crate) fn new(wire: WireDialect, live_endpoint: Option<LiveEndpointFamily>) -> Self {
        let live_endpoint = match wire {
            WireDialect::Gemini => {
                Some(live_endpoint.unwrap_or(LiveEndpointFamily::GeminiGenerateContent))
            }
            _ => None,
        };
        Self {
            wire,
            live_endpoint,
        }
    }

    pub(crate) fn wire(self) -> WireDialect {
        self.wire
    }

    pub(crate) fn stream_protocol(self) -> StreamProtocol {
        match (self.wire, self.live_endpoint) {
            (WireDialect::Anthropic, _) => StreamProtocol::AnthropicSse,
            (WireDialect::OpenAiCompat, _) => StreamProtocol::OpenAiSse,
            (WireDialect::Ollama, _) => StreamProtocol::OllamaNdjson,
            (WireDialect::Gemini, Some(LiveEndpointFamily::GeminiInteractions)) => {
                StreamProtocol::GeminiInteractionsSse
            }
            (WireDialect::Gemini, _) => StreamProtocol::GeminiJson,
        }
    }

    /// Lower a neutral request into the exact provider body selected by this
    /// contract. The provider modules retain syntax-heavy JSON mechanics; no
    /// provider or transport call site chooses a builder independently.
    pub(crate) fn build_request_body(self, request: &LlmRequestPayload) -> serde_json::Value {
        match self.stream_protocol() {
            StreamProtocol::AnthropicSse => {
                crate::llm::providers::AnthropicProvider::build_request_body(request)
            }
            StreamProtocol::OpenAiSse => {
                crate::llm::providers::OpenAiCompatibleProvider::build_request_body(request)
            }
            StreamProtocol::OllamaNdjson => {
                crate::llm::providers::OllamaProvider::build_request_body(request)
            }
            StreamProtocol::GeminiJson => {
                crate::llm::providers::GeminiProvider::build_request_body(request)
            }
            StreamProtocol::GeminiInteractionsSse => {
                crate::llm::providers::GeminiInteractions::build_request_body(request)
            }
        }
    }

    /// Parse a complete provider envelope under the same contract that built
    /// the request. Streaming assemblers also converge here once they have a
    /// complete envelope.
    pub(crate) fn parse_response(
        self,
        json: &serde_json::Value,
        request: &LlmRequestPayload,
        tools_offered: bool,
    ) -> Result<LlmResult, VmError> {
        match self.stream_protocol() {
            StreamProtocol::GeminiJson => {
                crate::llm::providers::GeminiProvider::parse_response(json, request)
            }
            StreamProtocol::GeminiInteractionsSse => {
                crate::llm::providers::GeminiInteractions::parse_response(json, request)
            }
            StreamProtocol::AnthropicSse
            | StreamProtocol::OpenAiSse
            | StreamProtocol::OllamaNdjson => super::response::parse_llm_response(
                json,
                &request.provider,
                &request.model,
                self.wire,
                tools_offered,
            ),
        }
    }

    /// Classify a provider HTTP failure without allowing transport code to
    /// select a second, potentially inconsistent provider family.
    pub(crate) fn classify_http_error(
        self,
        provider: &str,
        status: reqwest::StatusCode,
        retry_after: Option<&str>,
        body: &str,
    ) -> LlmErrorInfo {
        let error_owner = match self.stream_protocol() {
            StreamProtocol::AnthropicSse => "anthropic",
            StreamProtocol::OllamaNdjson => "ollama",
            StreamProtocol::GeminiJson | StreamProtocol::GeminiInteractionsSse => "gemini",
            StreamProtocol::OpenAiSse => provider,
        };
        super::errors::classify_provider_http_error(error_owner, status, retry_after, body)
    }

    pub(crate) fn requests_stream_usage(self, provider: &str, endpoint: &str) -> bool {
        match self.stream_protocol() {
            StreamProtocol::AnthropicSse => false,
            StreamProtocol::OllamaNdjson => endpoint.contains("/v1/"),
            StreamProtocol::OpenAiSse => Self::provider_reports_stream_usage(provider),
            StreamProtocol::GeminiJson | StreamProtocol::GeminiInteractionsSse => false,
        }
    }

    /// Whether a finish-reason frame is only content-terminal and the parser
    /// must continue through the provider's trailing accounting frame.
    pub(crate) fn awaits_stream_usage(self, provider: &str) -> bool {
        self.stream_protocol() == StreamProtocol::OpenAiSse
            && Self::provider_reports_stream_usage(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmCallOptions, LlmRequestPayload};

    #[derive(serde::Deserialize)]
    struct GoldenRequest {
        messages: Vec<serde_json::Value>,
        max_tokens: i64,
    }

    #[derive(serde::Deserialize)]
    struct GoldenResult {
        text: String,
        input_tokens: i64,
        output_tokens: i64,
        stop_reason: String,
    }

    #[derive(serde::Deserialize)]
    struct GoldenError {
        status: u16,
        retry_after: Option<String>,
        body: String,
        kind: String,
        reason: String,
        message_contains: String,
    }

    #[derive(serde::Deserialize)]
    struct DialectGolden {
        provider: String,
        model: String,
        wire: String,
        #[serde(default)]
        live_endpoint: Option<String>,
        stream_protocol: String,
        request: GoldenRequest,
        wire_request: serde_json::Value,
        wire_response: serde_json::Value,
        result: GoldenResult,
        error: GoldenError,
    }

    fn protocol_name(protocol: StreamProtocol) -> &'static str {
        match protocol {
            StreamProtocol::AnthropicSse => "anthropic_sse",
            StreamProtocol::OpenAiSse => "openai_sse",
            StreamProtocol::OllamaNdjson => "ollama_ndjson",
            StreamProtocol::GeminiJson => "gemini_json",
            StreamProtocol::GeminiInteractionsSse => "gemini_interactions_sse",
        }
    }

    fn assert_golden(source: &str) {
        let golden: DialectGolden = serde_json::from_str(source).expect("valid dialect golden");
        let options = LlmCallOptions {
            provider: golden.provider.clone(),
            model: golden.model.clone(),
            messages: golden.request.messages,
            max_tokens: golden.request.max_tokens,
            ..LlmCallOptions::default()
        };
        let request = LlmRequestPayload::from(&options);
        let dialect = match golden.live_endpoint.as_deref() {
            Some("gemini_interactions") => DialectContract::new(
                WireDialect::Gemini,
                Some(LiveEndpointFamily::GeminiInteractions),
            ),
            Some(other) => panic!("unknown golden live endpoint: {other}"),
            None => DialectContract::for_request(&request),
        };

        assert_eq!(dialect.wire().as_str(), golden.wire);
        assert_eq!(
            protocol_name(dialect.stream_protocol()),
            golden.stream_protocol
        );

        let actual_request = dialect.build_request_body(&request);
        assert_eq!(actual_request, golden.wire_request);
        assert_eq!(
            serde_json::to_vec(&actual_request).expect("request bytes"),
            serde_json::to_vec(&golden.wire_request).expect("golden request bytes"),
            "provider request bytes drifted"
        );

        let result = dialect
            .parse_response(&golden.wire_response, &request, false)
            .expect("golden response parses");
        assert_eq!(result.text, golden.result.text);
        assert_eq!(result.input_tokens, golden.result.input_tokens);
        assert_eq!(result.output_tokens, golden.result.output_tokens);
        assert_eq!(
            result.stop_reason.as_deref(),
            Some(golden.result.stop_reason.as_str())
        );

        let error = dialect.classify_http_error(
            &golden.provider,
            reqwest::StatusCode::from_u16(golden.error.status).expect("valid status"),
            golden.error.retry_after.as_deref(),
            &golden.error.body,
        );
        assert_eq!(error.kind.as_str(), golden.error.kind);
        assert_eq!(error.reason.as_str(), golden.error.reason);
        assert!(error.message.contains(&golden.error.message_contains));
    }

    #[test]
    fn openai_request_response_and_error_match_golden() {
        assert_golden(include_str!("../testdata/dialects/openai_compat.json"));
    }

    #[test]
    fn anthropic_request_response_and_error_match_golden() {
        assert_golden(include_str!("../testdata/dialects/anthropic.json"));
    }

    #[test]
    fn gemini_request_response_and_error_match_golden() {
        assert_golden(include_str!("../testdata/dialects/gemini.json"));
    }

    #[test]
    fn gemini_interactions_request_response_and_error_match_golden() {
        assert_golden(include_str!(
            "../testdata/dialects/gemini_interactions.json"
        ));
    }

    /// burin-code#6388. Fireworks ends a streamed completion with a trailing
    /// accounting frame after the finish-reason frame. Without this catalog
    /// fact the parser treats finish-reason as terminal and drops the usage,
    /// so every streamed agent call prices as `unknown` while non-streamed
    /// structured calls on the same route price correctly.
    #[test]
    fn fireworks_streams_a_trailing_usage_frame() {
        assert!(DialectContract::provider_reports_stream_usage("fireworks"));
    }

    #[test]
    fn llamacpp_requests_its_trailing_usage_frame() {
        assert!(DialectContract::provider_reports_stream_usage("llamacpp"));
    }

    #[test]
    fn nvidia_requests_and_awaits_its_trailing_usage_frame() {
        let contract = DialectContract::new(WireDialect::OpenAiCompat, None);
        assert!(contract.requests_stream_usage("nvidia", "/chat/completions"));
        assert!(contract.awaits_stream_usage("nvidia"));
    }
}
