use super::*;

use crate::config::{
    AutomaticModelEligibility, AutomaticModelEligibilityDecision, AutomaticModelEligibilitySchema,
    ModelEligibilityMeasurement, ModelEligibilityMeasurementKind,
};

#[test]
fn schema_id_tracks_schema_version() {
    assert_eq!(
        PROVIDER_CATALOG_SCHEMA_ID,
        format!(
            "https://harnlang.com/schemas/provider-catalog.v{PROVIDER_CATALOG_SCHEMA_VERSION}.json"
        )
    );
}

#[test]
fn downstream_bindings_include_automatic_eligibility_shape() {
    let typescript = typescript_declarations();
    assert!(typescript.contains("automatic_eligibility?: HarnAutomaticModelEligibility"));
    assert!(typescript
        .contains("kind: \"meter_holdout\" | \"tool_call_fidelity\" | \"provider_health\""));

    let swift = swift_binding().expect("swift binding renders");
    assert!(swift.contains("public let automaticEligibility: HarnAutomaticModelEligibility?"));
    assert!(swift.contains("public struct HarnModelEligibilityMeasurement"));
}

fn catalog_with_test_eligibility() -> ProviderCatalogArtifact {
    let mut catalog = artifact();
    // Validation fixtures must not depend on a measured production default.
    for variant in &mut catalog.variants {
        variant.automatic_eligibility = None;
    }
    catalog.variants[0].automatic_eligibility = Some(AutomaticModelEligibility {
        schema: AutomaticModelEligibilitySchema::V1,
        decision: AutomaticModelEligibilityDecision::Eligible,
        receipts: [
            ModelEligibilityMeasurementKind::MeterHoldout,
            ModelEligibilityMeasurementKind::ToolCallFidelity,
            ModelEligibilityMeasurementKind::ProviderHealth,
        ]
        .into_iter()
        .map(|kind| ModelEligibilityMeasurement {
            kind,
            source: "test-only://automatic-eligibility".to_string(),
            observed_at: "2026-11-07".to_string(),
            harn_version: "v0.10.0".to_string(),
            passed: 100,
            trials: 100,
        })
        .collect(),
    });
    let report = validate_artifact(&catalog);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    catalog
}

#[test]
fn validation_rejects_ambiguous_automatic_variants() {
    let mut catalog = catalog_with_test_eligibility();
    let eligibility = catalog.variants[0].automatic_eligibility.clone();
    catalog.variants[1].automatic_eligibility = eligibility;
    let report = validate_artifact(&catalog);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("automatic model variants; exactly zero or one is allowed")));
}

#[test]
fn validation_rejects_zero_trial_automatic_measurements() {
    let mut catalog = catalog_with_test_eligibility();
    let receipt = &mut catalog.variants[0]
        .automatic_eligibility
        .as_mut()
        .expect("automatic eligibility")
        .receipts[0];
    receipt.passed = 0;
    receipt.trials = 0;
    let report = validate_artifact(&catalog);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("zero-trial measurement receipt")));
}

#[test]
fn validation_rejects_automatic_eligibility_without_health_receipt() {
    let mut missing_health = catalog_with_test_eligibility();
    missing_health.variants[0]
        .automatic_eligibility
        .as_mut()
        .expect("automatic eligibility")
        .receipts
        .retain(|receipt| receipt.kind != ModelEligibilityMeasurementKind::ProviderHealth);
    let report = validate_artifact(&missing_health);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("must carry a provider_health receipt")));
}
