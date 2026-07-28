use super::util::parse_rfc3339;
use super::*;
use crate::redact::REDACTED_HEADER_VALUE;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::sync::Arc;

struct OwnedProviderSchema {
    metadata: ProviderMetadata,
}

impl OwnedProviderSchema {
    fn new(provider: &str, schema_name: &str) -> Self {
        Self {
            metadata: ProviderMetadata {
                provider: provider.to_string(),
                kinds: vec!["webhook".to_string()],
                schema_name: schema_name.to_string(),
                runtime: ProviderRuntimeMetadata::Placeholder,
                ..ProviderMetadata::default()
            },
        }
    }
}

impl ProviderSchema for OwnedProviderSchema {
    fn provider_id(&self) -> &str {
        &self.metadata.provider
    }

    fn harn_schema_name(&self) -> &str {
        &self.metadata.schema_name
    }

    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn normalize(
        &self,
        _kind: &str,
        _headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        Ok(ProviderPayload::Extension(ExtensionProviderPayload {
            provider: self.metadata.provider.clone(),
            schema_name: self.metadata.schema_name.clone(),
            raw,
        }))
    }
}

fn owned_provider_schema(provider: &str, schema_name: &str) -> Arc<dyn ProviderSchema> {
    Arc::new(OwnedProviderSchema::new(provider, schema_name))
}

fn sample_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("Authorization".to_string(), "Bearer secret".to_string()),
        ("Cookie".to_string(), "session=abc".to_string()),
        ("User-Agent".to_string(), "GitHub-Hookshot/123".to_string()),
        ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
        ("X-GitHub-Event".to_string(), "issues".to_string()),
        ("X-Webhook-Token".to_string(), "token".to_string()),
    ])
}

#[test]
fn default_redaction_policy_keeps_safe_headers() {
    let redacted = redact_headers(&sample_headers(), &HeaderRedactionPolicy::default());
    assert_eq!(redacted.get("User-Agent").unwrap(), "GitHub-Hookshot/123");
    assert_eq!(redacted.get("X-GitHub-Delivery").unwrap(), "delivery-123");
    assert_eq!(
        redacted.get("Authorization").unwrap(),
        REDACTED_HEADER_VALUE
    );
    assert_eq!(redacted.get("Cookie").unwrap(), REDACTED_HEADER_VALUE);
    assert_eq!(
        redacted.get("X-Webhook-Token").unwrap(),
        REDACTED_HEADER_VALUE
    );
}

#[test]
fn provider_catalog_rejects_duplicates() {
    let mut catalog = ProviderCatalog::default();
    catalog
        .register(owned_provider_schema("github", "GitHubEventPayload"))
        .unwrap();
    let error = catalog
        .register(owned_provider_schema("github", "GitHubEventPayload"))
        .unwrap_err();
    assert_eq!(
        error,
        ProviderCatalogError::DuplicateProvider("github".to_string())
    );
}

#[test]
fn merging_contributions_preserves_each_package() {
    let mut catalog = ProviderCatalog::with_defaults();
    catalog
        .merge(vec![owned_provider_schema("runtime-a", "RuntimeAPayload")])
        .unwrap();
    catalog
        .merge(vec![owned_provider_schema("runtime-b", "RuntimeBPayload")])
        .unwrap();

    assert!(catalog.metadata_for("runtime-a").is_some());
    assert!(catalog.metadata_for("runtime-b").is_some());
    assert!(catalog.metadata_for("github").is_none());
}

#[test]
fn reloading_the_same_package_is_idempotent() {
    let mut catalog = ProviderCatalog::with_defaults();
    let schemas = || vec![owned_provider_schema("runtime-a", "RuntimeAPayload")];
    catalog.merge(schemas()).unwrap();
    catalog.merge(schemas()).expect("reloading is idempotent");
}

#[test]
fn conflicting_package_schema_does_not_displace_owner() {
    let mut catalog = ProviderCatalog::with_defaults();
    catalog
        .merge(vec![owned_provider_schema("runtime-a", "RuntimeAPayload")])
        .unwrap();
    let error = catalog
        .merge(vec![owned_provider_schema("runtime-a", "OtherPayload")])
        .unwrap_err();

    assert_eq!(
        error,
        ProviderCatalogError::DuplicateProvider("runtime-a".to_string())
    );
    assert_eq!(
        catalog.metadata_for("runtime-a").unwrap().schema_name,
        "RuntimeAPayload"
    );
}

#[test]
fn package_cannot_displace_a_core_provider() {
    let mut catalog = ProviderCatalog::with_defaults();
    catalog
        .merge(vec![owned_provider_schema(
            "webhook",
            "PackageWebhookPayload",
        )])
        .expect("core provider remains authoritative");

    assert_eq!(
        catalog.metadata_for("webhook").unwrap().schema_name,
        "GenericWebhookPayload"
    );
}

#[test]
fn default_catalog_contains_only_core_provider_schemas() {
    let entries = registered_provider_metadata();
    for provider in ["github", "linear", "notion", "slack"] {
        assert!(
            entries.iter().all(|entry| entry.provider != provider),
            "{provider} must be registered only by its Harn package"
        );
    }
    for provider in ["a2a-push", "cron", "webhook"] {
        assert!(entries.iter().any(|entry| entry.provider == provider));
    }
    let kafka = entries
        .iter()
        .find(|entry| entry.provider == "kafka")
        .expect("kafka stream provider");
    assert_eq!(kafka.kinds, vec!["stream".to_string()]);
    assert_eq!(kafka.schema_name, "StreamEventPayload");
}

#[test]
fn extension_trigger_event_round_trip_is_stable() {
    let provider = ProviderId::from("github");
    let event = TriggerEvent {
        id: TriggerEventId("trigger_evt_fixed".to_string()),
        provider: provider.clone(),
        kind: "issues".to_string(),
        received_at: parse_rfc3339("2026-04-19T07:00:00Z").unwrap(),
        occurred_at: Some(parse_rfc3339("2026-04-19T06:59:59Z").unwrap()),
        dedupe_key: "delivery-123".to_string(),
        trace_id: TraceId("trace_fixed".to_string()),
        tenant_id: Some(TenantId("tenant_1".to_string())),
        headers: redact_headers(&sample_headers(), &HeaderRedactionPolicy::default()),
        provider_payload: ProviderPayload::Extension(ExtensionProviderPayload {
            provider: provider.as_str().to_string(),
            schema_name: "GitHubEventPayload".to_string(),
            raw: serde_json::json!({
                "action": "opened",
                "installation": {"id": 42},
                "issue": {"number": 99}
            }),
        }),
        signature_status: SignatureStatus::Verified,
        dedupe_claimed: false,
        batch: None,
        raw_body: Some(vec![0, 159, 255, 10]),
    };

    let once = serde_json::to_value(&event).unwrap();
    assert_eq!(once["raw_body"], serde_json::json!("AJ//Cg=="));
    let decoded: TriggerEvent = serde_json::from_value(once.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), once);
    assert_eq!(decoded, event);
}
