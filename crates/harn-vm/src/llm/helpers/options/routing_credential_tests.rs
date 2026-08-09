use super::extract::extract_llm_options;
use super::*;
use crate::llm_config::{AuthEnv, ProviderDef, ProvidersConfig};
use crate::value::VmDictExt;

fn test_provider(url: &str) -> ProviderDef {
    ProviderDef {
        base_url: url.to_string(),
        auth_style: "none".to_string(),
        auth_env: AuthEnv::None,
        chat_endpoint: "/chat/completions".to_string(),
        cost_per_1k_in: Some(0.0),
        cost_per_1k_out: Some(0.0),
        latency_p50_ms: Some(1000),
        ..Default::default()
    }
}

fn credentialed_test_provider(url: &str, env: &str) -> ProviderDef {
    ProviderDef {
        base_url: url.to_string(),
        auth_style: "bearer".to_string(),
        auth_env: AuthEnv::Single(env.to_string()),
        chat_endpoint: "/chat/completions".to_string(),
        ..Default::default()
    }
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn fallback_chain_defers_missing_primary_credentials_to_routing_executor() {
    let _guard = crate::llm::env_guard();
    let _missing_key = ScopedEnvVar::remove("HARN_TEST_MISSING_ROUTING_PRIMARY_KEY");
    crate::llm_config::clear_user_overrides();

    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "needs-key-primary".to_string(),
        credentialed_test_provider(
            "https://needs-key-primary.example/v1",
            "HARN_TEST_MISSING_ROUTING_PRIMARY_KEY",
        ),
    );
    overlay.providers.insert(
        "backup-no-key".to_string(),
        test_provider("https://backup-no-key.example/v1"),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "needs-key-primary");
    options.put_str("model", "needs-key-model");
    options.insert(
        crate::value::intern_key("fallback_chain"),
        VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            arcstr::ArcStr::from("backup-no-key".to_string()),
        )])),
    );

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("multi-link routing defers credential availability to executor");

    assert_eq!(opts.provider, "needs-key-primary");
    assert!(opts.api_key.is_empty());
    let policy = opts.routing_policy.expect("fallback lowered to routing");
    assert_eq!(policy.chain.len(), 2);
    assert_eq!(policy.chain[0].provider, "needs-key-primary");
    assert_eq!(policy.chain[1].provider, "backup-no-key");
    assert_eq!(policy.chain[1].model, "needs-key-model");

    crate::llm_config::clear_user_overrides();
}

#[test]
fn single_route_still_reports_missing_credentials_during_extraction() {
    let _guard = crate::llm::env_guard();
    let _missing_key = ScopedEnvVar::remove("HARN_TEST_MISSING_SINGLE_ROUTE_KEY");
    crate::llm_config::clear_user_overrides();

    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "needs-key-single".to_string(),
        credentialed_test_provider(
            "https://needs-key-single.example/v1",
            "HARN_TEST_MISSING_SINGLE_ROUTE_KEY",
        ),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "needs-key-single");
    options.put_str("model", "needs-key-model");
    let err = match extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ]) {
        Ok(_) => panic!("single-route missing credentials should fail during extraction"),
        Err(err) => err,
    };

    assert!(err
        .to_string()
        .contains("Missing API key: set HARN_TEST_MISSING_SINGLE_ROUTE_KEY environment variable"));

    crate::llm_config::clear_user_overrides();
}
