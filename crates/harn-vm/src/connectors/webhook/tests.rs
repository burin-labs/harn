use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;

use super::{GenericWebhookConnector, WebhookProviderProfile, WebhookSignatureVariant};
use crate::connectors::{
    Connector, ConnectorCtx, ConnectorError, ConnectorRegistry, InboxIndex, MetricsRegistry,
    RateLimiterFactory, RawInbound, TriggerBinding,
};
use crate::event_log::{AnyEventLog, EventLog, MemoryEventLog, Topic};
use crate::secrets::{SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider};
use crate::triggers::{ProviderId, SignatureStatus};

struct StaticSecretProvider {
    namespace: String,
    secrets: BTreeMap<SecretId, String>,
}

impl StaticSecretProvider {
    fn new(namespace: &str, secrets: BTreeMap<SecretId, String>) -> Self {
        Self {
            namespace: namespace.to_string(),
            secrets,
        }
    }
}

#[async_trait]
impl SecretProvider for StaticSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.secrets
            .get(id)
            .cloned()
            .map(SecretBytes::from)
            .ok_or_else(|| SecretError::NotFound {
                provider: self.namespace.clone(),
                id: id.clone(),
            })
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "put",
        })
    }

    async fn rotate(&self, _id: &SecretId) -> Result<crate::secrets::RotationHandle, SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "rotate",
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

#[derive(Clone)]
struct TestHarness {
    log: Arc<AnyEventLog>,
    connector: Arc<GenericWebhookConnector>,
}

impl TestHarness {
    async fn new(binding: TriggerBinding, secret: &str) -> Self {
        Self::with_connector(binding, "webhook", secret, GenericWebhookConnector::new()).await
    }

    async fn with_connector(
        binding: TriggerBinding,
        namespace: &str,
        secret: &str,
        mut connector: GenericWebhookConnector,
    ) -> Self {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let secrets = Arc::new(StaticSecretProvider::new(
            namespace,
            BTreeMap::from([(
                SecretId::new(namespace, "test-signing-secret"),
                secret.to_string(),
            )]),
        ));
        let metrics = Arc::new(MetricsRegistry::default());
        let inbox = Arc::new(
            InboxIndex::new(log.clone(), metrics.clone())
                .await
                .expect("inbox init"),
        );
        let ctx = ConnectorCtx {
            event_log: log.clone(),
            secrets,
            inbox,
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::default()),
        };

        connector.init(ctx).await.unwrap();
        connector.activate(&[binding]).await.unwrap();

        Self {
            log,
            connector: Arc::new(connector),
        }
    }

    async fn audit_events(&self) -> Vec<(u64, crate::event_log::LogEvent)> {
        self.log
            .read_range(
                &Topic::new(crate::connectors::SIGNATURE_VERIFY_AUDIT_TOPIC).unwrap(),
                None,
                32,
            )
            .await
            .unwrap()
    }
}

#[derive(Clone)]
struct Case {
    name: &'static str,
    variant: WebhookSignatureVariant,
    secret: &'static str,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    received_at: OffsetDateTime,
    expect_ok: bool,
}

fn binding(variant: WebhookSignatureVariant, dedupe_key: Option<&str>) -> TriggerBinding {
    let mut binding = TriggerBinding::new(ProviderId::from("webhook"), "webhook", "webhook.test");
    binding.dedupe_key = dedupe_key.map(ToString::to_string);
    binding.config = json!({
        "match": { "path": "/hooks/test" },
        "secrets": { "signing_secret": "webhook/test-signing-secret" },
        "webhook": {
            "signature_scheme": match variant {
                WebhookSignatureVariant::Standard => "standard",
                WebhookSignatureVariant::Stripe => "stripe",
                WebhookSignatureVariant::GitHub => "github",
            },
            "source": "fixtures",
        }
    });
    binding
}

fn raw_inbound(
    headers: BTreeMap<String, String>,
    body: &[u8],
    received_at: OffsetDateTime,
) -> RawInbound {
    let mut raw = RawInbound::new("", headers, body.to_vec());
    raw.received_at = received_at;
    raw
}

#[tokio::test]
async fn webhook_variants_cover_valid_and_failure_cases() {
    // Standard Webhooks vector from the public reference fixtures / spec examples:
    // https://github.com/standard-webhooks/standard-webhooks/tree/main/libraries
    // https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md
    let standard_valid = Case {
        name: "standard_valid",
        variant: WebhookSignatureVariant::Standard,
        secret: "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
        headers: BTreeMap::from([
            (
                "webhook-id".to_string(),
                "msg_p5jXN8AQM9LWM0D4loKWxJek".to_string(),
            ),
            (
                "webhook-signature".to_string(),
                "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=".to_string(),
            ),
            ("webhook-timestamp".to_string(), "1614265330".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        body: br#"{"test": 2432232314}"#.to_vec(),
        received_at: OffsetDateTime::from_unix_timestamp(1_614_265_330).unwrap(),
        expect_ok: true,
    };
    let standard_tampered = Case {
        name: "standard_tampered_body",
        body: br#"{"test": 2432232315}"#.to_vec(),
        expect_ok: false,
        ..standard_valid.clone()
    };
    let standard_bad_timestamp = Case {
        name: "standard_bad_timestamp",
        received_at: OffsetDateTime::from_unix_timestamp(1_614_265_700).unwrap(),
        expect_ok: false,
        ..standard_valid.clone()
    };
    let standard_bad_sig = Case {
        name: "standard_bad_sig",
        headers: BTreeMap::from([
            (
                "webhook-id".to_string(),
                "msg_p5jXN8AQM9LWM0D4loKWxJek".to_string(),
            ),
            (
                "webhook-signature".to_string(),
                "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            ),
            ("webhook-timestamp".to_string(), "1614265330".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        expect_ok: false,
        ..standard_valid.clone()
    };

    // Stripe signature shape and webhook payload fixture based on the official docs:
    // https://docs.stripe.com/webhooks#signatures
    let stripe_valid = Case {
        name: "stripe_valid",
        variant: WebhookSignatureVariant::Stripe,
        secret: "whsec_test_secret",
        headers: BTreeMap::from([(
            "Stripe-Signature".to_string(),
            "t=12345,v1=2672d138c9a412830f3bfe2ecc5bfb3277cf6f5b49d0119d77dd6cb64da1257e"
                .to_string(),
        )]),
        body: b"{\n  \"id\": \"evt_test_webhook\",\n  \"object\": \"event\"\n}".to_vec(),
        received_at: OffsetDateTime::from_unix_timestamp(12_350).unwrap(),
        expect_ok: true,
    };
    let stripe_tampered = Case {
        name: "stripe_tampered_body",
        body: b"{\n  \"id\": \"evt_test_webhook_tampered\",\n  \"object\": \"event\"\n}".to_vec(),
        expect_ok: false,
        ..stripe_valid.clone()
    };
    let stripe_bad_timestamp = Case {
        name: "stripe_bad_timestamp",
        received_at: OffsetDateTime::from_unix_timestamp(13_000).unwrap(),
        expect_ok: false,
        ..stripe_valid.clone()
    };
    let stripe_bad_sig = Case {
        name: "stripe_bad_sig",
        headers: BTreeMap::from([(
            "Stripe-Signature".to_string(),
            "t=12345,v1=0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        )]),
        expect_ok: false,
        ..stripe_valid.clone()
    };

    // GitHub vector from the official validating-webhook-deliveries docs:
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
    let github_valid = Case {
        name: "github_valid",
        variant: WebhookSignatureVariant::GitHub,
        secret: "It's a Secret to Everybody",
        headers: BTreeMap::from([
            (
                "X-Hub-Signature-256".to_string(),
                "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
                    .to_string(),
            ),
            ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
            ("X-GitHub-Event".to_string(), "ping".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        body: b"Hello, World!".to_vec(),
        received_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        expect_ok: true,
    };
    let github_tampered = Case {
        name: "github_tampered_body",
        body: b"Hello, World?\n".to_vec(),
        expect_ok: false,
        ..github_valid.clone()
    };
    let github_bad_timestamp = Case {
        name: "github_bad_timestamp_ignored",
        // GitHub's webhook signature scheme is untimestamped, so skewed receive
        // times should not change verification behavior.
        received_at: OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
        expect_ok: true,
        ..github_valid.clone()
    };
    let github_bad_sig = Case {
        name: "github_bad_sig",
        headers: BTreeMap::from([
            (
                "X-Hub-Signature-256".to_string(),
                "sha256=0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            ),
            ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
            ("X-GitHub-Event".to_string(), "ping".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        expect_ok: false,
        ..github_valid.clone()
    };

    let cases = vec![
        standard_valid,
        standard_tampered,
        standard_bad_timestamp,
        standard_bad_sig,
        stripe_valid,
        stripe_tampered,
        stripe_bad_timestamp,
        stripe_bad_sig,
        github_valid,
        github_tampered,
        github_bad_timestamp,
        github_bad_sig,
    ];

    for case in cases {
        let harness = TestHarness::new(binding(case.variant, None), case.secret).await;
        let result = harness.connector.normalize_inbound(raw_inbound(
            case.headers.clone(),
            &case.body,
            case.received_at,
        ));
        let audit_events = harness.audit_events().await;

        if case.expect_ok {
            let event =
                result.unwrap_or_else(|error| panic!("{} should succeed: {error}", case.name));
            assert_eq!(event.provider.as_str(), "webhook", "{}", case.name);
            assert!(
                matches!(event.signature_status, SignatureStatus::Verified),
                "{}",
                case.name
            );
            assert_eq!(audit_events.len(), 0, "{}", case.name);
        } else {
            let error = result.unwrap_err();
            assert!(
                matches!(
                    error,
                    ConnectorError::InvalidSignature(_)
                        | ConnectorError::TimestampOutOfWindow { .. }
                        | ConnectorError::InvalidHeader { .. }
                ),
                "{} produced unexpected error: {error:?}",
                case.name
            );
            assert_eq!(audit_events.len(), 1, "{}", case.name);
        }
    }
}

// TODO(harn#223): webhook dedupe is temporarily disabled in this PR because
// `Connector::normalize_inbound` is a sync trait method and cannot await the
// now-async `InboxIndex::insert_if_new`. Re-enable this test once the async
// bridge lands (harn#223). Cron dedupe (the primary at-least-once use case)
// is already wired and covered by `orchestrator_inbox_dedupe.rs`.
#[ignore = "webhook dedupe disabled until harn#223 bridges sync trait to async InboxIndex"]
#[tokio::test]
async fn normalize_inbound_dedupes_on_binding_delivery_key() {
    let harness = TestHarness::new(
        binding(WebhookSignatureVariant::Standard, Some("event.dedupe_key")),
        "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
    )
    .await;
    let headers = BTreeMap::from([
        (
            "webhook-id".to_string(),
            "msg_p5jXN8AQM9LWM0D4loKWxJek".to_string(),
        ),
        (
            "webhook-signature".to_string(),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=".to_string(),
        ),
        ("webhook-timestamp".to_string(), "1614265330".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]);
    let raw = raw_inbound(
        headers,
        br#"{"test": 2432232314}"#,
        OffsetDateTime::from_unix_timestamp(1_614_265_330).unwrap(),
    );

    let first = harness.connector.normalize_inbound(raw.clone()).unwrap();
    assert_eq!(first.dedupe_key, "msg_p5jXN8AQM9LWM0D4loKWxJek");

    let duplicate = harness.connector.normalize_inbound(raw).unwrap_err();
    assert!(matches!(duplicate, ConnectorError::DuplicateDelivery(_)));
}

#[tokio::test]
async fn github_profile_normalizes_events_under_the_github_provider() {
    let mut binding = TriggerBinding::new(ProviderId::from("github"), "webhook", "github.test");
    binding.config = json!({
        "match": { "path": "/hooks/github" },
        "secrets": { "signing_secret": "github/test-signing-secret" },
        "webhook": {}
    });

    let harness = TestHarness::with_connector(
        binding,
        "github",
        "It's a Secret to Everybody",
        GenericWebhookConnector::with_profile(WebhookProviderProfile::new(
            ProviderId::from("github"),
            "GitHubEventPayload",
            WebhookSignatureVariant::GitHub,
        )),
    )
    .await;

    let event = harness
        .connector
        .normalize_inbound(raw_inbound(
            BTreeMap::from([
                (
                    "X-Hub-Signature-256".to_string(),
                    "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
                        .to_string(),
                ),
                ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
                ("X-GitHub-Event".to_string(), "ping".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]),
            b"Hello, World!",
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        ))
        .unwrap();

    assert_eq!(event.provider.as_str(), "github");
    assert_eq!(event.provider_payload.provider(), "github");
}

#[test]
fn connector_registry_default_lists_github_and_webhook_connectors() {
    let registry = ConnectorRegistry::default();
    let providers = registry.list();
    assert!(providers
        .iter()
        .any(|provider| provider.as_str() == "github"));
    assert!(providers
        .iter()
        .any(|provider| provider.as_str() == "webhook"));
}
