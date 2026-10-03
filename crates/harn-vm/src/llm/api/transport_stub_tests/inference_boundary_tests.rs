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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let mut opts = base_opts("openai");
    opts.api_key = "invalid-test-key".into();
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("host ceiling must refuse this hosted route");
    assert_local_policy_denial(&error);

    let local = tokio::task::LocalSet::new();
    let offthread = runtime.block_on(local.run_until(async {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        vm_call_llm_full_streaming_offthread(&opts, tx)
            .await
            .expect_err("off-thread dispatch must retain the host refusal")
    }));
    assert_local_policy_denial(&offthread);
}

fn assert_local_policy_denial(error: &crate::value::VmError) {
    let projected = crate::llm::call::build_llm_error_dict(error, "openai", "test-model");
    let crate::value::VmValue::Dict(fields) = projected else {
        panic!("the real call must project a structured denial");
    };
    let facts = crate::llm::helpers::vm_value_dict_to_json(&fields);
    assert_eq!(facts["reason"], "policy_denied");
    assert_eq!(facts["category"], "egress_blocked");
    assert_eq!(facts["origin"], "local");
    assert_eq!(facts["rule"], "inference_boundary.local_only");
    assert_eq!(facts["code"], "inference_boundary.local_only");
    assert_eq!(facts["retryable"], false);
    assert_eq!(
        crate::llm::agent_terminal_class("provider_error", "provider_error", Some(&facts)),
        Some(crate::llm::AgentTerminalClass::ToolPolicyRejected),
    );
}
