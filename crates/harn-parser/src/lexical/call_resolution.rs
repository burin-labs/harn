use std::collections::{HashMap, HashSet};

use crate::ast::{SNode, TypedParam};

use super::{
    module_scope_node_slices, parameter_scope, BindingId, BindingOwner, LexicalAnalysis,
    MatchPatternCatalog, Scope,
};

/// Build the enum catalog with declarations imported into module scope.
/// Local declarations are registered last because they shadow imported types.
pub fn module_match_pattern_catalog_with_visible(
    program: &[SNode],
    visible_type_declarations: &[SNode],
) -> MatchPatternCatalog {
    let mut catalog = MatchPatternCatalog::default();
    catalog.extend_declarations(visible_type_declarations);
    for nodes in module_scope_node_slices(program) {
        catalog.extend_declarations(nodes);
    }
    catalog
}

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
    analyze_callable(params, body, source, match_patterns).lexically_resolved
}

/// Resolve exact declarations, including defaults and interpolated expressions.
pub fn resolved_identifier_bindings_with_source(
    params: &[TypedParam],
    body: &[SNode],
    source: Option<&str>,
    match_patterns: &MatchPatternCatalog,
) -> HashMap<(usize, usize), BindingId> {
    analyze_callable(params, body, source, match_patterns).resolved
}

fn analyze_callable<'source>(
    params: &[TypedParam],
    body: &[SNode],
    source: Option<&'source str>,
    match_patterns: &MatchPatternCatalog,
) -> LexicalAnalysis<'source> {
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
    analysis
}
