//! Assignment summaries preserve the lexical owner's shadowing rules.

use super::*;

/// Names reassigned by a nested callable, including unknown outer parameters.
pub fn nested_callable_reassigned_names(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> Vec<String> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.reassigned.into_iter().collect()
}

/// Captured values whose contents may change when a nested callable executes.
pub fn nested_callable_value_write_names(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> Vec<String> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    analysis.record_property_writes = true;
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.reassigned.into_iter().collect()
}

/// Values outside this block that any assignment may change. A declaration
/// inside the block shadows an outer value; property writes still mutate their
/// receiver. Nested callables are conservative because their effects may escape.
pub fn outer_value_write_names(
    body: &[SNode],
    match_patterns: &MatchPatternCatalog,
) -> Vec<String> {
    let mut analysis = LexicalAnalysis::new(match_patterns);
    analysis.record_property_writes = true;
    analysis.walk_body(body, Vec::new(), true, BindingOwner::Nested);
    analysis.reassigned.into_iter().collect()
}

pub(super) fn assignment_root_name(target: &SNode) -> Option<&str> {
    match &target.node {
        Node::Identifier(name) => Some(name),
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::SubscriptAccess { object, .. }
        | Node::OptionalSubscriptAccess { object, .. } => assignment_root_name(object),
        _ => None,
    }
}
