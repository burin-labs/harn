//! Catalog admission shared by compiler-facing consumers. No provider request
//! is made here; declared operations are read from the owning route registry.

use harn_lexer::Span;
use harn_parser::{diagnostic_codes::Code, DiagnosticSeverity, PredicateSite, TypeDiagnostic};

use crate::llm_config::{self, ModelOperation};

/// Inputs to operation admission, for check-result cache invalidation. Route
/// aliases and wire identities matter as well as the declared operation set.
pub fn predicate_model_catalog_identity() -> Vec<u8> {
    let config = llm_config::effective_config();
    let models: Vec<_> = config
        .models
        .iter()
        .map(|(id, model)| {
            (
                id,
                &model.provider,
                &model.wire_model,
                model.normalized_operations(),
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

pub fn validate_predicate_models(sites: &[PredicateSite]) -> Vec<TypeDiagnostic> {
    sites.iter().filter_map(|site| {
        let message = if let Some(route) = &site.model_route {
            let model = llm_config::model_catalog_id_for_route(&route.provider, &route.model)
                .and_then(|id| llm_config::model_catalog_entry(&id));
            // The currently registered backend is structured_llm. A native
            // decision-only route must not inherit a generic chat transport.
            let missing = [ModelOperation::Decision, ModelOperation::TextGeneration]
                .into_iter()
                .find(|operation| model.as_ref().is_none_or(|model| !model.supports_operation(*operation)));
            let operation = missing?;
            format!("predicate model `{}/{}` does not declare required operation `{}`", route.provider, route.model, operation.as_str())
        } else {
            "predicate model route must be a compile-time constant to check the required `decision` operation".into()
        };
        Some(TypeDiagnostic {
            code: Code::PredicateModelOperationMissing,
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(Span::with_offsets(site.start, site.end, site.line, site.column)),
            help: Some("declare a constant policy naming a catalog route with the decision operation; text generation alone does not grant predicate support".into()),
            related: Vec::new(),
            fix: None,
            details: None,
            repair: None,
        })
    }).collect()
}
