//! Structural checks over the shipped catalog, plus the ticket's falsifiers.
//!
//! These run against the built-in registry rather than a fixture on purpose.
//! A fixture would prove the code path works; only the real table proves the
//! rows Harn actually ships are complete, and every census below reports the
//! count it measured so a vacuous zero cannot read as a pass.

use super::*;
use crate::llm::capabilities::{self, DecisionProtocol};
use llm_config::ModelOperation;

/// Every catalog route declaring `decision`, as `(catalog_id, provider)`.
fn decision_rows() -> Vec<(String, String)> {
    llm_config::embedded_config(None)
        .models
        .iter()
        .filter(|(id, entry)| {
            entry.supports_operation(ModelOperation::Decision)
                || decision_contract_for_route(&entry.provider, id).is_some()
        })
        .map(|(id, entry)| (id.clone(), entry.provider.clone()))
        .collect()
}

#[test]
fn native_schema_gateway_derives_decision_but_explicit_unsupported_does_not() {
    let id = "vercel/openai/gpt-5.4-nano";
    let entry = llm_config::model_catalog_entry(id).expect("gateway route exists");
    let mut caps = capabilities::lookup(&entry.provider, id);
    let contract = decision_contract::resolved_decision_contract(id, &entry, &caps)
        .expect("native schema supports structured decisions");
    assert_eq!(contract.protocol, DecisionProtocol::StructuredLlm);
    assert!(decision_contract::resolved_operations(id, &entry, &caps)
        .contains(&ModelOperation::Decision));
    caps.structured_output = Some("none".into());
    caps.json_schema = Some("native".into());
    assert!(decision_contract::resolved_decision_contract(id, &entry, &caps).is_none());
    assert!(!decision_contract::resolved_operations(id, &entry, &caps)
        .contains(&ModelOperation::Decision));
}

#[test]
fn every_decision_route_resolves_a_complete_contract() {
    let rows = decision_rows();
    assert!(
        rows.len() >= 8,
        "measured only {} decision rows; a near-empty census cannot falsify anything",
        rows.len()
    );
    let incomplete: Vec<_> = rows
        .iter()
        .filter(|(id, provider)| decision_contract_for_route(provider, id).is_none())
        .map(|(id, _)| id.clone())
        .collect();
    assert!(
        incomplete.is_empty(),
        "{} of {} decision rows resolve no contract: {:?}",
        incomplete.len(),
        rows.len(),
        incomplete
    );
}

#[test]
fn a_native_decision_route_publishes_limits_and_claims_no_chat() {
    let mut native = 0;
    for (id, provider) in decision_rows() {
        let contract = decision_contract_for_route(&provider, &id).expect("contract resolves");
        if !contract.protocol.is_native() {
            continue;
        }
        native += 1;
        assert!(
            contract.limits.is_some(),
            "native decision route {id} publishes no request ceilings"
        );
        let entry = llm_config::model_catalog_entry(&id).expect("row exists");
        assert!(
            !entry.supports_operation(ModelOperation::TextGeneration),
            "native decision route {id} also claims text generation; a decision \
             model must not inherit a generic chat transport"
        );
        assert!(
            !contract.question_kinds.is_empty(),
            "native decision route {id} accepts no question kind"
        );
    }
    assert!(
        native >= 4,
        "measured only {native} native decision routes; expected the four Jev rows"
    );
}

#[test]
fn a_structured_llm_route_can_actually_honour_a_schema() {
    let mut structured = 0;
    for (id, provider) in decision_rows() {
        let contract = decision_contract_for_route(&provider, &id).expect("contract resolves");
        if contract.protocol != DecisionProtocol::StructuredLlm {
            continue;
        }
        structured += 1;
        let entry = llm_config::model_catalog_entry(&id).expect("row exists");
        assert!(
            entry.supports_operation(ModelOperation::TextGeneration),
            "structured_llm route {id} dials the chat endpoint but declares no \
             text_generation operation"
        );
        let caps = capabilities::lookup(&provider, &id);
        let mode = caps
            .structured_output
            .clone()
            .or_else(|| caps.json_schema.clone())
            .unwrap_or_else(|| "none".to_string());
        assert_eq!(
            mode, "native",
            "structured_llm route {id} resolves structured output {mode:?}; a \
             decision answered by schema needs native structured output"
        );
    }
    assert!(
        structured >= 4,
        "measured only {structured} structured_llm routes; the curated chat set is larger"
    );
}

#[test]
fn no_route_names_a_decision_protocol_without_declaring_the_operation() {
    let config = llm_config::embedded_config(None);
    let stray: Vec<_> = config
        .models
        .iter()
        .filter(|(_, entry)| !entry.supports_operation(ModelOperation::Decision))
        .filter(|(id, entry)| {
            capabilities::lookup(&entry.provider, id)
                .decision_protocol
                .is_some()
        })
        .map(|(id, _)| id.clone())
        .collect();
    assert!(
        stray.is_empty(),
        "{} rows carry a decision protocol without the operation that admits it: {:?}",
        stray.len(),
        stray
    );
}

/// Falsifier (a), catalog half: the Vercel Jev route declares no text
/// generation, which is what the runtime chat gate refuses on. The refusal
/// itself is exercised end to end in
/// `harn-cli/tests/harn_cli_e2e/decision_route_admission.rs`.
#[test]
fn jev_declares_no_text_generation_operation() {
    let entry = llm_config::model_catalog_entry("vercel/typesafe-ai/jev")
        .expect("the Jev route is shipped");
    assert!(!entry.supports_operation(ModelOperation::TextGeneration));
    assert!(entry.supports_operation(ModelOperation::Decision));

    // Positive control: the curated chat sibling declares BOTH, so a gate that
    // refused every row would not read as a pass here.
    let sibling = llm_config::model_catalog_entry("gpt-5.4-nano").expect("curated row is shipped");
    assert!(sibling.supports_operation(ModelOperation::TextGeneration));
    assert!(sibling.supports_operation(ModelOperation::Decision));
}

/// Falsifier (d), structural half: nothing alias-backed resolves to a
/// decision-only row.
///
/// This is the invariant that keeps a decision row out of tier selection and
/// escalation ladders, both of which enumerate ALIASES rather than model rows.
/// Asserting it here is cheaper and harder to bypass than filtering at each of
/// those call sites, and it fails the moment someone adds a convenience alias.
#[test]
fn no_alias_resolves_to_a_decision_only_row() {
    let config = llm_config::embedded_config(None);
    assert!(
        config.aliases.len() > 20,
        "measured only {} aliases; a near-empty census cannot falsify anything",
        config.aliases.len()
    );
    let offending: Vec<_> = config
        .aliases
        .iter()
        .filter(|(_, alias)| {
            config.models.get(&alias.id).is_some_and(|entry| {
                entry.supports_operation(ModelOperation::Decision)
                    && !entry.supports_operation(ModelOperation::TextGeneration)
            })
        })
        .map(|(name, _)| name.clone())
        .collect();
    assert!(
        offending.is_empty(),
        "{} aliases point at a decision-only row, which would make it selectable \
         as a tier or ladder rung: {:?}",
        offending.len(),
        offending
    );
}

/// Falsifier (b): an unserved sibling is refused before any request.
#[test]
fn an_unserved_jev_sibling_is_not_in_the_catalog_at_all() {
    assert!(
        llm_config::model_catalog_id_for_route("vercel_ai_gateway", "vercel/typesafe-ai/jev-2")
            .is_none(),
        "an unserved sibling must not resolve to a catalog row"
    );
    assert!(
        decision_contract_for_route("vercel_ai_gateway", "vercel/typesafe-ai/jev-2").is_none(),
        "an unserved sibling must resolve no decision contract"
    );
}

/// Falsifier (c): a predicate site naming each native route passes admission.
#[test]
fn a_predicate_site_naming_each_native_jev_route_is_admitted() {
    let routes = [
        ("typesafe", "typesafe/jev-1.13.0"),
        ("typesafe", "typesafe/jev-latest"),
        ("vercel_ai_gateway", "vercel/typesafe-ai/jev"),
        ("openrouter", "openrouter/typesafe/jev-1.13"),
    ];
    for (provider, model) in routes {
        let diagnostics = validate_predicate_models(&[predicate_site(provider, model)]);
        assert!(
            diagnostics.is_empty(),
            "{provider}/{model} was refused: {:?}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
        );
    }

    // Negative control through the same function: a chat-only route is still
    // refused, and the message names the operation it lacks.
    let refused = validate_predicate_models(&[predicate_site("anthropic", "claude-sonnet-5")]);
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0].message.contains("decision"),
        "{:?}",
        refused[0].message
    );
}

/// A route declaring `decision` whose capability rule names no protocol is
/// refused with a message about the protocol, not about the operation.
#[test]
fn a_decision_row_without_a_protocol_is_refused_by_name() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models.synthetic-protocolless-decision]
name = "Synthetic protocolless decision"
provider = "openrouter"
context_window = 8192
operations = ["decision"]
"#,
    )
    .expect("overlay parses");
    llm_config::set_user_overrides(Some(overlay));
    let diagnostics = validate_predicate_models(&[predicate_site(
        "openrouter",
        "synthetic-protocolless-decision",
    )]);
    llm_config::set_user_overrides(None);
    assert_eq!(
        diagnostics.len(),
        1,
        "{:?}",
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        diagnostics[0].message.contains("decision protocol"),
        "{:?}",
        diagnostics[0].message
    );
}

fn predicate_site(provider: &str, model: &str) -> harn_parser::PredicateSite {
    harn_parser::PredicateSite {
        id: "site".to_string(),
        kind: harn_parser::PredicateSiteKind::Predicate,
        questions: vec![harn_parser::PredicateQuestionSpec {
            id: "site".to_string(),
            kind: harn_parser::PredicateQuestionKind::Boolean,
            instructions: "is this ready".to_string(),
            labels: Vec::new(),
        }],
        input_type: harn_parser::TypeExpr::Named("dict".to_string()),
        line: 1,
        column: 1,
        start: 0,
        end: 1,
        model_route: Some(harn_parser::PredicateModelRoute {
            provider: provider.to_string(),
            model: model.to_string(),
        }),
    }
}
