use super::*;

/// Project one provider through `catalog_provider` and read
/// `cache_usage_accounting` back off the serialized row, so the assertion sees
/// what a consumer of the published artifact sees rather than the in-memory
/// struct.
fn serialized_provider_field(declared: Option<bool>) -> Option<serde_json::Value> {
    let provider = crate::config::ProviderDef {
        cache_usage_accounting: declared,
        ..crate::config::ProviderDef::default()
    };
    let row = catalog_provider("probe".to_string(), provider);
    let encoded = serde_json::to_value(&row).expect("catalog row serializes");
    encoded
        .as_object()
        .expect("catalog row is a JSON object")
        .get("cache_usage_accounting")
        .cloned()
}

#[test]
fn october_accounting_audit_publishes_evidence_and_declared_wire_fields() {
    let config = crate::config::embedded_config(None);
    let audit = config
        .usage_accounting_audit
        .as_ref()
        .expect("audit registry");
    let providers = [
        "azure_openai",
        "baseten",
        "dashscope",
        "flexai",
        "friendli",
        "inception",
        "mistral",
        "mlx",
        "nebius",
        "qianfan",
        "tgi",
        "vercel_ai_gateway",
        "volcengine_ark",
        "zai",
    ];
    for id in providers {
        let provider = config.providers.get(id).expect("catalogued provider");
        let row = serde_json::to_value(catalog_provider(id.to_string(), provider.clone())).unwrap();
        assert_eq!(
            row.get("stream_usage_accounting"),
            Some(&serde_json::json!(true)),
            "{id}"
        );
        assert_eq!(
            row.get("cache_usage_accounting"),
            Some(&serde_json::json!(id != "tgi")),
            "{id}"
        );
        let evidence = audit
            .verified
            .iter()
            .find(|entry| entry.provider == id)
            .expect("dated evidence");
        assert_eq!(evidence.checked_on, "2026-10-16");
        assert_eq!(evidence.fields.len(), 2);
        assert!(!evidence.sources.is_empty());
        assert!(!audit.unverified.iter().any(|entry| entry == id));
    }
    // A concrete serializer without cache fields is an audited false. An
    // arbitrary local proxy remains unknown and must not inherit that claim.
    assert_eq!(config.providers["local"].cache_usage_accounting, None);
    assert_eq!(config.providers["hunyuan"].cache_usage_accounting, None);
    assert_eq!(
        config.providers["hunyuan"].stream_usage_accounting,
        Some(true)
    );
    let hunyuan = audit
        .verified
        .iter()
        .find(|entry| entry.provider == "hunyuan")
        .expect("documented terminal usage contract");
    assert_eq!(
        hunyuan.fields,
        [crate::config::UsageAccountingField::Stream]
    );
    assert_eq!(hunyuan.checked_on, "2026-11-01");
    assert!(!audit
        .unverified
        .iter()
        .any(|provider| provider == "hunyuan"));
    // One Hugging Face upstream's counters do not prove the entire gateway.
    assert_eq!(config.providers["huggingface"].cache_usage_accounting, None);
    assert_eq!(
        config.providers["huggingface"].stream_usage_accounting,
        Some(true)
    );
    assert_eq!(
        audit
            .verified
            .iter()
            .find(|entry| entry.provider == "huggingface")
            .unwrap()
            .fields,
        [crate::config::UsageAccountingField::Stream]
    );
    // Conditional emission is supported; the ledger retains category absence.
    assert_eq!(config.providers["vllm"].cache_usage_accounting, Some(true));
    assert_eq!(config.providers["vllm"].stream_usage_accounting, Some(true));
    assert_eq!(
        audit
            .verified
            .iter()
            .find(|entry| entry.provider == "vllm")
            .unwrap()
            .fields,
        [
            crate::config::UsageAccountingField::Cache,
            crate::config::UsageAccountingField::Stream
        ]
    );
    assert_eq!(audit.expires_on, "2026-10-31");
}

fn catalog_schema() -> serde_json::Value {
    schema_value()
}

#[test]
fn cerebras_and_together_publish_both_verified_accounting_fields() {
    let config = crate::config::embedded_config(None);
    let audit = config
        .usage_accounting_audit
        .as_ref()
        .expect("audit registry");
    for id in ["cerebras", "together"] {
        let provider = config.providers.get(id).expect("catalogued provider");
        let row = serde_json::to_value(catalog_provider(id.to_string(), provider.clone()))
            .expect("published provider row");
        assert_eq!(
            row.get("cache_usage_accounting"),
            Some(&serde_json::json!(true)),
            "{id}"
        );
        assert_eq!(
            row.get("stream_usage_accounting"),
            Some(&serde_json::json!(true)),
            "{id}"
        );
        let verified = audit
            .verified
            .iter()
            .find(|entry| entry.provider == id)
            .expect("provider evidence");
        assert_eq!(verified.fields.len(), 2, "{id} must complete both fields");
        assert!(!audit.unverified.iter().any(|entry| entry == id));
    }
    let pending = config.providers.get("local").expect("unaudited provider");
    let row = serde_json::to_value(catalog_provider("local".to_string(), pending.clone()))
        .expect("pending provider row");
    assert_eq!(
        row.get("cache_usage_accounting"),
        None,
        "an unverified cache field must remain absent"
    );
    assert_eq!(row.get("stream_usage_accounting"), None);
}

#[test]
fn swift_binding_defaults_pre_v7_cache_accounting_to_unsupported() {
    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("private let encodedCacheUsageAccounting: Bool?"));
    assert!(swift.contains(
        "public var cacheUsageAccounting: Bool { encodedCacheUsageAccounting ?? false }"
    ));
}

/// An undeclared route must stay undeclared through the published catalog.
///
/// `Some(false)` is a claim — that the route reports no cache usage, which
/// consumers surface as an audited zero. `None` is the absence of a claim.
/// Collapsing the second into the first puts an assertion in the mouth of every
/// route nobody has looked at, and no downstream reader can tell them apart
/// afterwards, because the collapse happens at the boundary that owns the shape.
///
/// This is the falsifier for that: restore `.unwrap_or(false)` in
/// `catalog_provider` and the absent case below fails.
#[test]
fn an_undeclared_cache_accounting_row_is_omitted_rather_than_published_as_false() {
    let declared_true = serialized_provider_field(Some(true));
    let declared_false = serialized_provider_field(Some(false));
    let undeclared = serialized_provider_field(None);

    assert_eq!(
        declared_true,
        Some(serde_json::json!(true)),
        "an explicit true must survive projection",
    );
    assert_eq!(
        declared_false,
        Some(serde_json::json!(false)),
        "an explicit false is a real claim and must survive projection",
    );
    assert_eq!(
        undeclared, None,
        "an undeclared route must omit the field, not publish it as false",
    );
}

/// The schema must not require what an undeclared row omits, or the projection
/// above emits documents its own schema rejects.
#[test]
fn cache_accounting_is_not_required_by_the_catalog_schema() {
    let schema = catalog_schema();
    let required = schema["$defs"]["provider"]["required"]
        .as_array()
        .expect("provider schema declares a required list");
    assert!(
        !required
            .iter()
            .any(|field| field.as_str() == Some("cache_usage_accounting")),
        "cache_usage_accounting must be optional so an undeclared row validates",
    );
    assert!(
        required.iter().any(|field| field.as_str() == Some("id")),
        "guard against an empty or renamed required list passing this vacuously",
    );
}
