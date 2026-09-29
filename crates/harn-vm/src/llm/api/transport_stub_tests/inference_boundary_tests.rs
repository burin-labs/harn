//! Inference boundary checks on the real loopback HTTP path.
use super::*;

#[test]
fn host_local_ceiling_allows_loopback_transport_and_records_its_rule() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let server = spawn_ollama_stub();
    let _ollama_host = ScopedEnvVar::set("OLLAMA_HOST", format!("http://{}", server.addr()));
    let _boundary = ScopedEnvVar::set(
        super::super::inference_boundary::HOST_BOUNDARY_ENV,
        r#"{"reach":"local_only","allow_training_discounts":false}"#,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime
        .block_on(vm_call_llm_full(&base_opts("ollama")))
        .expect("loopback route is allowed");
    assert_eq!(result.text, "hello world");
    assert_eq!(
        result.telemetry.inference_boundary_rule.as_deref(),
        Some("inference_boundary.local_runtime")
    );
    assert_eq!(
        result
            .telemetry
            .data_controls
            .as_ref()
            .and_then(|receipt| receipt.inference_boundary_rule.as_deref()),
        Some("inference_boundary.local_runtime")
    );
    let evidence = result
        .telemetry
        .data_controls
        .as_ref()
        .and_then(|receipt| receipt.inference_catalog_evidence.as_ref())
        .expect("allowed route records the catalog facts used by the rule");
    assert!(evidence.local_runtime);
    assert_eq!(evidence.open_weight, None);
}

#[test]
fn host_local_ceiling_refuses_hosted_chat_before_transport() {
    let _guard = env_guard();
    let _allow_llm_transport = allow_stubbed_llm_transport();
    let _boundary = ScopedEnvVar::set(
        super::super::inference_boundary::HOST_BOUNDARY_ENV,
        r#"{"reach":"local_only","allow_training_discounts":false}"#,
    );
    let _openai_base = ScopedEnvVar::set("OPENAI_BASE_URL", "http://127.0.0.1:9");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut opts = base_opts("openai");
    opts.api_key = "invalid-test-key".into();
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("host ceiling must refuse this hosted route");
    assert!(format!("{error:?}").contains("inference_boundary.local_only"));
}
