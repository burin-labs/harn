//! Catalog admission shared by compiler-facing consumers. No provider request
//! is made here; declared operations are read from the owning route registry.

use harn_lexer::Span;
use harn_parser::{diagnostic_codes::Code, DiagnosticSeverity, PredicateSite, TypeDiagnostic};

use crate::llm_config::{self, ModelOperation};
use crate::provider_catalog::decision_contract::decision_contract_for_route;

/// Inputs to operation admission, for check-result cache invalidation. Route
/// aliases and wire identities matter as well as the declared operation set,
/// and so does the decision protocol a route's capability rule names, because
/// the protocol decides which operations the site needs.
pub fn predicate_model_catalog_identity() -> Vec<u8> {
    let config = llm_config::effective_config();
    let models: Vec<_> = config
        .models
        .iter()
        .map(|(id, model)| {
            let caps = crate::llm::capabilities::lookup(&model.provider, id);
            let contract = super::decision_contract::resolved_decision_contract(id, model, &caps);
            (
                id,
                &model.provider,
                &model.wire_model,
                super::decision_contract::resolved_operations(id, model, &caps),
                contract.map(|contract| contract.protocol.as_str()),
            )
        })
        .collect();
    let aliases: Vec<_> = config
        .aliases
        .iter()
        .map(|(name, alias)| (name, &alias.provider, &alias.id))
        .collect();
    serde_json::to_vec(&(models, aliases)).expect("catalog admission facts serialize")
}

/// What a route is missing before it can answer a predicate.
enum AdmissionGap {
    /// The route does not declare the `decision` operation at all.
    Operation(ModelOperation),
    /// The route declares `decision` but no capability rule names how the
    /// operation is dialled, so there is no endpoint to send the request to.
    Protocol,
}

fn admission_gap(provider: &str, model: &str) -> Option<AdmissionGap> {
    if decision_contract_for_route(provider, model).is_some() {
        return None;
    }
    let entry = llm_config::model_catalog_id_for_route(provider, model)
        .and_then(|id| llm_config::model_catalog_entry(&id));
    let Some(entry) = entry else {
        return Some(AdmissionGap::Operation(ModelOperation::Decision));
    };
    if !entry.supports_operation(ModelOperation::Decision) {
        return Some(AdmissionGap::Operation(ModelOperation::Decision));
    }
    Some(AdmissionGap::Protocol)
}

pub fn validate_predicate_models(sites: &[PredicateSite]) -> Vec<TypeDiagnostic> {
    sites
        .iter()
        .filter(|site| site.kind != harn_parser::PredicateSiteKind::RuntimeEvaluation)
        .filter_map(|site| {
            let (message, help) = if let Some(route) = &site.model_route {
                match admission_gap(&route.provider, &route.model)? {
                    AdmissionGap::Operation(operation) => (
                        format!(
                            "predicate model `{}/{}` does not declare required operation `{}`",
                            route.provider,
                            route.model,
                            operation.as_str()
                        ),
                        "declare a constant policy naming a catalog route with the decision operation; text generation alone does not grant predicate support",
                    ),
                    AdmissionGap::Protocol => (
                        format!(
                            "predicate model `{}/{}` declares the `decision` operation but no capability rule names a decision protocol for it",
                            route.provider, route.model
                        ),
                        "add a `decision_protocol` capability rule for this route; the operation picks its endpoint before transport, so there is nothing to dial without one",
                    ),
                }
            } else {
                (
                    "predicate model route must be a compile-time constant to check the required `decision` operation".to_string(),
                    "declare a constant policy naming a catalog route with the decision operation; text generation alone does not grant predicate support",
                )
            };
            Some(TypeDiagnostic {
                code: Code::PredicateModelOperationMissing,
                message,
                severity: DiagnosticSeverity::Error,
                span: Some(Span::with_offsets(site.start, site.end, site.line, site.column)),
                help: Some(help.into()),
                related: Vec::new(),
                fix: None,
                details: None,
                repair: None,
            })
        })
        .collect()
}
