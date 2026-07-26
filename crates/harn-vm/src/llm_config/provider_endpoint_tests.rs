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

#[test]
fn test_session_environment_governs_provider_endpoint_env() {
    let _guard = crate::llm::env_guard();
    const ENDPOINT: &str = "HARN_TEST_POLICY_ENDPOINT";
    unsafe {
        std::env::set_var(ENDPOINT, "https://ambient.example/v1");
    }
    let provider = ProviderDef {
        base_url: "https://catalog.example/v1".to_string(),
        base_url_env: Some(ENDPOINT.to_string()),
        ..Default::default()
    };

    crate::stdlib::process::set_session_environment(Some(
        crate::security::SessionEnvironment::isolated(),
    ));
    assert_eq!(resolve_base_url(&provider), "https://catalog.example/v1");

    let granted = crate::security::SessionEnvironment::launch(
        crate::security::EnvironmentPolicyKind::Granted,
        vec![crate::security::GrantSpec {
            name: "endpoint".to_string(),
            source: crate::security::GrantSourceSpec::Env {
                var: ENDPOINT.to_string(),
            },
            expose_as_env: Some(ENDPOINT.to_string()),
        }],
        &|name| std::env::var(name).ok(),
    )
    .unwrap();
    crate::stdlib::process::set_session_environment(Some(granted));
    assert_eq!(resolve_base_url(&provider), "https://ambient.example/v1");

    crate::stdlib::process::set_session_environment(None);
    unsafe {
        std::env::remove_var(ENDPOINT);
    }
}
