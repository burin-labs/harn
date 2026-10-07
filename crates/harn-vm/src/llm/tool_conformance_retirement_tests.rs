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
    assert_eq!(report.evidence_source, ToolProbeEvidenceSource::Unknown);
    assert!(report.require_live_evidence().is_err());
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

#[test]
fn retired_adapter_refusal_is_not_live_provider_evidence() {
    let _guard = crate::llm::env_guard();
    let options = ToolConformanceProbeOptions::new("deepinfra", "deepinfra/Qwen/Qwen3.8-2.4T-A95B");
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(run_tool_conformance_probe(options));
    assert_eq!(report.cases.len(), 2);
    assert!(report.cases.iter().all(|case| {
        case.failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("select a different model explicitly"))
    }));
    assert_eq!(report.evidence_source, ToolProbeEvidenceSource::Unknown);
    assert!(report.require_live_evidence().is_err());
}

#[test]
fn completed_adapter_response_retains_observation_provenance() {
    let _guard = crate::llm::env_guard();
    let mut options = ToolConformanceProbeOptions::new("mock", "mock");
    options.modes = vec![ToolProbeMode::NonStreaming];
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(run_tool_conformance_probe(options));
    assert_eq!(report.cases.len(), 1);
    assert!(
        report.cases[0].usage.is_some(),
        "adapter response was reached"
    );
    assert_eq!(report.evidence_source, ToolProbeEvidenceSource::LiveRequest);
    assert!(report.require_live_evidence().is_ok());
}
