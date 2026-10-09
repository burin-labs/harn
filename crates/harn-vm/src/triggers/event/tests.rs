use super::util::parse_rfc3339;
use super::*;
use crate::redact::REDACTED_HEADER_VALUE;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
fn provider_metadata(provider: &str, schema_name: &str) -> ProviderMetadata {
    ProviderMetadata {
        provider: provider.to_string(),
        kinds: vec!["webhook".to_string()],
        schema_name: schema_name.to_string(),
        runtime: ProviderRuntimeMetadata::Placeholder,
        ..ProviderMetadata::default()
    }
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
        .register(provider_metadata("github", "GitHubEventPayload"))
        .unwrap();
    let error = catalog
        .register(provider_metadata("github", "GitHubEventPayload"))
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
        .merge(vec![provider_metadata("runtime-a", "RuntimeAPayload")])
        .unwrap();
    catalog
        .merge(vec![provider_metadata("runtime-b", "RuntimeBPayload")])
        .unwrap();

    assert!(catalog.metadata_for("runtime-a").is_some());
    assert!(catalog.metadata_for("runtime-b").is_some());
    assert!(catalog.metadata_for("github").is_none());
}

#[test]
fn reloading_the_same_package_is_idempotent() {
    let mut catalog = ProviderCatalog::with_defaults();
    let providers = || vec![provider_metadata("runtime-a", "RuntimeAPayload")];
    catalog.merge(providers()).unwrap();
    catalog.merge(providers()).expect("reloading is idempotent");
}

#[test]
fn same_schema_name_with_different_metadata_is_a_conflict() {
    let mut catalog = ProviderCatalog::with_defaults();
    catalog
        .merge(vec![provider_metadata("runtime-a", "RuntimeAPayload")])
        .unwrap();
    let mut changed = provider_metadata("runtime-a", "RuntimeAPayload");
    changed.kinds = vec!["poll".to_string()];

    assert_eq!(
        catalog.merge(vec![changed]).unwrap_err(),
        ProviderCatalogError::DuplicateProvider("runtime-a".to_string())
    );
    assert_eq!(
        catalog.metadata_for("runtime-a").unwrap().kinds,
        vec!["webhook".to_string()]
    );
}

#[test]
fn failed_merge_keeps_the_original_catalog() {
    let mut catalog = ProviderCatalog::with_defaults();
    let original = catalog.entries();
    let error = catalog
        .merge(vec![
            provider_metadata("runtime-a", "RuntimeAPayload"),
            provider_metadata("webhook", "PackageWebhookPayload"),
        ])
        .unwrap_err();

    assert_eq!(
        error,
        ProviderCatalogError::DuplicateProvider("webhook".to_string())
    );
    assert_eq!(catalog.entries(), original);
    assert!(catalog.metadata_for("runtime-a").is_none());
}

#[test]
fn conflicting_package_schema_does_not_displace_owner() {
    let mut catalog = ProviderCatalog::with_defaults();
    catalog
        .merge(vec![provider_metadata("runtime-a", "RuntimeAPayload")])
        .unwrap();
    let error = catalog
        .merge(vec![provider_metadata("runtime-a", "OtherPayload")])
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
    let error = catalog
        .merge(vec![provider_metadata("webhook", "PackageWebhookPayload")])
        .unwrap_err();

    assert_eq!(
        error,
        ProviderCatalogError::DuplicateProvider("webhook".to_string())
    );
    assert_eq!(
        catalog.metadata_for("webhook").unwrap().schema_name,
        "GenericWebhookPayload"
    );
}

#[test]
fn invalid_registration_is_rejected_before_install() {
    let mut catalog = ProviderCatalog::default();
    let error = catalog
        .register(provider_metadata(" ", "RuntimeAPayload"))
        .unwrap_err();
    assert!(matches!(error, ProviderCatalogError::InvalidMetadata(_)));

    let mut spoofed = provider_metadata("package-provider", "PackagePayload");
    spoofed.runtime = ProviderRuntimeMetadata::Builtin {
        connector: "webhook".into(),
        default_signature_variant: None,
    };
    let error = catalog.register(spoofed).unwrap_err();
    assert!(matches!(error, ProviderCatalogError::InvalidMetadata(_)));
    assert!(catalog.entries().is_empty());
}

#[test]
fn extension_normalization_uses_the_registered_identity() {
    let mut catalog = ProviderCatalog::default();
    catalog
        .register(provider_metadata("runtime-a", "RuntimeAPayload"))
        .unwrap();

    let raw = serde_json::json!({"id": 1});
    let payload = catalog
        .normalize(
            &ProviderId::from("runtime-a"),
            "webhook",
            &BTreeMap::new(),
            raw.clone(),
        )
        .unwrap();
    assert_eq!(
        payload,
        ProviderPayload::Extension(ExtensionProviderPayload {
            provider: "runtime-a".into(),
            schema_name: "RuntimeAPayload".into(),
            raw,
        })
    );
    assert_eq!(
        catalog
            .normalize(
                &ProviderId::from("runtime-b"),
                "webhook",
                &BTreeMap::new(),
                JsonValue::Null,
            )
            .unwrap_err(),
        ProviderCatalogError::UnknownProvider("runtime-b".into())
    );
}

#[test]
fn default_catalog_contains_only_core_provider_schemas() {
    let catalog = ProviderCatalog::with_defaults();
    let entries = catalog.entries();
    assert_eq!(entries.len(), 9);
    for entry in &entries {
        let payload = catalog
            .normalize(
                &ProviderId::from(entry.provider.as_str()),
                "test",
                &BTreeMap::new(),
                JsonValue::Null,
            )
            .unwrap();
        assert_eq!(payload.provider(), entry.provider);
    }
    for provider in ["github", "linear", "slack"] {
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
