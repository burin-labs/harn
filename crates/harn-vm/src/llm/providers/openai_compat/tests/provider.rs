//! Provider-level surface: model-generation gating, tool-search variant
//! advertisement, and HTTP error classification.

use crate::llm::api::{LlmErrorKind, LlmErrorReason};
use crate::llm::provider::LlmProvider;
use crate::llm::providers::openai_compat::{
    gpt_generation, gpt_model_supports_tool_search, OpenAiCompatibleProvider,
};

#[test]
fn tool_search_supported_for_gpt_5_4_and_up() {
    assert!(gpt_model_supports_tool_search("gpt-5.4"));
    assert!(gpt_model_supports_tool_search("gpt-5.4-preview"));
    assert!(gpt_model_supports_tool_search("gpt-5.4-turbo"));
    assert!(gpt_model_supports_tool_search("gpt-5-4"));
    assert!(gpt_model_supports_tool_search("gpt-5.5"));
    assert!(gpt_model_supports_tool_search("gpt-6.0"));
}

#[test]
fn tool_search_unsupported_for_pre_5_4() {
    assert!(!gpt_model_supports_tool_search("gpt-4o"));
    assert!(!gpt_model_supports_tool_search("gpt-4.1"));
    assert!(!gpt_model_supports_tool_search("gpt-4-turbo"));
    assert!(!gpt_model_supports_tool_search("gpt-3.5-turbo"));
    assert!(!gpt_model_supports_tool_search("gpt-5.0"));
    assert!(!gpt_model_supports_tool_search("gpt-5.3-preview"));
    assert!(!gpt_model_supports_tool_search("gpt-5"));
}

#[test]
fn tool_search_unsupported_for_non_gpt() {
    assert!(!gpt_model_supports_tool_search("claude-opus-4-7"));
    assert!(!gpt_model_supports_tool_search("llama-3.1-70b"));
    assert!(!gpt_model_supports_tool_search(""));
}

#[test]
fn gpt_generation_handles_openrouter_prefix() {
    // OpenRouter model IDs carry an `openai/` prefix. Same capability
    // check must produce the same answer.
    assert_eq!(gpt_generation("openai/gpt-5.4-preview"), Some((5, 4)));
    assert_eq!(gpt_generation("azure/gpt-5.5-turbo"), Some((5, 5)));
    assert!(gpt_model_supports_tool_search("openai/gpt-5.4"));
    assert!(!gpt_model_supports_tool_search("openai/gpt-4o"));
}

#[test]
fn gpt_generation_ignores_date_suffix_as_minor() {
    // `gpt-5-20260115` should parse as generation (5, 0), not (5, 20260115).
    assert_eq!(gpt_generation("gpt-5-20260115"), Some((5, 0)));
    assert!(!gpt_model_supports_tool_search("gpt-5-20260115"));
}

#[test]
fn native_tool_search_variants_lists_hosted_first() {
    let provider = OpenAiCompatibleProvider::new("openai".to_string());
    let variants = provider.native_tool_search_variants("gpt-5.4-preview");
    assert_eq!(variants, vec!["hosted".to_string(), "client".to_string()]);
}

#[test]
fn native_tool_search_variants_empty_for_old_model() {
    let provider = OpenAiCompatibleProvider::new("openai".to_string());
    assert!(provider.native_tool_search_variants("gpt-4o").is_empty());
}

#[test]
fn classifies_openai_context_length_as_terminal_context_overflow() {
    let info = OpenAiCompatibleProvider::classify_http_error(
        "openai",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ContextOverflow);
}

#[test]
fn classifies_openai_rate_limit_as_transient_rate_limit() {
    let info = OpenAiCompatibleProvider::classify_http_error(
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        Some("5"),
        r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::RateLimit);
    assert!(info.message.contains("retry-after: 5"));
}

#[test]
fn supports_defer_loading_matches_tool_search_gate() {
    let provider = OpenAiCompatibleProvider::new("openai".to_string());
    assert!(provider.supports_defer_loading("gpt-5.4"));
    assert!(!provider.supports_defer_loading("gpt-4o"));
}
