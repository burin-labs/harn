use std::collections::HashSet;

use crate::ast::{SNode, TypedParam};

use super::{parameter_scope, BindingOwner, LexicalAnalysis, MatchPatternCatalog, Scope};

/// Return identifier-use spans that resolve to any lexical binding.
///
/// Unlike `resolved_identifier_bindings`, this includes callable names whose
/// declaration identity is immaterial to the consumer. Source text lets the
/// analysis also reach expression holes inside interpolated strings.
pub fn lexically_resolved_identifier_spans(
    params: &[TypedParam],
    body: &[SNode],
    source: Option<&str>,
    match_patterns: &MatchPatternCatalog,
) -> HashSet<(usize, usize)> {
    let mut analysis = LexicalAnalysis::new_with_source(match_patterns, source);
    // Defaults execute left to right: only earlier parameters are visible.
    let mut visible_params = Scope::new();
    for param in params {
        if let Some(default) = &param.default_value {
            analysis.walk_node(
                default,
                std::slice::from_ref(&visible_params),
                false,
                &BindingOwner::Current,
            );
        }
        visible_params.extend(parameter_scope(
            std::slice::from_ref(param),
            &BindingOwner::Current,
        ));
    }
    analysis.walk_body_with_bindings(
        body,
        Vec::new(),
        false,
        BindingOwner::Current,
        parameter_scope(params, &BindingOwner::Current),
    );
    analysis.lexically_resolved
}
