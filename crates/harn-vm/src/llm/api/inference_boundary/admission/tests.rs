use super::*;
use crate::llm::test_env::ScopedEnvVar;
use crate::llm_config::{DataControlScope, ProvidersConfig, TrainingDefault};

#[test]
fn preflight_reports_malformed_endpoints_before_inference_admission() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _region = ScopedEnvVar::remove("ZAI_REGION");
    let route = request("zai", "glm-5.3", super::super::InferenceReach::AnyHosted);
    for (endpoint, expected) in [
        (
            "https://[invalid",
            crate::llm_config::ProviderEndpointConfigurationError::InvalidUrl,
        ),
        (
            "https://userinfo:credential-marker@[invalid/path-marker",
            crate::llm_config::ProviderEndpointConfigurationError::InvalidUrl,
        ),
        (
            "file:///credential-marker",
            crate::llm_config::ProviderEndpointConfigurationError::NotAbsoluteHttp,
        ),
    ] {
        let _endpoint = ScopedEnvVar::set("ZAI_BASE_URL", endpoint);
        let policy_only = preview_inference_admission(&route);
        assert_eq!(
            policy_only.governing_rule.as_deref(),
            Some("inference_boundary.training_unknown")
        );
        let error = crate::llm::api::preflight_inference_admission(&route).unwrap_err();
        assert_eq!(error, expected);
        let diagnostic = format!(
            "{error} {error:?} {}",
            serde_json::to_string(&error).unwrap()
        );
        for supplied in [endpoint, "userinfo", "credential-marker", "path-marker"] {
            assert!(
                !diagnostic.contains(supplied),
                "configuration diagnostic exposed supplied endpoint bytes"
            );
        }
    }
}

#[test]
fn preflight_preserves_valid_route_admission_and_missing_facts() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _region = ScopedEnvVar::remove("ZAI_REGION");
    let _endpoint = ScopedEnvVar::remove("ZAI_BASE_URL");
    let route = request("zai", "glm-5.3", super::super::InferenceReach::AnyHosted);
    let known = crate::llm::api::preflight_inference_admission(&route).unwrap();
    assert_eq!(known.status, InferenceAdmissionStatus::Admitted);
    {
        let _endpoint = ScopedEnvVar::set("ZAI_BASE_URL", "https://unverified.example/v1");
        let unknown = crate::llm::api::preflight_inference_admission(&route).unwrap();
        assert_eq!(unknown.status, InferenceAdmissionStatus::Unknown);
        assert_eq!(
            unknown.governing_rule.as_deref(),
            Some("inference_boundary.training_unknown")
        );
        assert_eq!(unknown.training_default, None);
    }
    let missing = crate::llm::api::preflight_inference_admission(&request(
        "unknown-endpoint-provider",
        "unknown-model",
        super::super::InferenceReach::AnyHosted,
    ))
    .unwrap();
    assert_eq!(missing.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(
        missing.governing_rule.as_deref(),
        Some("inference_boundary.catalog_provider_unknown")
    );
}

#[test]
fn preflight_uses_the_canonical_endpoint_override_precedence() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _region = ScopedEnvVar::remove("ZAI_REGION");
    let _endpoint = ScopedEnvVar::remove("ZAI_BASE_URL");
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::llm_config::clear_user_overrides();
            crate::llm_config::clear_runtime_provider_endpoint_overrides();
        }
    }
    let _reset = Reset;
    let route = request("zai", "glm-5.3", super::super::InferenceReach::AnyHosted);
    let catalog = crate::llm_config::provider_config("zai").unwrap();
    let mut project = ProvidersConfig::default();
    project.providers.insert(
        "zai".into(),
        crate::llm_config::ProviderDef {
            base_url: "https://[invalid-project".into(),
            ..catalog.clone()
        },
    );
    crate::llm_config::set_user_overrides(Some(project));
    assert_eq!(
        crate::llm::api::preflight_inference_admission(&route).unwrap_err(),
        crate::llm_config::ProviderEndpointConfigurationError::InvalidUrl
    );
    {
        let _endpoint = ScopedEnvVar::set("ZAI_BASE_URL", &catalog.base_url);
        assert_eq!(
            crate::llm::api::preflight_inference_admission(&route)
                .unwrap()
                .status,
            InferenceAdmissionStatus::Admitted
        );
    }
    let _endpoint = ScopedEnvVar::set("ZAI_BASE_URL", "https://[invalid-environment");
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        crate::llm_config::RuntimeProviderEndpointOverrides::single(
            "zai",
            "https://unverified.example/v1",
        )
        .unwrap(),
    );
    let runtime = crate::llm::api::preflight_inference_admission(&route).unwrap();
    assert_eq!(runtime.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(
        runtime.governing_rule.as_deref(),
        Some("inference_boundary.training_unknown")
    );
}

#[test]
fn preflight_reads_only_the_selected_session_environment() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _region = ScopedEnvVar::remove("ZAI_REGION");
    let _endpoint = ScopedEnvVar::set("ZAI_BASE_URL", "https://[invalid-ambient");
    struct Reset(Option<crate::security::SessionEnvironment>);
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::security::session_environment::set_session_environment(self.0.take());
        }
    }
    let _reset = Reset(crate::security::session_environment::current_session_environment());
    let route = request("zai", "glm-5.3", super::super::InferenceReach::AnyHosted);
    crate::security::session_environment::set_session_environment(Some(
        crate::security::SessionEnvironment::isolated(),
    ));
    assert_eq!(
        crate::llm::api::preflight_inference_admission(&route)
            .unwrap()
            .status,
        InferenceAdmissionStatus::Admitted
    );
    let granted = crate::security::SessionEnvironment::launch(
        crate::security::EnvironmentPolicyKind::Granted,
        vec![crate::security::GrantSpec {
            name: "endpoint".into(),
            source: crate::security::GrantSourceSpec::Env {
                var: "ZAI_BASE_URL".into(),
            },
            expose_as_env: Some("ZAI_BASE_URL".into()),
            for_command: None,
            expose_to: Default::default(),
        }],
        &|name| match name {
            "ZAI_BASE_URL" => Some("https://[invalid-ambient".into()),
            _ => None,
        },
    )
    .unwrap();
    crate::security::session_environment::set_session_environment(Some(granted));
    assert_eq!(
        crate::llm::api::preflight_inference_admission(&route).unwrap_err(),
        crate::llm_config::ProviderEndpointConfigurationError::InvalidUrl
    );
}

#[test]
fn preflight_preserves_native_catalog_templates_and_acp_transports() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _bedrock = ScopedEnvVar::remove("BEDROCK_BASE_URL");
    let _azure = ScopedEnvVar::remove("AZURE_OPENAI_ENDPOINT");
    for provider in ["bedrock", "azure_openai"] {
        let route = request(
            provider,
            "fixture-model",
            super::super::InferenceReach::AnyHosted,
        );
        let policy = preview_inference_admission(&route);
        let checked = crate::llm::api::preflight_inference_admission(&route).unwrap();
        assert_eq!(checked.status, policy.status);
        assert_eq!(checked.governing_rule, policy.governing_rule);
    }
    let _azure = ScopedEnvVar::set("AZURE_OPENAI_ENDPOINT", "https://[invalid-override");
    let route = request(
        "azure_openai",
        "fixture-model",
        super::super::InferenceReach::AnyHosted,
    );
    assert_eq!(
        crate::llm::api::preflight_inference_admission(&route).unwrap_err(),
        crate::llm_config::ProviderEndpointConfigurationError::InvalidUrl
    );
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::llm_config::clear_user_overrides();
        }
    }
    let _reset = Reset;
    let mut project = ProvidersConfig::default();
    project.providers.insert(
        "acp-endpoint-fixture".into(),
        crate::llm_config::ProviderDef {
            protocol: Some("acp".into()),
            base_url: "not-an-http-endpoint".into(),
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(project));
    let route = request(
        "acp-endpoint-fixture",
        "fixture-model",
        super::super::InferenceReach::AnyHosted,
    );
    let policy = preview_inference_admission(&route);
    let checked = crate::llm::api::preflight_inference_admission(&route).unwrap();
    assert_eq!(checked.status, policy.status);
    assert_eq!(checked.governing_rule, policy.governing_rule);
}

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
fn regional_training_fact_does_not_admit_unverified_endpoints() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let _region = ScopedEnvVar::remove("ZAI_REGION");
    let _endpoint = ScopedEnvVar::remove("ZAI_BASE_URL");
    let route = request("zai", "glm-5.3", super::super::InferenceReach::AnyHosted);
    let global = preview_inference_admission(&route);
    assert_eq!(global.status, InferenceAdmissionStatus::Admitted);
    assert_eq!(global.training_default.as_deref(), Some("does_not_train"));
    let strict = preview_inference_admission(&InferenceAdmissionRequest {
        data_controls: Some(crate::llm_config::DataPosture::StrictestAvailable),
        ..route.clone()
    });
    assert_eq!(strict.status, InferenceAdmissionStatus::Admitted);
    assert_eq!(
        strict.data_controls_outcome,
        Some(DataControlsOutcome::ProviderUnresearched)
    );
    {
        let _cn = ScopedEnvVar::set("ZAI_REGION", "cn");
        let snapshot = preview_inference_admission(&route);
        assert_eq!(snapshot.status, InferenceAdmissionStatus::Unknown);
        assert_eq!(
            snapshot.governing_rule.as_deref(),
            Some("inference_boundary.training_unknown")
        );
        assert_eq!(snapshot.training_default, None);
    }
    for endpoint in [
        "https://api.z.ai.evil.example/api/paas/v4",
        "https://api.z.ai/api/paas/v4/",
        "https://api.z.ai/api/paas/v4/other",
        "http://api.z.ai/api/paas/v4",
    ] {
        let _override = ScopedEnvVar::set("ZAI_BASE_URL", endpoint);
        let snapshot = preview_inference_admission(&route);
        assert_eq!(snapshot.status, InferenceAdmissionStatus::Unknown);
        assert_eq!(
            snapshot.governing_rule.as_deref(),
            Some("inference_boundary.training_unknown")
        );
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
    assert_eq!(open.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(
        open.governing_rule.as_deref(),
        Some("inference_boundary.training_unknown")
    );
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
            crate::llm_config::clear_runtime_catalog_overlay();
        }
    }
    let _reset = Reset;
    // Synthetic host catalog declarations exercise the plan owner without
    // claiming these controls are supported by Meta or Gemini's actual service.
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
            control.applies_to = vec![crate::llm_config::DataControlDialect::OpenAiSse];
        }
        overlay.providers.insert(provider.into(), definition);
    }
    crate::llm_config::set_runtime_catalog_overlay(Some(overlay));
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

fn unbounded(
    provider: &str,
    model: &str,
    posture: Option<DataPosture>,
) -> InferenceAdmissionRequest {
    InferenceAdmissionRequest {
        provider: provider.into(),
        model: model.into(),
        boundary: None,
        data_controls: posture,
    }
}

#[test]
fn preview_reports_the_posture_inference_will_send_and_the_routes_own_note() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    // No posture named: the catalog's resolved default is what inference sends.
    let standard = preview_inference_admission(&unbounded("meta", "muse-spark-1.3", None));
    assert_eq!(
        standard.data_posture,
        crate::llm_config::data_controls_default_posture()
    );
    assert_eq!(
        standard.data_controls_outcome,
        Some(DataControlsOutcome::NotRequested)
    );
    let provider_note = standard.data_controls_note.expect("meta declares a note");
    // A route whose model row declares its own handling reports that row, not
    // the provider line: the provider says it does not train, this route does.
    let contributor =
        preview_inference_admission(&unbounded("meta", "muse-spark-1.3-contributor", None));
    assert_eq!(contributor.training_default.as_deref(), Some("trains"));
    let route_note = contributor
        .data_controls_note
        .expect("the route declares a note");
    assert_ne!(route_note, provider_note);
    assert!(route_note.contains("train"), "{route_note}");
    // Same posture the per-request receipt reports for the route.
    let receipt = super::super::chat_controls(
        "meta",
        "muse-spark-1.3-contributor",
        crate::llm_config::data_controls_default_posture(),
        None,
        crate::llm::api::LlmApiMode::ChatCompletions,
        None,
    )
    .receipt;
    assert_eq!(
        serde_json::to_value(contributor.data_posture).unwrap(),
        serde_json::Value::String(receipt.requested_posture)
    );
}

#[test]
fn preview_refuses_a_route_the_strict_posture_cannot_honor() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let strict = Some(DataPosture::StrictestAvailable);
    let refused =
        preview_inference_admission(&unbounded("meta", "muse-spark-1.3-contributor", strict));
    assert_eq!(refused.status, InferenceAdmissionStatus::Denied);
    assert_eq!(
        refused.governing_rule.as_deref(),
        Some(DATA_CONTROLS_TRAINING_REFUSED_RULE)
    );
    assert_eq!(refused.data_posture, DataPosture::StrictestAvailable);
    assert_eq!(
        refused.data_controls_refusal,
        Some(DataControlsRefusal::TrainingWithoutControl)
    );
    // Negative control: the same posture on the route that does not train is
    // admitted, and the default posture admits the training route.
    let admitted = preview_inference_admission(&unbounded("meta", "muse-spark-1.3", strict));
    assert_eq!(admitted.status, InferenceAdmissionStatus::Admitted);
    assert_eq!(admitted.data_controls_refusal, None);
    let default = preview_inference_admission(&unbounded(
        "meta",
        "muse-spark-1.3-contributor",
        Some(DataPosture::Default),
    ));
    assert_eq!(default.status, InferenceAdmissionStatus::Admitted);
    assert_eq!(default.data_posture, DataPosture::Default);
}

#[test]
fn an_unresolved_route_reports_no_data_controls_outcome() {
    let _guard = crate::llm::env_guard();
    let _host = ScopedEnvVar::remove(super::super::HOST_BOUNDARY_ENV);
    let unknown = preview_inference_admission(&unbounded("not-a-catalog-provider", "model", None));
    assert_eq!(unknown.status, InferenceAdmissionStatus::Unknown);
    assert_eq!(unknown.data_controls_outcome, None);
    assert!(!serde_json::to_value(&unknown)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("data_controls_outcome"));
}
