use super::*;
use crate::llm::api::InferenceReach;

fn request(reach: InferenceReach) -> ManagedSupplyRequest {
    ManagedSupplyRequest {
        version: MANAGED_SUPPLY_VERSION,
        logical_route: ManagedSupplyLogicalRoute {
            provider: "mistral".into(),
            model: "mistral-large-2512".into(),
            capability_fingerprint: capability_fingerprint("mistral", "mistral-large-2512"),
        },
        inference_boundary: InferenceBoundary {
            reach,
            allow_training_discounts: false,
        },
    }
}

#[test]
fn physical_weight_class_is_admitted_independently_of_matching_capabilities() {
    let _guard = crate::llm::env_guard();
    struct ClearCatalog;
    impl Drop for ClearCatalog {
        fn drop(&mut self) {
            crate::llm_config::clear_runtime_catalog_overlay();
        }
    }
    let _clear_catalog = ClearCatalog;
    assert_eq!(
        capability_fingerprint("mistral", "mistral-large-2512"),
        capability_fingerprint("mistral", "codestral-2508"),
        "the negative must reach beyond capability compatibility"
    );
    let open = request(InferenceReach::HostedOpenWeight);
    let unknown = compatible_served_route(&open, "mistral", "mistral-large-2512")
        .expect_err("unconfirmed account must not imply no training");
    assert_eq!(unknown.code(), "inference_boundary.training_unknown");
    let mut mistral = crate::llm_config::provider_config("mistral").unwrap();
    mistral.data_controls.as_mut().unwrap().training_default =
        crate::llm_config::TrainingDefault::DoesNotTrain;
    let mut fixture = crate::llm_config::ProvidersConfig::default();
    fixture.providers.insert("mistral".into(), mistral);
    crate::llm_config::set_runtime_catalog_overlay(Some(fixture));
    compatible_served_route(&open, "mistral", "mistral-large-2512")
        .expect("open-weight supply with a trusted no-training fixture");
    let refusal = compatible_served_route(&open, "mistral", "codestral-2508")
        .expect_err("matching capabilities do not grant closed-weight supply");
    assert_eq!(refusal.code(), "inference_boundary.hosted_open_weight");
    assert!(refusal.is_policy_denial());
    compatible_served_route(
        &request(InferenceReach::AnyHosted),
        "mistral",
        "codestral-2508",
    )
    .expect("explicit hosted reach allows the trusted no-training closed fixture");
    let refusal = compatible_served_route(
        &request(InferenceReach::LocalOnly),
        "mistral",
        "mistral-large-2512",
    )
    .expect_err("local-only reach never grants hosted supply");
    assert_eq!(refusal.code(), "inference_boundary.local_only");
}

#[test]
fn request_authority_is_required_closed_and_versioned() {
    let valid = serde_json::to_value(request(InferenceReach::HostedOpenWeight)).unwrap();
    let mut missing = valid.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("inference_boundary");
    assert!(serde_json::from_value::<ManagedSupplyRequest>(missing).is_err());
    for malformed in [
        serde_json::Value::Null,
        serde_json::json!({"reach":"any_hosted"}),
        serde_json::json!({"reach":"unknown", "allow_training_discounts":false}),
        serde_json::json!({"reach":"any_hosted", "allow_training_discounts":"yes"}),
        serde_json::json!({"reach":"any_hosted", "allow_training_discounts":false, "grant":true}),
    ] {
        let mut raw = valid.clone();
        raw["inference_boundary"] = malformed;
        assert!(serde_json::from_value::<ManagedSupplyRequest>(raw).is_err());
    }
    let mut obsolete = request(InferenceReach::AnyHosted);
    obsolete.version = 1;
    let error = validate_request(&obsolete).expect_err("version 1 authority is unsupported");
    assert!(!error.is_policy_denial());
    assert_eq!(error.code(), "invalid_managed_supply_contract");
}

#[test]
fn unknown_weight_and_training_facts_never_become_default_grants() {
    let _guard = crate::llm::env_guard();
    let boundary = request(InferenceReach::HostedOpenWeight).inference_boundary;
    let (model, catalog) = crate::llm_config::model_catalog_entries()
        .into_iter()
        .find(|(_, row)| {
            row.open_weight.is_none()
                && crate::llm_config::provider_config(&row.provider)
                    .is_some_and(|provider| provider.local_runtime.is_none())
        })
        .expect("a real cataloged hosted model with unverified weight class");
    assert_eq!(
        crate::llm_config::model_catalog_entry(&model)
            .unwrap()
            .open_weight,
        None,
        "the negative reaches a cataloged model with unverified weight class"
    );
    let error = validate_served_admission(boundary, &catalog.provider, &model).unwrap_err();
    assert_eq!(error.code(), "inference_boundary.hosted_open_weight");
    let hosted = request(InferenceReach::AnyHosted).inference_boundary;
    let error = validate_served_admission(hosted, "deepseek", "deepseek-flash").unwrap_err();
    assert_eq!(error.code(), "inference_boundary.training_default");
    let error =
        validate_served_admission(hosted, "dashscope", "dashscope/qwen3-coder-next").unwrap_err();
    assert_eq!(error.code(), "inference_boundary.training_unknown");
}
