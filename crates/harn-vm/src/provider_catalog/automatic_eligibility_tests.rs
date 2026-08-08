use super::*;

use crate::llm_config::ModelEligibilityMeasurementKind;

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

#[test]
fn generated_catalog_exports_one_measured_fireworks_automatic_default() {
    let catalog = artifact();
    let automatic = catalog
        .variants
        .iter()
        .filter(|variant| variant.automatic_eligibility.is_some())
        .collect::<Vec<_>>();
    assert_eq!(automatic.len(), 1, "catalog must own one automatic default");
    assert_eq!(automatic[0].id, "balanced");
    assert_eq!(automatic[0].provider, "fireworks");
    assert_eq!(
        automatic[0].model_id,
        "accounts/fireworks/models/gpt-oss-120b"
    );
    let receipts = &automatic[0]
        .automatic_eligibility
        .as_ref()
        .expect("automatic eligibility")
        .receipts;
    assert_eq!(receipts.len(), 3);
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| (receipt.kind, receipt.passed, receipt.trials))
            .collect::<Vec<_>>(),
        vec![
            (ModelEligibilityMeasurementKind::MeterHoldout, 80, 100),
            (ModelEligibilityMeasurementKind::ToolCallFidelity, 97, 100),
            (ModelEligibilityMeasurementKind::ProviderHealth, 99, 100),
        ]
    );
}

#[test]
fn validation_rejects_unmeasured_or_ambiguous_automatic_variants() {
    let mut catalog = artifact();
    let eligibility = catalog
        .variants
        .iter()
        .find_map(|variant| variant.automatic_eligibility.clone())
        .expect("shipped automatic eligibility");
    catalog.variants[0].automatic_eligibility = Some(eligibility);
    let report = validate_artifact(&catalog);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("automatic model variants; exactly zero or one is allowed")));

    let automatic = catalog
        .variants
        .iter_mut()
        .find(|variant| variant.id == "balanced")
        .expect("balanced variant");
    automatic
        .automatic_eligibility
        .as_mut()
        .expect("automatic eligibility")
        .receipts[0]
        .trials = 0;
    let report = validate_artifact(&catalog);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("zero-trial measurement receipt")));

    let mut missing_health = artifact();
    missing_health
        .variants
        .iter_mut()
        .find(|variant| variant.id == "balanced")
        .expect("balanced variant")
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
