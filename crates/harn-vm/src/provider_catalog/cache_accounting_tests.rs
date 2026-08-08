use super::*;

#[test]
fn provider_catalog_owns_verified_cache_usage_accounting() {
    let catalog = artifact();
    for provider_id in ["anthropic", "openai", "deepseek", "openrouter"] {
        let provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .unwrap_or_else(|| panic!("{provider_id} is cataloged"));
        assert!(
            provider.cache_usage_accounting,
            "{provider_id} cache usage mapping is verified"
        );
    }

    for provider_id in ["ollama", "llamacpp"] {
        let provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .unwrap_or_else(|| panic!("{provider_id} is cataloged"));
        assert!(
            !provider.cache_usage_accounting,
            "{provider_id} does not report cache usage"
        );
    }
}

#[test]
fn swift_binding_defaults_pre_v7_cache_accounting_to_unsupported() {
    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("private let encodedCacheUsageAccounting: Bool?"));
    assert!(swift.contains(
        "public var cacheUsageAccounting: Bool { encodedCacheUsageAccounting ?? false }"
    ));
}
