use super::*;

pub(super) async fn collect_task_stream_until_terminal(
    mut rx: UnboundedReceiver<JsonValue>,
) -> Vec<JsonValue> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for stream event: {}",
                    events_json(&events)
                )
            });
        let Some(event) = event else {
            break;
        };
        let terminal = event
            .pointer("/result/status/state")
            .and_then(JsonValue::as_str)
            .is_some_and(is_terminal_status);
        events.push(event);
        if terminal {
            break;
        }
    }
    events
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "rejected")
}

pub(super) fn events_json(events: &[JsonValue]) -> String {
    serde_json::to_string_pretty(events).unwrap_or_else(|_| "<unprintable events>".to_string())
}

pub(super) fn is_progress_status_update(event: &JsonValue) -> bool {
    event.pointer("/result/kind").and_then(JsonValue::as_str) == Some("status-update")
}
