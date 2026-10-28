//! Inference boundary checks on the real loopback HTTP path.
use super::*;

#[test]
fn project_privacy_facts_and_endpoint_cannot_widen_host_ceiling() {
    use crate::llm::api::{
        preview_inference_admission, InferenceAdmissionRequest, InferenceAdmissionStatus,
    };
    use crate::llm_config::{
        DataControlDef, DataControlDialect, DataControlEffect, DataControlLocation,
        DataControlValue, ProvidersConfig, TrainingDefault,
    };
    use std::io::Write;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct ClearTrustedCatalog;
    impl Drop for ClearTrustedCatalog {
        fn drop(&mut self) {
            crate::llm_config::clear_runtime_catalog_overlay();
        }
    }
    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let _cleanup = PrivacyOverrides;
    let _trusted_cleanup = ClearTrustedCatalog;
    let _host = ScopedEnvVar::remove(super::super::inference_boundary::HOST_BOUNDARY_ENV);
    let count = Arc::new(AtomicUsize::new(0));
    let reached = count.clone();
    let models = Arc::new(Mutex::new(Vec::new()));
    let sent_models = models.clone();
    let server = spawn_llm_stub_many("host privacy authority", 2, move |_, stream| {
        let (_, body) = super::ollama_openai_compat::read_http_request(stream);
        sent_models.lock().unwrap().push(body["model"].clone());
        reached.fetch_add(1, Ordering::SeqCst);
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"accepted"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#;
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let redirected_count = Arc::new(AtomicUsize::new(0));
    let redirected_reached = redirected_count.clone();
    let redirected = spawn_llm_stub("project redirect", move |_| {
        redirected_reached.fetch_add(1, Ordering::SeqCst);
    });
    let mut provider = crate::llm_config::provider_config("openai").unwrap();
    provider.base_url = format!("http://{}/v1", server.addr());
    provider.base_url_env = None;
    provider.data_controls.as_mut().unwrap().training_default = TrainingDefault::DoesNotTrain;
    let mut host = ProvidersConfig::default();
    host.providers.insert("openai".into(), provider.clone());
    crate::llm_config::set_runtime_catalog_overlay(Some(host));
    let boundary = super::super::inference_boundary::InferenceBoundary {
        reach: super::super::inference_boundary::InferenceReach::AnyHosted,
        allow_training_discounts: false,
    };
    let mut opts = base_opts("openai");
    opts.model = "gpt-5.6-luna".into();
    opts.api_key = "synthetic".into();
    opts.stream = false;
    opts.inference_boundary = Some(boundary);
    let preview = || {
        preview_inference_admission(&InferenceAdmissionRequest {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            boundary: Some(boundary),
            data_controls: None,
        })
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(preview().status, InferenceAdmissionStatus::Admitted);
    assert_eq!(
        runtime.block_on(vm_call_llm_full(&opts)).unwrap().text,
        "accepted"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "positive control reached the host endpoint"
    );
    let mut overridden = opts.clone();
    overridden.provider_overrides = Some(serde_json::json!({"model": "project-overridden-model"}));
    assert!(
        runtime.block_on(vm_call_llm_full(&overridden)).is_err(),
        "model override must not change the admitted route; sent {:?}",
        models.lock().unwrap()
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "model override sent no request"
    );

    let mut project_provider = provider.clone();
    project_provider
        .data_controls
        .as_mut()
        .unwrap()
        .training_default = TrainingDefault::Trains;
    let mut project_model = crate::llm_config::model_catalog_entry(&opts.model).unwrap();
    project_model.data_controls = Some(crate::llm_config::ModelDataControlsDef {
        training_default: TrainingDefault::DoesNotTrain,
        retention_default: None,
        checked_on: "2026-10-27".into(),
        sources: vec!["https://example.invalid/model-claim".into()],
        note: None,
    });
    let mut conflicting = ProvidersConfig::default();
    conflicting
        .providers
        .insert("openai".into(), project_provider);
    conflicting.models.insert(opts.model.clone(), project_model);
    crate::llm_config::set_user_overrides(Some(conflicting));
    assert_eq!(
        crate::llm_config::effective_training_default("openai", &opts.model),
        Some(TrainingDefault::DoesNotTrain),
        "the model claim must actually mask the provider row in the merged catalog"
    );
    assert_eq!(preview().status, InferenceAdmissionStatus::Denied);
    let refusal = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("a model claim cannot erase the project's stricter provider restriction");
    assert!(
        refusal
            .to_string()
            .contains("inference_boundary.training_default"),
        "{refusal}"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "conflicting project rows sent no request"
    );
    crate::llm_config::clear_user_overrides();

    let mut project = provider.clone();
    project.base_url = format!("http://{}/v1", redirected.addr());
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert("openai".into(), project);
    crate::llm_config::set_user_overrides(Some(overlay));
    assert_eq!(
        preview().status,
        InferenceAdmissionStatus::Unknown,
        "redirect cannot inherit the host endpoint's promise"
    );
    assert!(runtime.block_on(vm_call_llm_full(&opts)).is_err());
    assert_eq!(
        redirected_count.load(Ordering::SeqCst),
        0,
        "redirect sent no request"
    );

    crate::llm_config::clear_user_overrides();
    provider.data_controls.as_mut().unwrap().training_default = TrainingDefault::Unspecified;
    let mut host = ProvidersConfig::default();
    host.providers.insert("openai".into(), provider.clone());
    crate::llm_config::set_runtime_catalog_overlay(Some(host));
    assert_eq!(
        preview().status,
        InferenceAdmissionStatus::Unknown,
        "host has no no-training promise"
    );
    provider.data_controls.as_mut().unwrap().training_default = TrainingDefault::DoesNotTrain;
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert("openai".into(), provider);
    crate::llm_config::set_user_overrides(Some(overlay));
    assert_eq!(
        preview().status,
        InferenceAdmissionStatus::Unknown,
        "project cannot invent a no-training promise"
    );
    assert!(runtime.block_on(vm_call_llm_full(&opts)).is_err());
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "forged fact sent no request"
    );

    crate::llm_config::clear_user_overrides();
    let mut fake_control = crate::llm_config::provider_config("openai").unwrap();
    fake_control
        .data_controls
        .as_mut()
        .unwrap()
        .request_controls
        .push(DataControlDef {
            location: DataControlLocation::Body,
            name: "invented_no_training".into(),
            value: DataControlValue::Bool(true),
            effect: DataControlEffect::Training,
            applies_to: vec![DataControlDialect::OpenAiSse],
            caveat: None,
        });
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert("openai".into(), fake_control);
    crate::llm_config::set_user_overrides(Some(overlay));
    let decision = preview();
    assert_eq!(decision.status, InferenceAdmissionStatus::Unknown);
    assert!(
        !decision.training_control_planned,
        "project control is not host evidence"
    );
    assert!(runtime.block_on(vm_call_llm_full(&opts)).is_err());
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "invented control sent no request"
    );

    crate::llm_config::clear_user_overrides();
    let mut trusted_provider = crate::llm_config::provider_config("openai").unwrap();
    trusted_provider
        .data_controls
        .as_mut()
        .unwrap()
        .training_default = TrainingDefault::DoesNotTrain;
    let mut host = ProvidersConfig::default();
    host.providers.insert("openai".into(), trusted_provider);
    crate::llm_config::set_runtime_catalog_overlay(Some(host));
    assert_eq!(
        preview().status,
        InferenceAdmissionStatus::Admitted,
        "trusted model remains admissible"
    );
    let mut remapped =
        crate::llm_config::model_catalog_entry_for_route("openai", &opts.model).unwrap();
    remapped.wire_model = Some("project-remapped-model".into());
    let mut project = ProvidersConfig::default();
    project.models.insert(opts.model.clone(), remapped);
    crate::llm_config::set_user_overrides(Some(project));
    assert_eq!(
        preview().status,
        InferenceAdmissionStatus::Unknown,
        "project cannot attach trusted facts to a different wire model"
    );
    assert!(runtime.block_on(vm_call_llm_full(&opts)).is_err());
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "remapped model sent no request"
    );

    crate::llm_config::clear_user_overrides();
    let open_weight = super::super::inference_boundary::InferenceBoundary {
        reach: super::super::inference_boundary::InferenceReach::HostedOpenWeight,
        allow_training_discounts: true,
    };
    let weight_request = InferenceAdmissionRequest {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        boundary: Some(open_weight),
        data_controls: None,
    };
    assert_ne!(
        preview_inference_admission(&weight_request).status,
        InferenceAdmissionStatus::Admitted
    );
    let mut model =
        crate::llm_config::model_catalog_entry_for_route("openai", &opts.model).unwrap();
    model.open_weight = Some(true);
    let mut project = ProvidersConfig::default();
    project.models.insert(opts.model.clone(), model);
    crate::llm_config::set_user_overrides(Some(project));
    assert_ne!(
        preview_inference_admission(&weight_request).status,
        InferenceAdmissionStatus::Admitted,
        "project cannot invent open-weight evidence"
    );

    crate::llm_config::clear_user_overrides();
    let mut project_provider = crate::llm_config::provider_config("openai").unwrap();
    project_provider.local_runtime = crate::llm_config::provider_config("ollama")
        .unwrap()
        .local_runtime;
    let mut project = ProvidersConfig::default();
    project.providers.insert("openai".into(), project_provider);
    crate::llm_config::set_user_overrides(Some(project));
    let local_request = InferenceAdmissionRequest {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        boundary: Some(super::super::inference_boundary::InferenceBoundary {
            reach: super::super::inference_boundary::InferenceReach::LocalOnly,
            allow_training_discounts: false,
        }),
        data_controls: None,
    };
    assert_ne!(
        preview_inference_admission(&local_request).status,
        InferenceAdmissionStatus::Admitted,
        "project cannot relabel a hosted provider as local"
    );
}

#[test]
fn unverified_endpoint_is_refused_before_transport() {
    use crate::llm::api::{
        preview_inference_admission, InferenceAdmissionRequest, InferenceAdmissionStatus,
    };
    use crate::llm_config::RuntimeProviderEndpointOverrides;
    use std::io::Write;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let _cleanup = PrivacyOverrides;
    let _host = ScopedEnvVar::remove(super::super::inference_boundary::HOST_BOUNDARY_ENV);
    let count = Arc::new(AtomicUsize::new(0));
    let reached = count.clone();
    let server = spawn_llm_stub_many("endpoint privacy", 2, move |_, stream| {
        super::ollama_openai_compat::read_http_request(stream);
        reached.fetch_add(1, Ordering::SeqCst);
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"accepted"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#;
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let endpoint = format!("http://{}/v1", server.addr());
    let _trusted_endpoint = TrustedTestEndpoint::install("openai", &endpoint);
    let mut overrides = RuntimeProviderEndpointOverrides::single("openai", &endpoint).unwrap();
    overrides.insert("zai", &endpoint).unwrap();
    crate::llm_config::set_runtime_provider_endpoint_overrides(overrides);
    let boundary = super::super::inference_boundary::InferenceBoundary {
        reach: super::super::inference_boundary::InferenceReach::AnyHosted,
        allow_training_discounts: false,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut allowed = base_opts("openai");
    allowed.model = "gpt-5.6-luna".into();
    allowed.api_key = "synthetic".into();
    allowed.stream = false;
    allowed.inference_boundary = Some(boundary);
    assert_eq!(
        runtime.block_on(vm_call_llm_full(&allowed)).unwrap().text,
        "accepted"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let mut denied = base_opts("zai");
    denied.model = "glm-5.3".into();
    denied.api_key = "synthetic".into();
    denied.stream = false;
    denied.inference_boundary = Some(boundary);
    let preview = preview_inference_admission(&InferenceAdmissionRequest {
        provider: denied.provider.clone(),
        model: denied.model.clone(),
        boundary: Some(boundary),
        data_controls: None,
    });
    assert_eq!(preview.status, InferenceAdmissionStatus::Unknown);
    let error = runtime.block_on(vm_call_llm_full(&denied)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("inference_boundary.training_unknown"),
        "{error}"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "the refused route sent nothing"
    );
}

#[test]
fn no_training_ceiling_plans_and_sends_only_required_controls() {
    use crate::llm::api::{
        preview_inference_admission, InferenceAdmissionRequest, InferenceAdmissionStatus,
    };
    use crate::llm_config::{DataPosture, RuntimeProviderEndpointOverrides};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let _cleanup = PrivacyOverrides;
    let counter = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let requests = counter.clone();
    let bodies = captured.clone();
    let server = spawn_llm_stub_many("privacy controls", 5, move |_, stream| {
        let (_, body) = super::ollama_openai_compat::read_http_request(stream);
        let responses = body.get("input").is_some();
        bodies.lock().unwrap().push(body);
        requests.fetch_add(1, Ordering::SeqCst);
        let body = if responses {
            r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"accepted"}]}],"usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}}"#
        } else {
            r#"{"choices":[{"message":{"role":"assistant","content":"accepted"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#
        };
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let endpoint = format!("http://{}", server.addr());
    let _trusted_endpoint = TrustedTestEndpoint::install("openrouter", &endpoint);
    crate::llm_config::set_runtime_provider_endpoint_overrides(
        RuntimeProviderEndpointOverrides::single("openrouter", endpoint).unwrap(),
    );
    let boundary = super::super::inference_boundary::InferenceBoundary {
        reach: super::super::inference_boundary::InferenceReach::AnyHosted,
        allow_training_discounts: false,
    };
    let _host_boundary = ScopedEnvVar::set(
        super::super::inference_boundary::HOST_BOUNDARY_ENV,
        serde_json::to_string(&boundary).unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut opts = base_opts("openrouter");
    opts.model = "google/gemini-3.5-flash-lite".into();
    opts.api_key = "invalid-test-key".into();
    opts.stream = false;
    opts.inference_boundary = None;
    for posture in [
        None,
        Some(DataPosture::Default),
        Some(DataPosture::StrictestAvailable),
    ] {
        opts.data_controls = posture;
        let before = counter.load(Ordering::SeqCst);
        let preview = preview_inference_admission(&InferenceAdmissionRequest {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            boundary: opts.inference_boundary,
            data_controls: posture,
        });
        assert_eq!(preview.status, InferenceAdmissionStatus::Admitted);
        assert!(preview.training_control_planned);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            before,
            "preview never sends"
        );
        let result = runtime
            .block_on(vm_call_llm_full(&opts))
            .expect("required training control admits transport");
        assert_eq!(result.text, "accepted");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            before + 1,
            "actual same listener reached"
        );
        let receipt = result.telemetry.data_controls.unwrap();
        assert_eq!(receipt.inference_boundary_rule, preview.governing_rule);
        assert_eq!(receipt.outcome, preview.data_controls_outcome.unwrap());
        let bodies = captured.lock().unwrap();
        let body = bodies.last().unwrap();
        assert_eq!(body["provider"]["data_collection"], "deny");
        if posture == Some(DataPosture::StrictestAvailable) {
            assert_eq!(body["provider"]["zdr"], true);
        } else {
            assert!(
                body["provider"].get("zdr").is_none(),
                "training ceiling adds no retention restriction"
            );
            assert_eq!(receipt.applied.len(), 1);
        }
    }

    for override_field in [
        serde_json::json!({"models": ["other/closed-model"]}),
        serde_json::json!({"fallbacks": [{"model": "other/closed-model"}]}),
    ] {
        opts.provider_overrides = Some(override_field);
        assert!(
            runtime.block_on(vm_call_llm_full(&opts)).is_err(),
            "bounded route must not delegate to an unadmitted fallback model"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "fallback override sent no request"
        );
    }
    opts.provider_overrides = None;

    let _without_host = ScopedEnvVar::remove(super::super::inference_boundary::HOST_BOUNDARY_ENV);
    opts.data_controls = Some(DataPosture::Default);
    let result = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect("unbounded default still works");
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    assert_eq!(
        result.telemetry.data_controls.unwrap().outcome,
        super::super::data_controls::DataControlsOutcome::NotRequested
    );
    let bodies = captured.lock().unwrap();
    let provider = &bodies.last().unwrap()["provider"];
    assert!(provider.get("data_collection").is_none());
    assert!(provider.get("zdr").is_none());
    drop(bodies);
    drop(_without_host);

    opts.inference_boundary = Some(boundary);
    let mut responses = crate::llm_config::provider_config("openrouter").unwrap();
    responses.features.push("responses_api".into());
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert("openrouter".into(), responses);
    crate::llm_config::set_user_overrides(Some(overlay));
    opts.api_mode = super::super::LlmApiMode::Responses;
    assert!(crate::llm_config::provider_has_feature(
        "openrouter",
        "responses_api"
    ));
    assert!(crate::llm::capabilities::should_use_responses_transport(
        &opts.provider,
        &opts.model,
        true
    ));
    let preflight = super::super::inference_boundary::preflight_chat(
        &super::super::LlmRequestPayload::from(&opts),
    );
    let result = runtime.block_on(vm_call_llm_full(&opts));
    assert_eq!(
        counter.load(Ordering::SeqCst),
        4,
        "Responses preflight_admitted={}, response_accepted={}, transmitted_without_training_control={}",
        preflight.is_ok(),
        result.as_ref().is_ok_and(|result| result.text == "accepted"),
        captured.lock().unwrap().last().unwrap()["provider"]
            .get("data_collection")
            .is_none()
    );
    assert!(preflight.is_err());
    assert!(result
        .expect_err("Responses cannot credit an unapplied training control")
        .to_string()
        .contains("inference_boundary.training_unknown"));
    opts.api_mode = super::super::LlmApiMode::ChatCompletions;
    crate::llm_config::clear_user_overrides();
    let mut definition = crate::llm_config::provider_config("openrouter").unwrap();
    for retention_only in [true, false] {
        if retention_only {
            definition
                .data_controls
                .as_mut()
                .unwrap()
                .request_controls
                .retain(|control| control.effect != crate::llm_config::DataControlEffect::Training);
        } else {
            definition
                .data_controls
                .as_mut()
                .unwrap()
                .request_controls
                .clear();
        }
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay
            .providers
            .insert("openrouter".into(), definition.clone());
        crate::llm_config::set_user_overrides(Some(overlay));
        let preview = preview_inference_admission(&InferenceAdmissionRequest {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            boundary: opts.inference_boundary,
            data_controls: opts.data_controls,
        });
        assert_eq!(preview.status, InferenceAdmissionStatus::Unknown);
        assert!(!preview.training_control_planned);
        let error = runtime
            .block_on(vm_call_llm_full(&opts))
            .expect_err("unknown without training control refuses");
        assert!(error
            .to_string()
            .contains("inference_boundary.training_unknown"));
        assert_eq!(
            counter.load(Ordering::SeqCst),
            4,
            "positive-controlled counter proves refusal before transport"
        );
    }
    crate::llm_config::clear_user_overrides();
    let mut native = crate::llm_config::provider_config("gemini").unwrap();
    native.data_controls = crate::llm_config::provider_config("openrouter")
        .unwrap()
        .data_controls;
    for control in &mut native.data_controls.as_mut().unwrap().request_controls {
        control.applies_to.clear();
    }
    let mut overlay = crate::llm_config::ProvidersConfig::default();
    overlay.providers.insert("gemini".into(), native);
    crate::llm_config::set_user_overrides(Some(overlay));
    opts.provider = "gemini".into();
    opts.model = "gemini-3.5-flash-lite".into();
    opts.data_controls = Some(DataPosture::StrictestAvailable);
    let preview = preview_inference_admission(&InferenceAdmissionRequest {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        boundary: opts.inference_boundary,
        data_controls: opts.data_controls,
    });
    assert_eq!(preview.status, InferenceAdmissionStatus::Unknown);
    assert!(
        !preview.training_control_planned,
        "native adapter cannot apply declared controls"
    );
    let error = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect_err("native cannot credit unapplied controls");
    assert!(error
        .to_string()
        .contains("inference_boundary.training_unknown"));
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    crate::llm_config::clear_user_overrides();
    opts.provider = "openrouter".into();
    opts.model = "google/gemini-3.5-flash-lite".into();
    opts.data_controls = Some(DataPosture::Default);
    opts.inference_boundary = None;
    let result = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect("same listener remains live after refusals");
    assert_eq!(result.text, "accepted");
    assert_eq!(counter.load(Ordering::SeqCst), 5);
}

/// The listener stands in for the catalog endpoint, not a project redirect.
struct TrustedTestEndpoint;

impl TrustedTestEndpoint {
    fn install(provider: &str, endpoint: &str) -> Self {
        let mut definition = crate::llm_config::provider_config(provider).unwrap();
        definition.base_url = endpoint.to_string();
        let mut trusted = crate::llm_config::ProvidersConfig::default();
        trusted.providers.insert(provider.to_string(), definition);
        crate::llm_config::set_runtime_catalog_overlay(Some(trusted));
        Self
    }
}

impl Drop for TrustedTestEndpoint {
    fn drop(&mut self) {
        crate::llm_config::clear_runtime_catalog_overlay();
    }
}

struct PrivacyOverrides;

impl Drop for PrivacyOverrides {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
    }
}

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

#[test]
fn admission_preview_never_sends_and_the_same_listener_has_a_non_null_control() {
    use crate::llm::api::{
        preview_inference_admission, InferenceAdmissionRequest, InferenceAdmissionStatus,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let counter = Arc::new(AtomicUsize::new(0));
    let server = spawn_ollama_empty_then_success_stub(counter.clone());
    let _endpoint = ScopedEnvVar::set("OLLAMA_HOST", format!("http://{}", server.addr()));
    let _boundary = ScopedEnvVar::set(
        super::super::inference_boundary::HOST_BOUNDARY_ENV,
        r#"{"reach":"local_only","allow_training_discounts":false}"#,
    );
    let opts = base_opts("ollama");
    let request = InferenceAdmissionRequest {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        boundary: None,
        data_controls: None,
    };
    assert_eq!(
        preview_inference_admission(&request).status,
        InferenceAdmissionStatus::Admitted
    );
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime
        .block_on(vm_call_llm_full(&opts))
        .expect("the real route must reach the listener");
    assert_eq!(result.text, "retried");
    let observed = counter.load(Ordering::SeqCst);
    assert!(
        observed > 0,
        "a measured zero needs the same counter's non-null control"
    );
    assert_eq!(
        preview_inference_admission(&request).status,
        InferenceAdmissionStatus::Admitted
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        observed,
        "preview must not send another request"
    );
}
