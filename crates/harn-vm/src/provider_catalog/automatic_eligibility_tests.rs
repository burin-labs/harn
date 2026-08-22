use super::*;

use crate::llm_config::ModelEligibilityMeasurementKind;

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
