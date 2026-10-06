use super::*;

#[test]
fn live_probe_preserves_retired_catalog_identity_through_wire_overlay() {
    let _guard = crate::llm::env_guard();
    let retired = "deepinfra/Qwen/Qwen3.8-2.4T-A95B";
    let mut overlay = llm_config::ProvidersConfig::default();
    let mut renamed = llm_config::effective_config().models["Qwen/Qwen3.8-2.4T-A95B"].clone();
    renamed.provider = "deepinfra".to_string();
    renamed.wire_model = Some("private-probe-model".to_string());
    overlay.models.insert(retired.to_string(), renamed);
    llm_config::set_user_overrides(Some(overlay));
    let mut options = ToolConformanceProbeOptions::new("deepinfra", retired);
    options.base_url = Some("http://127.0.0.1:9".to_string());
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(run_tool_conformance_probe(options));
    llm_config::clear_user_overrides();
    assert_eq!(report.cases.len(), 2);
    for case in report.cases {
        assert!(
            case.failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("select a different model explicitly")),
            "{case:?}"
        );
        assert_eq!(case.elapsed_ms, None);
        assert_eq!(case.http_status, None);
        assert!(case.usage.is_none());
    }
}
