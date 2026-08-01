//! Harness-owned HTTP fixture dispatch.
//!
//! Typed harnesses can cross executor threads, so this path carries their
//! fixture registry explicitly instead of relying on the legacy ambient
//! thread-local registry.

use crate::value::{VmError, VmValue};

pub(in crate::http) async fn vm_execute_http_request_with_mocks(
    mocks: &crate::http::HttpMockRegistry,
    method: &str,
    url: &str,
    options: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    if let Some(session_id) = super::session_from_options(options) {
        return execute_session_request(mocks, &session_id, method, url, options).await;
    }

    let config = super::parse_http_options(options);
    let client = super::pooled_http_client(&config)?;
    super::vm_execute_http_request_with_client(Some(mocks), client, &config, method, url, options)
        .await
}

pub(super) fn consume_http_mock(
    harness_mocks: Option<&crate::http::HttpMockRegistry>,
    method: &str,
    final_url: &str,
    parts: &super::HttpRequestParts,
) -> Option<crate::http::mock::MockResponse> {
    harness_mocks
        .and_then(|mocks| {
            mocks.consume(
                method,
                final_url,
                parts.recorded_headers.clone(),
                parts.body.clone(),
            )
        })
        .or_else(|| {
            crate::http::mock::consume_http_mock(
                method,
                final_url,
                parts.recorded_headers.clone(),
                parts.body.clone(),
            )
        })
}

async fn execute_session_request(
    mocks: &crate::http::HttpMockRegistry,
    session_id: &str,
    method: &str,
    url: &str,
    options: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let session = super::HTTP_SESSIONS.with(|sessions| sessions.borrow().get(session_id).cloned());
    let Some(session) = session else {
        return Err(super::vm_error(format!(
            "http_session_request: unknown HTTP session '{session_id}'"
        )));
    };
    let merged_options = super::merge_options(&session.options, options);
    let config = super::parse_http_options(&merged_options);
    super::vm_execute_http_request_with_client(
        Some(mocks),
        session.client,
        &config,
        method,
        url,
        &merged_options,
    )
    .await
}
