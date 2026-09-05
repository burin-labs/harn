//! Wire-level contract for the data-controls plan.
//!
//! Every assertion here reads the request body Harn would send, not an
//! intermediate struct: the claim is "the control is on the wire", and a
//! struct field proves only that a struct was populated.

use super::*;
use serde_json::json;

fn base_body() -> serde_json::Value {
    json!({"model": MODEL_WITH_NO_DECLARATION, "messages": [{"role": "user", "content": "hi"}]})
}

/// The cases below are about the provider's per-request controls, so they
/// route through a model id that carries no declaration of its own and
/// therefore inherits its provider's. The route-level override has its own
/// cases at the bottom of this file.
const MODEL_WITH_NO_DECLARATION: &str = "m";

#[test]
fn default_posture_leaves_the_request_byte_identical() {
    let mut body = base_body();
    let plan = resolve(
        "openai",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::Default,
    );
    plan.write_body(&mut body);

    assert_eq!(body, base_body(), "default posture must not touch the body");
    assert!(plan.headers.is_empty());
    assert_eq!(plan.receipt.outcome, DataControlsOutcome::NotRequested);
    // The scope is still reported: a host can see what it declined to use.
    assert_eq!(
        plan.receipt.control_scope,
        Some(DataControlScope::PerRequest)
    );
    assert!(plan.receipt.applied.is_empty());
}

#[test]
fn strict_posture_puts_openai_store_false_on_the_wire() {
    let mut body = base_body();
    let plan = resolve(
        "openai",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);

    assert_eq!(body["store"], json!(false));
    assert_eq!(plan.receipt.outcome, DataControlsOutcome::Applied);
    assert_eq!(plan.receipt.applied.len(), 1);
    let applied = &plan.receipt.applied[0];
    assert_eq!(applied.location, "body");
    assert_eq!(applied.name, "store");
    assert_eq!(applied.value, json!(false));
    assert_eq!(applied.effect, "retention");
    assert!(
        applied.caveat.is_some(),
        "the abuse-monitoring caveat must survive onto the receipt"
    );
}

/// The negative control the issue asks for. Anthropic exposes no per-request
/// retention flag, so the strict posture must report that it was NOT achieved
/// rather than reporting success over an unchanged body.
#[test]
fn strict_posture_on_a_provider_with_no_control_says_so() {
    let mut body = base_body();
    let plan = resolve(
        "anthropic",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::AnthropicSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);

    assert_eq!(body, base_body(), "nothing may be invented on this wire");
    assert!(plan.headers.is_empty());
    assert_eq!(
        plan.receipt.outcome,
        DataControlsOutcome::NoControlAvailable
    );
    assert_eq!(plan.receipt.control_scope, Some(DataControlScope::Account));
    assert!(
        plan.receipt
            .note
            .as_deref()
            .is_some_and(|note| note.contains("organization")),
        "the receipt must carry why the strict posture was unavailable"
    );
}

/// An unresearched provider must not read as a researched "offers nothing".
#[test]
fn an_unknown_provider_reads_as_unresearched_not_as_no_control() {
    let mut body = base_body();
    let plan = resolve(
        "provider-that-does-not-exist",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);

    assert_eq!(
        plan.receipt.outcome,
        DataControlsOutcome::ProviderUnresearched
    );
    assert_eq!(plan.receipt.control_scope, None);
    assert_ne!(
        plan.receipt.outcome,
        DataControlsOutcome::NoControlAvailable,
        "absence of a declaration is not a declaration of absence"
    );
}

#[test]
fn openrouter_writes_both_effects_into_a_nested_provider_object() {
    let mut body = base_body();
    let plan = resolve(
        "openrouter",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);

    assert_eq!(body["provider"]["data_collection"], json!("deny"));
    assert_eq!(body["provider"]["zdr"], json!(true));
    assert_eq!(plan.receipt.outcome, DataControlsOutcome::Applied);
    let effects: Vec<&str> = plan
        .receipt
        .applied
        .iter()
        .map(|control| control.effect.as_str())
        .collect();
    assert!(effects.contains(&"training"));
    assert!(effects.contains(&"retention"));
}

#[test]
fn a_nested_path_merges_into_an_existing_provider_object() {
    let mut body = base_body();
    body["provider"] = json!({"order": ["a"]});
    resolve(
        "openrouter",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    )
    .write_body(&mut body);

    assert_eq!(body["provider"]["order"], json!(["a"]));
    assert_eq!(body["provider"]["data_collection"], json!("deny"));
}

/// Gemini documents `store` on the Interactions API and not on
/// `generateContent`. Applying it to both would put an undocumented field on a
/// live wire, so the dialect scope has to bite.
#[test]
fn gemini_store_applies_only_to_the_interactions_dialect() {
    let mut generate_content = base_body();
    let plan = resolve(
        "gemini",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::GeminiJson,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut generate_content);
    assert_eq!(generate_content, base_body());
    assert_eq!(
        plan.receipt.outcome,
        DataControlsOutcome::NoControlAvailable
    );

    let mut interactions = base_body();
    let plan = resolve(
        "gemini",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::GeminiInteractionsSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut interactions);
    assert_eq!(interactions["store"], json!(false));
    assert_eq!(plan.receipt.outcome, DataControlsOutcome::Applied);
}

/// Direction control for the registry's totality gate.
///
/// The audit gate asserts every provider is classified. That gate stays green
/// when a provider is classified backwards, so this pins the direction of the
/// rows the feature actually rests on: one that must act and one that must
/// not. Flip either row in the catalog source and exactly one of these fails
/// while the totality gate stays green.
#[test]
fn load_bearing_rows_are_classified_in_the_right_direction() {
    let acting = declaration_for("openai").expect("openai is researched");
    assert_eq!(acting.control_scope, DataControlScope::PerRequest);
    assert!(acting.offers_per_request_control());

    let contractual = declaration_for("anthropic").expect("anthropic is researched");
    assert_eq!(contractual.control_scope, DataControlScope::Account);
    assert!(!contractual.offers_per_request_control());
    assert!(contractual.request_controls.is_empty());
}

/// A provider whose declaration is a researched "nothing to opt out of" is a
/// different fact from an account-scoped one, and both differ from silence.
#[test]
fn scope_none_is_a_researched_claim_with_a_citation() {
    let cerebras = declaration_for("cerebras").expect("cerebras is researched");
    assert_eq!(cerebras.control_scope, DataControlScope::None);
    assert!(cerebras.request_controls.is_empty());
    assert!(cerebras
        .sources
        .iter()
        .all(|source| source.starts_with("https://")));
}

#[test]
fn every_stream_protocol_maps_to_a_declared_dialect() {
    // A compile-time-exhaustive map is only useful if something exercises it.
    for protocol in [
        StreamProtocol::AnthropicSse,
        StreamProtocol::OpenAiSse,
        StreamProtocol::OllamaNdjson,
        StreamProtocol::GeminiJson,
        StreamProtocol::GeminiInteractionsSse,
    ] {
        let dialect = dialect_of(protocol);
        let mut body = base_body();
        // Any dialect must survive a strict-posture pass without panicking.
        resolve(
            "openai",
            MODEL_WITH_NO_DECLARATION,
            dialect,
            DataPosture::StrictestAvailable,
        )
        .write_body(&mut body);
    }
}

#[test]
fn the_receipt_serializes_with_the_outcome_spelled_out() {
    let mut body = base_body();
    let plan = resolve(
        "anthropic",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::AnthropicSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);
    let json = serde_json::to_value(&plan.receipt).expect("receipt serializes");
    assert_eq!(json["outcome"], json!("no_control_available"));
    assert_eq!(json["requested_posture"], json!("strictest_available"));
    assert_eq!(json["control_scope"], json!("account"));
}

/// Precedence. The body half is written after the caller's
/// `provider_overrides` escape hatch, so an override that sets the same field
/// loses. Anything else would leave the receipt claiming a control the wire
/// does not carry.
#[test]
fn a_declared_control_overwrites_a_conflicting_caller_value() {
    let mut body = base_body();
    body["store"] = json!(true);
    let plan = resolve(
        "openai",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    );
    plan.write_body(&mut body);

    assert_eq!(body["store"], json!(false));
    assert_eq!(plan.receipt.outcome, DataControlsOutcome::Applied);
}

/// A non-object sitting on a nested control's path is replaced rather than
/// silently swallowing the write.
#[test]
fn a_nested_control_replaces_a_non_object_on_its_path() {
    let mut body = base_body();
    body["provider"] = json!("openai");
    resolve(
        "openrouter",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    )
    .write_body(&mut body);

    assert_eq!(body["provider"]["zdr"], json!(true));
}

// ── Training-tier refusal ───────────────────────────────────────────────────
//
// The strict posture's promise is that the traffic is not trained on. On a
// route with no per-request control, "apply every declared control" applies
// nothing, so without a refusal the call would go out unchanged and the
// receipt would read `no_control_available` — a strict request that silently
// achieved nothing. These pin the refusal, and, just as importantly, pin what
// must NOT refuse.

#[test]
fn strict_posture_refuses_a_model_row_that_declares_training() {
    let refusal = training_refusal(
        "meta",
        "muse-spark-1.3-contributor",
        DataPosture::StrictestAvailable,
    )
    .expect("the contributor tier trains and must be refused under the strict posture");

    assert!(
        refusal.contains("muse-spark-1.3-contributor"),
        "the refusal must name the route being refused: {refusal}"
    );
    // The person reading this has to be able to check the claim themselves.
    assert!(
        refusal.contains("https://"),
        "the refusal must cite the source backing the training claim: {refusal}"
    );
}

/// Negative control for the test above. Without this, a `training_refusal`
/// that refused unconditionally would pass the positive case green.
#[test]
fn strict_posture_allows_the_standard_tier_of_the_same_provider() {
    assert_eq!(
        training_refusal("meta", "muse-spark-1.3", DataPosture::StrictestAvailable),
        None,
        "the standard tier does not train and must still route"
    );
}

/// The direction of the model-level override, pinned. A row classified
/// backwards passes any gate that only asks "is every row classified?".
#[test]
fn the_two_meta_tiers_are_classified_in_opposite_directions() {
    use crate::llm_config::{effective_training_default, TrainingDefault};

    assert_eq!(
        effective_training_default("meta", "muse-spark-1.3-contributor"),
        Some(TrainingDefault::Trains),
    );
    assert_eq!(
        effective_training_default("meta", "muse-spark-1.3"),
        Some(TrainingDefault::DoesNotTrain),
    );
}

/// The model row overrides its provider rather than merely coexisting with it.
/// Meta's provider-level declaration says `does_not_train`, so a contributor
/// row that failed to override would read as safe.
#[test]
fn a_model_row_overrides_its_providers_declaration() {
    use crate::llm_config::{effective_training_default, provider_config, TrainingDefault};

    let provider_level = provider_config("meta")
        .and_then(|definition| definition.data_controls)
        .expect("meta declares provider-level data controls")
        .training_default;
    assert_eq!(provider_level, TrainingDefault::DoesNotTrain);

    assert_eq!(
        effective_training_default("meta", "muse-spark-1.3-contributor"),
        Some(TrainingDefault::Trains),
        "the model row must win over the provider's declaration"
    );
}

/// The second negative control control asked for: this closes a pre-existing
/// exposure rather than only covering the row this change added. Both
/// providers were already classified as training on API traffic in the
/// catalog, with nothing stopping a strict run from routing to them.
#[test]
fn strict_posture_refuses_providers_already_classified_as_training() {
    for (provider, model) in [
        ("deepseek", "deepseek-v4-pro"),
        ("cohere", "command-a-plus-05-2026"),
    ] {
        assert!(
            training_refusal(provider, model, DataPosture::StrictestAvailable).is_some(),
            "{provider} is classified as training on API traffic and must be refused"
        );
    }
}

/// The refusal is scoped to the posture that asked for it. Under the shipped
/// `default` posture nothing is refused, because the caller never claimed the
/// traffic would go untrained.
#[test]
fn the_default_posture_refuses_nothing() {
    assert_eq!(
        training_refusal("meta", "muse-spark-1.3-contributor", DataPosture::Default),
        None,
    );
    assert_eq!(
        training_refusal("deepseek", "deepseek-v4-pro", DataPosture::Default),
        None,
    );
}

/// "Nobody has checked" must not acquire the force of "we checked and it
/// trains". An unresearched provider keeps reporting itself through the
/// receipt's `provider_unresearched` outcome instead of failing the call.
#[test]
fn an_unresearched_provider_is_not_refused() {
    assert_eq!(
        training_refusal(
            "nvidia",
            "some-unresearched-route",
            DataPosture::StrictestAvailable
        ),
        None,
        "an unresearched provider reports through the receipt, it does not refuse"
    );
}

// ── What the receipt reports ────────────────────────────────────────────────

/// A receipt that named only the provider would report Meta's provider-level
/// line, "not used to improve our products", on a route that is trained on.
/// The receipt is what a host, a transcript, and an eval trial all read, so it
/// has to answer for the route that actually ran.
#[test]
fn the_receipt_reports_the_routes_own_training_posture_not_its_providers() {
    let contributor = resolve(
        "meta",
        "muse-spark-1.3-contributor",
        DataControlDialect::OpenAiSse,
        DataPosture::Default,
    )
    .receipt;

    assert_eq!(contributor.model, "muse-spark-1.3-contributor");
    assert_eq!(
        contributor.training_default,
        Some(crate::llm_config::TrainingDefault::Trains),
        "the receipt must carry the route's declaration, not the provider's"
    );
    let note = contributor
        .note
        .expect("the contributor row carries its own note");
    assert!(
        note.contains("train"),
        "the note surfaced for this route must be the route's own: {note}"
    );

    let standard = resolve(
        "meta",
        "muse-spark-1.3",
        DataControlDialect::OpenAiSse,
        DataPosture::Default,
    )
    .receipt;
    assert_eq!(
        standard.training_default,
        Some(crate::llm_config::TrainingDefault::DoesNotTrain),
        "the standard tier of the same provider must read the other way"
    );
}

/// A route with no declaration at either level reports `None` rather than
/// borrowing a neighbour's answer. "Nobody has checked" stays distinguishable
/// from "we checked and it does not train".
#[test]
fn an_unresearched_route_reports_no_training_posture_at_all() {
    let receipt = resolve(
        "provider-that-does-not-exist",
        MODEL_WITH_NO_DECLARATION,
        DataControlDialect::OpenAiSse,
        DataPosture::StrictestAvailable,
    )
    .receipt;

    assert_eq!(receipt.outcome, DataControlsOutcome::ProviderUnresearched);
    assert_eq!(receipt.training_default, None);
}

/// The reported posture is the one that applied, so a deployment that ships
/// the strict posture cannot be misread as one that never asked.
#[test]
fn the_reported_posture_is_the_one_that_applied() {
    for (posture, expected) in [
        (DataPosture::Default, "default"),
        (DataPosture::StrictestAvailable, "strictest_available"),
    ] {
        assert_eq!(
            resolve(
                "openai",
                MODEL_WITH_NO_DECLARATION,
                DataControlDialect::OpenAiSse,
                posture,
            )
            .receipt
            .requested_posture,
            expected
        );
    }
}
