//! Provider endpoint resolution and ephemeral runtime-route override tests.

use super::*;

fn reset_overrides() {
    clear_user_overrides();
    clear_runtime_provider_endpoint_overrides();
}

#[test]
fn test_resolve_base_url_no_env() {
    let pdef = ProviderDef {
        base_url: "https://example.com".to_string(),
        ..Default::default()
    };
    assert_eq!(resolve_base_url(&pdef), "https://example.com");
}

#[test]
fn test_runtime_provider_endpoint_requires_named_absolute_http_url() {
    assert!(RuntimeProviderEndpointOverrides::single("", "https://verified.example/v1").is_err());
    assert!(RuntimeProviderEndpointOverrides::single("fixture", "not-a-url").is_err());
    assert!(RuntimeProviderEndpointOverrides::single("fixture", "file:///tmp/llm").is_err());
    assert!(
        RuntimeProviderEndpointOverrides::single("fixture", " https://verified.example/v1 ")
            .is_ok()
    );
}

#[test]
fn test_runtime_provider_endpoint_wins_over_ambient_endpoint_env() {
    let _guard = crate::llm::env_guard();
    unsafe {
        std::env::set_var("HARN_TEST_RUNTIME_ENDPOINT", "https://ambient.example/v1");
    }
    let mut config = ProvidersConfig::default();
    config.providers.insert(
        "fixture".to_string(),
        ProviderDef {
            base_url: "https://catalog.example/v1".to_string(),
            base_url_env: Some("HARN_TEST_RUNTIME_ENDPOINT".to_string()),
            ..Default::default()
        },
    );
    set_user_overrides(Some(config));
    set_runtime_provider_endpoint_overrides(
        RuntimeProviderEndpointOverrides::single("fixture", " https://verified.example/v1 ")
            .expect("runtime endpoint override"),
    );

    let provider = provider_config("fixture").expect("runtime endpoint provider overlay");
    assert_eq!(resolve_base_url(&provider), "https://verified.example/v1");

    reset_overrides();
    unsafe {
        std::env::remove_var("HARN_TEST_RUNTIME_ENDPOINT");
    }
}
