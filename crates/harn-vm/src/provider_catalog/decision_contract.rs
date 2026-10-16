//! Resolve the current runtime route before applying the shared contract.

pub use harn_provider_catalog::decision_contract::{
    resolved_decision_contract, resolved_operations, DecisionContract,
};

/// Resolve the decision contract, including runtime overlays and probe facts.
/// A raw operation label alone never grants evaluator admission.
pub fn decision_contract_for_route(provider: &str, model: &str) -> Option<DecisionContract> {
    decision_contract_with_upstream_for_route(provider, model).map(|(contract, _)| contract)
}

/// Keep transport metadata on the same capability snapshot as admission,
/// without adding fields to the published, exhaustively constructible contract.
pub(crate) fn decision_contract_with_upstream_for_route(
    provider: &str,
    model: &str,
) -> Option<(DecisionContract, Option<String>)> {
    let catalog_id = crate::llm_config::model_catalog_id_for_route(provider, model)?;
    let entry = crate::llm_config::model_catalog_entry(&catalog_id)?;
    let caps = crate::llm::capabilities::lookup(provider, model);
    let contract = resolved_decision_contract(&catalog_id, &entry, &caps)?;
    Some((contract, caps.decision_upstream_provider))
}
