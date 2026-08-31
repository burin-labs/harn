//! Final Anthropic request checks that must cross the HTTP transport seam.

use super::{
    allow_stubbed_llm_transport, base_opts, env_guard, spawn_anthropic_stub_with_request_capture,
    vm_call_llm_full,
};

#[test]
fn override_cannot_restore_unsupported_prefill_at_egress() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server = spawn_anthropic_stub_with_request_capture(captured.clone());
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.providers.insert(
            "anthropic".to_string(),
            crate::llm_config::ProviderDef {
                base_url: format!("http://{}", server.addr()),
                auth_style: "none".to_string(),
                auth_env: crate::llm_config::AuthEnv::None,
                extra_headers: std::collections::BTreeMap::from([(
                    "anthropic-version".to_string(),
                    "2023-06-01".to_string(),
                )]),
                chat_endpoint: "/messages".to_string(),
                ..Default::default()
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let mut opts = base_opts("anthropic");
        // The builder sees a prefill-capable route. The caller then replaces
        // both model and messages at the wire-override seam, so final
        // reconciliation must read the model the request will send.
        opts.model = "claude-opus-4-5".to_string();
        opts.stream = false;
        opts.output_format = super::super::OutputFormat::Text;
        opts.output_schema = None;
        opts.native_tools = None;
        opts.tool_choice = None;
        opts.provider_overrides = Some(serde_json::json!({
            "model": "claude-opus-4-6",
            "messages": [
                {"role": "user", "content": "Inspect the workspace"},
                {"role": "assistant", "content": "I will continue by"},
            ],
        }));
        let result = vm_call_llm_full(&opts)
            .await
            .expect("stubbed Anthropic response");

        crate::llm_config::clear_user_overrides();
        drop(server);

        assert_eq!(result.text, "ok");
        let request = captured
            .lock()
            .expect("captured request")
            .clone()
            .expect("request captured");
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
        assert_eq!(
            body["model"], "claude-opus-4-6",
            "the negative control must reach the overridden unsupported model"
        );
        assert_eq!(
            body["messages"],
            serde_json::json!([{"role": "user", "content": "Inspect the workspace"}]),
            "final egress must reconcile messages after caller overrides"
        );
    });
}
