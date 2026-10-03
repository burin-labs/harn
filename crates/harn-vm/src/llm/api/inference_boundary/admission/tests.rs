use super::*;
use crate::llm::test_env::ScopedEnvVar;
use crate::llm_config::{DataControlScope, ProvidersConfig, TrainingDefault};

fn request(
    provider: &str,
    model: &str,
    reach: super::super::InferenceReach,
) -> InferenceAdmissionRequest {
    InferenceAdmissionRequest {
        provider: provider.into(),
        model: model.into(),
        boundary: Some(InferenceBoundary {
            reach,
            allow_training_discounts: false,
        }),
        data_controls: None,
    }
}

#[test]
fn preview_uses_the_effective_host_meet_and_preserves_missing_facts() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::set(
        super::super::HOST_BOUNDARY_ENV,
        r#"{"reach":"local_only","allow_training_discounts":false}"#,
    );
    let _key = ScopedEnvVar::set("OPENAI_API_KEY", "admission-preview-secret-marker");
    let snapshot = preview_inference_admission(&request(
        "openai",
        "gpt-5.6-luna",
        super::super::InferenceReach::AnyHosted,
    ));
    assert_eq!(snapshot.status, InferenceAdmissionStatus::Denied);
    assert_eq!(
        snapshot.effective_boundary.unwrap().reach,
        super::super::InferenceReach::LocalOnly
    );
    assert_eq!(
        snapshot.governing_rule.as_deref(),
        Some("inference_boundary.local_only")
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("admission-preview-secret-marker"));

    let unknown = preview_inference_admission(&request(
        "not-a-catalog-provider",
        "model",
        super::super::InferenceReach::AnyHosted,
    ));
    assert_eq!(unknown.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(unknown.local_runtime, None);
    assert_eq!(
        unknown.governing_rule.as_deref(),
        Some("inference_boundary.catalog_provider_unknown")
    );
}

#[test]
fn preview_admits_loopback_but_refuses_a_remote_local_provider_override() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let route = request(
        "ollama",
        "preview-model",
        super::super::InferenceReach::LocalOnly,
    );
    {
        let _endpoint = ScopedEnvVar::set("OLLAMA_HOST", "http://127.0.0.1:9");
        let snapshot = preview_inference_admission(&route);
        assert_eq!(snapshot.status, InferenceAdmissionStatus::Admitted);
        assert_eq!(snapshot.local_runtime, Some(true));
        assert_eq!(
            snapshot.governing_rule.as_deref(),
            Some("inference_boundary.local_runtime")
        );
    }
    let _endpoint = ScopedEnvVar::set(
        "OLLAMA_HOST",
        "https://remote.example.invalid/private-endpoint-marker",
    );
    let snapshot = preview_inference_admission(&route);
    assert_eq!(snapshot.status, InferenceAdmissionStatus::Denied);
    assert_eq!(
        snapshot.governing_rule.as_deref(),
        Some("inference_boundary.local_endpoint_untrusted")
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("private-endpoint-marker"));
}

#[test]
fn preview_needs_positive_open_weight_evidence_and_uses_model_training_overrides() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let open = preview_inference_admission(&request(
        "mistral",
        "mistral-medium-3-5",
        super::super::InferenceReach::HostedOpenWeight,
    ));
    assert_eq!(open.open_weight, Some(true));
    assert_eq!(open.status, InferenceAdmissionStatus::Admitted);
    let missing = preview_inference_admission(&request(
        "mistral",
        "unknown-weight-model",
        super::super::InferenceReach::HostedOpenWeight,
    ));
    assert_eq!(missing.open_weight, None);
    assert_eq!(missing.status, InferenceAdmissionStatus::Unknown);

    let standard = preview_inference_admission(&request(
        "meta",
        "muse-spark-1.3",
        super::super::InferenceReach::AnyHosted,
    ));
    let contributor = preview_inference_admission(&request(
        "meta",
        "muse-spark-1.3-contributor",
        super::super::InferenceReach::AnyHosted,
    ));
    assert_eq!(standard.training_default.as_deref(), Some("does_not_train"));
    assert_eq!(standard.status, InferenceAdmissionStatus::Admitted);
    assert_eq!(contributor.training_default.as_deref(), Some("trains"));
    assert_eq!(contributor.status, InferenceAdmissionStatus::Denied);
    let mut discounted = request(
        "meta",
        "muse-spark-1.3-contributor",
        super::super::InferenceReach::AnyHosted,
    );
    discounted
        .boundary
        .as_mut()
        .unwrap()
        .allow_training_discounts = true;
    assert_eq!(
        preview_inference_admission(&discounted).status,
        InferenceAdmissionStatus::Admitted
    );
}

#[test]
fn preview_credits_a_declared_shared_transport_control_without_crediting_native_adapters() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::llm_config::clear_user_overrides();
        }
    }
    let _reset = Reset;
    // Synthetic catalog declarations exercise the plan owner without claiming
    // these controls are supported by Meta or Gemini's actual service.
    let controls = crate::llm_config::provider_config("openrouter")
        .unwrap()
        .data_controls
        .unwrap()
        .request_controls;
    let mut overlay = ProvidersConfig::default();
    for provider in ["meta", "gemini"] {
        let mut definition = crate::llm_config::provider_config(provider).unwrap();
        let declaration = definition.data_controls.as_mut().unwrap();
        declaration.training_default = TrainingDefault::Trains;
        declaration.control_scope = DataControlScope::PerRequest;
        declaration.request_controls = controls.clone();
        for control in &mut declaration.request_controls {
            control.applies_to.clear();
        }
        overlay.providers.insert(provider.into(), definition);
    }
    crate::llm_config::set_user_overrides(Some(overlay));
    let mut shared = request(
        "meta",
        "muse-spark-1.3-contributor",
        super::super::InferenceReach::AnyHosted,
    );
    shared.data_controls = Some(DataPosture::StrictestAvailable);
    let shared = preview_inference_admission(&shared);
    assert!(shared.training_control_planned);
    assert_eq!(shared.status, InferenceAdmissionStatus::Admitted);
    let mut native = request(
        "gemini",
        "synthetic-native-model",
        super::super::InferenceReach::AnyHosted,
    );
    native.data_controls = Some(DataPosture::StrictestAvailable);
    let native = preview_inference_admission(&native);
    assert!(!native.training_control_planned);
    assert_eq!(native.status, InferenceAdmissionStatus::Denied);
}

#[test]
fn malformed_authority_is_unknown_without_echoing_supplied_values() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::set(super::super::HOST_BOUNDARY_ENV, "malformed-private-marker");
    let snapshot = preview_inference_admission(&request(
        "openai",
        "gpt-5.6-luna",
        super::super::InferenceReach::AnyHosted,
    ));
    assert_eq!(snapshot.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(
        snapshot.governing_rule.as_deref(),
        Some("inference_boundary.host_boundary_malformed")
    );
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("malformed-private-marker"));
}
