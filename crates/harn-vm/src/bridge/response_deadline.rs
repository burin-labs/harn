use std::time::Duration;

pub(super) fn bridge_call_timeout(
    method: &str,
    params: &serde_json::Value,
    ordinary: Duration,
) -> Option<Duration> {
    match method {
        crate::llm::acp_permission::METHOD_REQUEST_PERMISSION => None,
        "host/call"
            if params
                .get("name")
                .and_then(serde_json::Value::as_str)
                .and_then(|name| name.split_once('.'))
                .is_some_and(|operation| {
                    operation == crate::orchestration::CONSENT_CAPABILITY
                        || matches!(
                            operation,
                            ("interaction", "ask" | "ask_user" | "request_approval")
                        )
                }) =>
        {
            None
        }
        _ => Some(ordinary),
    }
}

/// Wait for an ordinary host deadline. Human input and approvals instead wait
/// for their response, cancellation, or transport disconnection.
pub async fn wait_for_bridge_call_timeout(
    method: &str,
    params: &serde_json::Value,
    ordinary: Duration,
) -> Duration {
    if let Some(timeout) = bridge_call_timeout(method, params, ordinary) {
        tokio::time::sleep(timeout).await;
        timeout
    } else {
        std::future::pending::<Duration>().await
    }
}
