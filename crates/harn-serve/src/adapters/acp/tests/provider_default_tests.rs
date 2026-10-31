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
        serde_json::json!({
            "image": false,
            "audio": false,
            "embeddedContext": false,
            "delivery": {"steer": {}, "queue": {}},
        })
    );
    harn_vm::llm_config::clear_user_overrides();
}

#[test]
fn initialize_uses_the_logical_model_capabilities_for_managed_supply() {
    let _guard = acp_env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _env = EnvSnapshot::capture(&[
        "HARN_LLM_PROVIDER",
        "HARN_LLM_MODEL",
        "LOCAL_LLM_BASE_URL",
        "LOCAL_LLM_MODEL",
    ]);
    std::env::remove_var("LOCAL_LLM_BASE_URL");
    std::env::remove_var("LOCAL_LLM_MODEL");
    let overlay = harn_vm::llm_config::parse_config_toml(
        r#"
[providers.fixture_gateway]
base_url = "https://gateway.example.invalid/v1"
chat_endpoint = "/chat/completions"
managed_supply = { version = 2 }
"#,
    )
    .expect("managed provider overlay");

    for (provider, model, image) in [
        ("openai", "gpt-6-luna", true),
        ("fixture_gateway", "gpt-6-luna", true),
        (
            "fixture_gateway",
            "accounts/fireworks/models/gpt-oss-120b",
            false,
        ),
        ("fixture_gateway", "unknown-image-model-999", false),
        ("mlx", "qwen3-8b", false),
    ] {
        std::env::set_var("HARN_LLM_PROVIDER", provider);
        std::env::set_var("HARN_LLM_MODEL", model);
        let response = harn_clock::test_support::current_thread_runtime().block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let config = AcpServerConfig::new(None).with_llm_overrides(Some(overlay.clone()), None);
            let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(tx));
            server
                .handle_incoming_message(serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                }))
                .await;
            recv_response_with_id(&mut rx, 1).await
        });
        assert_eq!(
            response["result"]["agentCapabilities"]["promptCapabilities"]["image"], image,
            "initialize advertised the wrong image capability for {provider}:{model}: {response}"
        );
    }
}
