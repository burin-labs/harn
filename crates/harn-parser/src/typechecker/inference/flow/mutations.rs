//! Assignment effects invalidate facts at their lexical owner.

use super::*;

impl TypeChecker {
    /// Closures capture by reference, so their writes make outer facts unstable
    /// for the whole callable. Run before checking each callable body. Binding
    /// reassignment suppresses narrowing; property writes also suppress constant
    /// evaluation, without widening unrelated reference-path types.
    pub(in crate::typechecker) fn mark_closure_mutated_captures(
        scope: &mut TypeScope,
        body: &[SNode],
    ) {
        let match_patterns = scope.lexical_match_pattern_catalog();
        scope
            .const_unstable_vars
            .extend(crate::lexical::nested_callable_value_write_names(
                body,
                &match_patterns,
            ));
        for name in crate::lexical::nested_callable_reassigned_names(body, &match_patterns) {
            scope.mark_closure_mutated(&name);
        }
    }

    /// Invalidate facts whose values a continuing branch or loop may change.
    pub(in crate::typechecker) fn invalidate_assigned_narrowings(
        scope: &mut TypeScope,
        body: &[SNode],
    ) {
        for name in
            crate::lexical::outer_value_write_names(body, &scope.lexical_match_pattern_catalog())
        {
            scope.const_values.insert(name, None);
        }
        for name in assigned_var_names(body) {
            if let Some(original) = scope.narrowed_original(&name).cloned() {
                scope.narrowed_vars.remove(&name);
                scope.update_var(&name, original);
            }
            scope.clear_narrowed_paths_rooted_at(&name);
            scope.clear_unknown_ruled_out_paths_rooted_at(&name);
        }
    }
}
