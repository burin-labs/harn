use super::sse::consume_sse_lines;
use crate::llm::api::{DialectContract, LlmResult};
use crate::llm::capabilities::WireDialect;
use crate::value::VmError;

const CONTENT: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"positive\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
);

async fn response(body: &str, provider: &str) -> Result<LlmResult, VmError> {
    response_reader(tokio::io::BufReader::new(body.as_bytes()), provider).await
}

async fn response_reader<R: tokio::io::AsyncBufRead + Unpin>(
    reader: R,
    provider: &str,
) -> Result<LlmResult, VmError> {
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    consume_sse_lines(
        reader,
        provider,
        "accounting-fixture",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        sender,
        None,
        None,
        false,
    )
    .await
}

#[tokio::test]
async fn unknown_route_retains_actual_trailing_token_and_cache_counts() {
    let provider = crate::llm_config::provider_config("local").expect("known local route");
    assert_eq!(provider.stream_usage_accounting, None);
    let body = format!(
        "{CONTENT}data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":37,\"completion_tokens\":11,\"total_tokens\":48,\"prompt_tokens_details\":{{\"cached_tokens\":7}}}}}}\n\ndata: [DONE]\n\n"
    );
    let result = response(&body, "local").await.expect("reported receipt");
    assert_eq!(result.text, "positive");
    assert_eq!((result.input_tokens, result.output_tokens), (37, 11));
    assert_eq!(result.cache_read_tokens, 7);
    assert_eq!(result.telemetry.server_total_tokens, Some(48));
    assert_eq!(result.usage().usage_unknown_calls, 0);
}

#[tokio::test]
async fn absent_receipt_at_done_or_eof_stays_unknown() {
    for suffix in [
        "data: [DONE]\n\n",
        "",
        "data: {\"choices\":[],\"usage\":{}}\n\ndata: [DONE]\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":null,\"completion_tokens\":null}}\n\ndata: [DONE]\n\n",
    ] {
        let result = response(&format!("{CONTENT}{suffix}"), "local")
            .await
            .expect("completed content without accounting");
        assert_eq!(result.text, "positive");
        assert_eq!(result.stop_reason.as_deref(), Some("stop"));
        assert_eq!(result.telemetry.server_prompt_tokens, None);
        assert_eq!(result.telemetry.server_output_tokens, None);
        assert_eq!(result.telemetry.server_total_tokens, None);
        assert_eq!(result.usage().usage_unknown_calls, 1);
    }
}

#[tokio::test]
async fn explicit_zero_receipt_is_reported_rather_than_absent() {
    let body = format!(
        "{CONTENT}data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"prompt_tokens_details\":{{\"cached_tokens\":0}}}}}}\n\n"
    );
    let result = response(&body, "local")
        .await
        .expect("observed zero receipt");
    assert_eq!(result.text, "positive");
    assert_eq!(result.telemetry.server_prompt_tokens, Some(0));
    assert_eq!(result.telemetry.server_output_tokens, Some(0));
    assert_eq!(result.telemetry.server_total_tokens, Some(0));
    assert_eq!(result.usage().usage_unknown_calls, 0);
    assert!(result.usage().cache_counts_present);
}

#[tokio::test]
async fn read_failure_after_content_is_not_a_complete_accounting_observation() {
    let error = response_reader(erroring_reader(), "local")
        .await
        .expect_err("an interrupted receipt must not silently complete");
    assert!(
        error
            .provider_stream_failure()
            .expect("typed read failure")
            .partial
    );
    assert!(crate::llm::agent_observe::is_retryable_llm_error(&error));
}

fn erroring_reader() -> impl tokio::io::AsyncBufRead + Unpin {
    let chunks = vec![
        Ok(CONTENT.as_bytes()),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "fixture reset",
        )),
    ];
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(tokio_stream::iter(
        chunks,
    )))
}

#[tokio::test]
async fn a_total_only_receipt_does_not_invent_prompt_or_output_counts() {
    let body = format!("{CONTENT}data: {{\"choices\":[],\"usage\":{{\"total_tokens\":48}}}}\n\n");
    let result = response(&body, "local").await.expect("measured total");
    assert_eq!(result.text, "positive");
    assert_eq!(result.telemetry.server_prompt_tokens, None);
    assert_eq!(result.telemetry.server_output_tokens, None);
    assert_eq!(result.telemetry.server_total_tokens, Some(48));
    assert_eq!(result.usage().reported_total_tokens, Some(48));
}

#[test]
fn an_explicit_nonaccounting_route_keeps_its_content_terminal() {
    let _guard = crate::llm::env_guard();
    let provider = "explicit-no-stream-accounting";
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.into(),
        crate::llm_config::ProviderDef {
            stream_usage_accounting: Some(false),
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime.block_on(response_reader(erroring_reader(), provider));
    crate::llm_config::clear_user_overrides();
    assert_eq!(result.expect("declared content terminal").text, "positive");
}
