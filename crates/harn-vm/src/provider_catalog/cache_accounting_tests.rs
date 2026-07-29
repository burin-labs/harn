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
