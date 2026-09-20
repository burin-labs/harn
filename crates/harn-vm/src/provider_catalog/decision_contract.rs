//! The decision request contract for one catalog route.
//!
//! Two facts about a decision route live in two different owners, and this is
//! the single place that joins them: the catalog model row says *whether* the
//! route answers decision requests (`operations`) and what it costs, and the
//! capability rule says *how* the request is dialled (`decision_protocol` and
//! the declared ceilings). Callers read the joined value and never re-derive
//! either half.
//!
//! No provider request is made here.

use crate::llm::capabilities::{DecisionLimits, DecisionProtocol, DecisionQuestionKind};
use crate::llm_config::{self, ModelOperation};

/// Everything a decision evaluator needs to shape and price one request on a
/// route, resolved once from the catalog row and its matched capability rule.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionContract {
    /// How the decision operation is dialled on this route.
    pub protocol: DecisionProtocol,
    /// Question kinds the route accepts, in catalog order.
    pub question_kinds: Vec<DecisionQuestionKind>,
    /// Published request ceilings, when the protocol publishes any. `None` for
    /// a `structured_llm` route, whose bound is its ordinary context window.
    pub limits: Option<DecisionLimits>,
    /// USD per million input tokens, from the catalog row.
    pub input_price_per_mtok: Option<f64>,
    /// USD per million output tokens, from the catalog row. Jev bills input
    /// only, so this is `0.0` on every Jev route rather than absent.
    pub output_price_per_mtok: Option<f64>,
    /// The model id sent on the wire, which is not always the catalog key.
    pub served_model_id: String,
}

/// Resolve the decision contract for `(provider, model)`, or `None` when the
/// route does not serve the decision operation.
///
/// Returns `None` rather than a partial value when the row declares the
/// operation but its capability rule names no protocol: a caller must not be
/// handed a contract it could mistake for a dialable route. The repository
/// test `every_decision_route_resolves_a_complete_contract` fails closed on
/// that combination, so it cannot ship unnoticed.
pub fn decision_contract_for_route(provider: &str, model: &str) -> Option<DecisionContract> {
    let catalog_id = llm_config::model_catalog_id_for_route(provider, model)?;
    let entry = llm_config::model_catalog_entry(&catalog_id)?;
    if !entry.supports_operation(ModelOperation::Decision) {
        return None;
    }
    let caps = crate::llm::capabilities::lookup(provider, model);
    let protocol = caps.decision_protocol?;
    if caps.decision_question_kinds.is_empty() {
        return None;
    }
    let pricing = entry.pricing.as_ref();
    Some(DecisionContract {
        protocol,
        question_kinds: caps.decision_question_kinds.clone(),
        limits: caps.decision_limits,
        input_price_per_mtok: pricing.map(|pricing| pricing.input_per_mtok),
        output_price_per_mtok: pricing.map(|pricing| pricing.output_per_mtok),
        served_model_id: entry.wire_model.clone().unwrap_or(catalog_id),
    })
}
