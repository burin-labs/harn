use crate::triggers::TriggerEvent;

pub(super) fn event_selector_matches(event: &TriggerEvent, expected: &str) -> bool {
    if expected == event.kind {
        return true;
    }
    let selector = expected
        .strip_prefix(event.provider.as_str())
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(expected);
    if selector == event.kind {
        return true;
    }
    let Some(expected_action) = selector
        .strip_prefix(event.kind.as_str())
        .and_then(|rest| rest.strip_prefix('.'))
        .filter(|action| !action.is_empty())
    else {
        return false;
    };
    event.provider_payload.action() == Some(expected_action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triggers::event::KnownProviderPayload;
    use crate::triggers::{ProviderId, ProviderPayload, SignatureStatus};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn event_selectors_match_normalized_kind_and_action() {
        let mut event = TriggerEvent::new(
            ProviderId::from("github"),
            "pull_request",
            None,
            "selector-test",
            None,
            BTreeMap::new(),
            ProviderPayload::extension(
                "github",
                "GitHubEventPayload",
                json!({"action": "opened", "topic": "github.pull_request.opened"}),
            ),
            SignatureStatus::Verified,
        );
        for selector in [
            "pull_request",
            "github.pull_request",
            "pull_request.opened",
            "github.pull_request.opened",
        ] {
            assert!(event_selector_matches(&event, selector), "{selector}");
        }
        for selector in [
            "pull_request.closed",
            "github.pull_request.closed",
            "gitlab.pull_request.opened",
            "pull_request.opened.extra",
            "pull_request.",
            "pull_requestx.opened",
            "github.pr_opened",
        ] {
            assert!(!event_selector_matches(&event, selector), "{selector}");
        }
        for payload in [
            json!({"action": null}),
            json!({"action": 7}),
            json!({"action": ""}),
            json!({"topic": "github.pull_request.opened"}),
            json!({"raw": {"action": "opened"}}),
        ] {
            event.provider_payload =
                ProviderPayload::extension("github", "GitHubEventPayload", payload);
            assert!(event_selector_matches(&event, "github.pull_request"));
            assert!(!event_selector_matches(
                &event,
                "github.pull_request.opened"
            ));
        }
        event.kind = "message.channels".to_string();
        event.provider = ProviderId::from("slack");
        assert!(event_selector_matches(&event, "message.channels"));
        assert!(event_selector_matches(&event, "slack.message.channels"));
        assert!(!event_selector_matches(&event, "message"));
    }

    #[test]
    fn generic_webhook_selectors_use_the_payload_action() {
        let event = TriggerEvent::new(
            ProviderId::from("webhook"),
            "issues",
            None,
            "generic-selector-test",
            None,
            BTreeMap::new(),
            ProviderPayload::Known(KnownProviderPayload::Webhook(
                crate::triggers::GenericWebhookPayload {
                    source: None,
                    content_type: None,
                    raw: json!({"action": "opened"}),
                },
            )),
            SignatureStatus::Unsigned,
        );
        assert!(event_selector_matches(&event, "webhook.issues.opened"));
        assert!(!event_selector_matches(&event, "webhook.issues.closed"));
    }

    #[test]
    fn stream_selectors_use_the_normalized_payload_action() {
        let provider = ProviderId::from("kafka");
        let payload = ProviderPayload::normalize(
            &provider,
            "order",
            &BTreeMap::new(),
            json!({"kind": "order", "action": "created"}),
        )
        .unwrap();
        assert!(matches!(
            &payload,
            ProviderPayload::Known(KnownProviderPayload::Kafka(_))
        ));
        let event = TriggerEvent::new(
            provider,
            "order",
            None,
            "stream-selector-test",
            None,
            BTreeMap::new(),
            payload,
            SignatureStatus::Unsigned,
        );
        for selector in [
            "order",
            "kafka.order",
            "order.created",
            "kafka.order.created",
        ] {
            assert!(event_selector_matches(&event, selector), "{selector}");
        }
        assert!(!event_selector_matches(&event, "kafka.order.deleted"));
    }
}
