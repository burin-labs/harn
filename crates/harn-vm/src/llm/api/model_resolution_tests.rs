use super::options::base_opts;
use super::vm_call_llm_full;

#[test]
fn model_resolution_drift_fails_before_provider_dispatch() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut opts = base_opts("fake");
        opts.model = "actual-model".to_string();
        opts.model_resolution = Some(crate::llm_config::ModelResolution {
            requested_model: "openai:gpt-5.6-sol".to_string(),
            alias_chain: Vec::new(),
            resolved_provider: "openai".to_string(),
            resolved_model: "gpt-5.6-sol".to_string(),
            catalog_version: crate::llm_config::MODEL_CATALOG_VERSION.to_string(),
        });

        let error = vm_call_llm_full(&opts)
            .await
            .expect_err("receipt/payload drift must fail before fake provider dispatch");
        assert!(
            error
                .to_string()
                .contains("model resolution invariant failed before provider call"),
            "unexpected error: {error}"
        );
    });
}
