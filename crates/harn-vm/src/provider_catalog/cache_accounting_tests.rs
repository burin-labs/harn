use super::*;

/// Project one provider through `catalog_provider` and read
/// `cache_usage_accounting` back off the serialized row, so the assertion sees
/// what a consumer of the published artifact sees rather than the in-memory
/// struct.
fn serialized_provider_field(declared: Option<bool>) -> Option<serde_json::Value> {
    let provider = crate::llm_config::ProviderDef {
        cache_usage_accounting: declared,
        ..crate::llm_config::ProviderDef::default()
    };
    let row = catalog_provider("probe".to_string(), provider);
    let encoded = serde_json::to_value(&row).expect("catalog row serializes");
    encoded
        .as_object()
        .expect("catalog row is a JSON object")
        .get("cache_usage_accounting")
        .cloned()
}

fn catalog_schema() -> serde_json::Value {
    schema_value()
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
