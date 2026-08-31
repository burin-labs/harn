//! The registry's totality gate, and the controls that keep it from passing
//! vacuously or in the wrong direction.

use super::*;

const AS_OF: &str = "2026-08-30";

#[test]
fn every_catalog_provider_is_classified_or_explicitly_queued() {
    let config = super::embedded_config(None);
    let validation = validate_data_controls_audit(&config, AS_OF);

    assert!(validation.is_clean(), "{:?}", validation.errors);
    // A measured zero is not the same as measuring nothing: pin the shape the
    // gate actually reached so a config that stopped loading providers cannot
    // read as a clean catalog.
    assert!(
        validation.provider_count >= 40,
        "gate reached only {} providers",
        validation.provider_count
    );
    assert_eq!(
        validation.declared_count + validation.unverified_count,
        validation.provider_count,
        "every provider must be declared or queued, never both and never neither"
    );
    assert!(validation.declared_count > 0);
    assert!(validation.unverified_count > 0);
}

#[test]
fn the_gate_refuses_vacuous_provider_input() {
    let config = ProvidersConfig {
        data_controls_audit: super::embedded_config(None).data_controls_audit,
        ..ProvidersConfig::default()
    };
    let validation = validate_data_controls_audit(&config, AS_OF);

    assert_eq!(validation.provider_count, 0);
    assert!(validation
        .errors
        .iter()
        .any(|error| error.contains("reached no providers")));
}

#[test]
fn removing_a_declaration_without_queueing_it_fails() {
    let mut config = super::embedded_config(None);
    config
        .providers
        .get_mut("openai")
        .expect("openai provider")
        .data_controls = None;

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation.errors.iter().any(|error| error
            .contains("provider openai has neither a data_controls declaration nor an unverified")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn a_declared_provider_may_not_also_sit_in_the_unresearched_queue() {
    let mut config = super::embedded_config(None);
    config
        .data_controls_audit
        .as_mut()
        .expect("audit registry")
        .unverified
        .push("openai".to_string());

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation.errors.iter().any(|error| error
            .contains("provider openai declares data_controls but is queued as unverified")),
        "{:?}",
        validation.errors
    );
}

/// Direction leg. A totality gate is blind to a row classified backwards, so
/// the validator checks structural coherence: a `per_request` claim must name
/// a control, and an `account`/`none` claim must not.
#[test]
fn a_per_request_claim_with_no_control_is_rejected() {
    let mut config = super::embedded_config(None);
    let openai = config
        .providers
        .get_mut("openai")
        .expect("openai provider")
        .data_controls
        .as_mut()
        .expect("openai declaration");
    openai.request_controls.clear();

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation.errors.iter().any(|error| error
            .contains("declares control_scope per_request but names no request control")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn an_account_scoped_row_may_not_smuggle_in_a_request_control() {
    let mut config = super::embedded_config(None);
    let borrowed = config
        .providers
        .get("openai")
        .expect("openai provider")
        .data_controls
        .as_ref()
        .expect("openai declaration")
        .request_controls
        .clone();
    let anthropic = config
        .providers
        .get_mut("anthropic")
        .expect("anthropic provider")
        .data_controls
        .as_mut()
        .expect("anthropic declaration");
    anthropic.request_controls = borrowed;

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation.errors.iter().any(|error| error
            .contains("provider anthropic names a request control but declares control_scope")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn a_declaration_without_an_https_citation_is_rejected() {
    let mut config = super::embedded_config(None);
    config
        .providers
        .get_mut("groq")
        .expect("groq provider")
        .data_controls
        .as_mut()
        .expect("groq declaration")
        .sources
        .clear();

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation
            .errors
            .iter()
            .any(|error| error.contains("provider groq data_controls must cite")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn the_unresearched_queue_expires() {
    let config = super::embedded_config(None);
    let validation = validate_data_controls_audit(&config, "2026-12-01");

    assert!(
        validation
            .errors
            .iter()
            .any(|error| error.contains("unverified queue expired")),
        "{:?}",
        validation.errors
    );
}

#[test]
fn an_audit_row_that_names_no_catalog_provider_is_rejected() {
    let mut config = super::embedded_config(None);
    config
        .data_controls_audit
        .as_mut()
        .expect("audit registry")
        .unverified
        .push("not-a-provider".to_string());

    let validation = validate_data_controls_audit(&config, AS_OF);
    assert!(
        validation
            .errors
            .iter()
            .any(|error| error.contains("does not name a catalog provider")),
        "{:?}",
        validation.errors
    );
}

/// The shipped default is `default`, deliberately. Making the strict posture
/// the runtime default is an embedder's visible product decision, not a
/// silent one — the same mistake as the old `store: None`.
#[test]
fn the_shipped_default_posture_is_not_strict() {
    let config = super::embedded_config(None);
    assert_eq!(
        config.data_controls_policy.default_posture,
        DataPosture::Default
    );
}

#[test]
fn an_overlay_can_flip_the_default_posture_without_touching_code() {
    let mut config = super::embedded_config(None);
    let overlay = ProvidersConfig {
        data_controls_policy: DataControlsPolicy {
            default_posture: DataPosture::StrictestAvailable,
        },
        ..ProvidersConfig::default()
    };
    config.merge_from(&overlay);

    assert_eq!(
        config.data_controls_policy.default_posture,
        DataPosture::StrictestAvailable
    );
}
