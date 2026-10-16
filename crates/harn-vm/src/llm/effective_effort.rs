//! Native option-selection provenance for provider-confirmed reasoning effort.

#[cfg(test)]
use harn_llm::effective_effort::EffectiveReasoningEffort;
use harn_llm::effective_effort::ReasoningEffortSource;

pub(crate) fn request_source(opts: &crate::llm::api::LlmCallOptions) -> ReasoningEffortSource {
    if opts
        .provider_overrides
        .as_ref()
        .and_then(|v| v.pointer("/reasoning/effort"))
        .is_some()
    {
        ReasoningEffortSource::Operator
    } else {
        match opts
            .resolution
            .iter()
            .find(|row| row.setting == "reasoning")
            .map(|row| row.source)
        {
            Some("caller.effort" | "caller.thinking") => ReasoningEffortSource::Operator,
            Some("catalog.model_defaults.reasoning_effort") => ReasoningEffortSource::Catalog,
            Some("reasoning_policy") => ReasoningEffortSource::Policy,
            _ => ReasoningEffortSource::Request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_effort_types_and_selection_provenance_agree() {
        use harn_parser::builtin_signatures::TyExt;
        use harn_parser::{Node, TypeExpr};
        let declarations = harn_parser::parse_source(
            harn_modules::stdlib_catalog()
                .source("llm/envelope")
                .expect("embedded envelope"),
        )
        .expect("envelope parses");
        let alias = |name: &str| {
            declarations
                .iter()
                .find_map(|node| match &node.node {
                    Node::TypeDecl {
                        name: declared,
                        type_expr,
                        ..
                    } if declared == name => Some(type_expr.clone()),
                    _ => None,
                })
                .expect("public alias")
        };
        let sources = alias("ReasoningEffortSource");
        assert_eq!(
            sources,
            harn_builtin_meta::shapes::REASONING_EFFORT_SOURCE.to_type_expr()
        );
        let mut observation = alias("EffectiveReasoningEffort");
        let TypeExpr::Union(variants) = &mut observation else {
            panic!("observation must be a union")
        };
        for variant in variants {
            let TypeExpr::Shape(fields) = variant else {
                panic!("observation arms must be closed records")
            };
            for field in fields {
                if field.name == "source" {
                    assert_eq!(
                        field.type_expr,
                        TypeExpr::Named("ReasoningEffortSource".into())
                    );
                    field.type_expr = sources.clone();
                }
            }
        }
        assert_eq!(
            observation,
            harn_builtin_meta::shapes::EFFECTIVE_REASONING_EFFORT.to_type_expr()
        );
        for (resolved_source, expected) in [
            ("caller.effort", ReasoningEffortSource::Operator),
            ("caller.thinking", ReasoningEffortSource::Operator),
            (
                "catalog.model_defaults.reasoning_effort",
                ReasoningEffortSource::Catalog,
            ),
            ("reasoning_policy", ReasoningEffortSource::Policy),
            ("unset", ReasoningEffortSource::Request),
        ] {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.resolution.push(crate::llm::api::ResolvedSetting {
                setting: "reasoning",
                source: resolved_source,
                ..Default::default()
            });
            let mut observation = EffectiveReasoningEffort::Reported {
                level: "high".into(),
                source: ReasoningEffortSource::Request,
            };
            observation.resolve_source(request_source(&opts));
            assert_eq!(
                observation,
                EffectiveReasoningEffort::Reported {
                    level: "high".into(),
                    source: expected
                }
            );
        }
    }
}
