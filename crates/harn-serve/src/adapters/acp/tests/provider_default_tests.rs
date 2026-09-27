use super::*;

#[test]
fn acp_does_not_advertise_capabilities_from_another_providers_default() {
    let _guard = acp_env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _env = EnvSnapshot::capture(&[
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "LOCAL_LLM_BASE_URL",
        "LOCAL_LLM_MODEL",
    ]);
    let provider = "fixture-no-default";
    let mut overlay = harn_vm::llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.to_string(),
        harn_vm::llm_config::ProviderDef {
            base_url: "https://fixture.invalid/v1".to_string(),
            ..Default::default()
        },
    );
    harn_vm::llm_config::set_user_overrides(Some(overlay));
    std::env::set_var("HARN_LLM_PROVIDER", provider);
    std::env::remove_var("HARN_LLM_MODEL");
    std::env::remove_var("LOCAL_LLM_BASE_URL");
    std::env::remove_var("LOCAL_LLM_MODEL");
    assert!(configured_llm_route_for_capabilities().is_err());
    assert_eq!(
        acp_agent_capabilities()["promptCapabilities"],
        serde_json::json!({"image": false, "audio": false, "embeddedContext": false})
    );
    harn_vm::llm_config::clear_user_overrides();
}
