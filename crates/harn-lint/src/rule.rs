//! The pluggable lint-rule registry.
//!
//! Historically the linter was a hardcoded monolith: every rule was an
//! inline call in [`Linter::lint_program`][crate::linter::Linter], the
//! [`lint_node`][crate::linter::Linter] walk, or
//! [`Linter::finalize`][crate::linter::Linter], with no seam for adding
//! a rule without editing those sites. This module introduces the
//! [`Rule`] trait and a registry so built-in, engine-pattern, and
//! `.harn`-authored rules can all flow through one list.
//!
//! A rule observes the program through up to three phases that mirror
//! the linter's dispatch sites:
//!
//! - [`Rule::check_program`] — whole-program pass, before the AST walk.
//! - [`Rule::check_node`] — per-node pass, interleaved with the walk.
//! - [`Rule::finalize`] — post-walk pass, once all walk state is known.
//!
//! Every hook pushes into an `out` sink rather than returning a `Vec`,
//! so a rule that fires nothing costs no allocation. All hooks default
//! to no-ops; a rule implements only the phases it needs. The
//! intricately-stateful core checks (scope/declaration/reference
//! tracking) remain intrinsic to the [`Linter`][crate::linter::Linter]
//! walk; the registry hosts the rules that are pure functions of the
//! AST and source, which is also the exact shape engine-pattern and
//! `.harn` rules take.

use harn_parser::SNode;

use crate::diagnostic::LintDiagnostic;

/// Read-only context shared with every rule hook. Carries the inputs a
/// rule needs beyond the node/program it is handed; today that is just
/// the original source for source-aware rules.
pub(crate) struct RuleCtx<'a> {
    pub source: Option<&'a str>,
}

/// A single lint rule plugged into the registry.
pub(crate) trait Rule {
    /// Stable identifier for this rule. Matches the `rule` field on the
    /// diagnostics it emits and is the handle used for `disable_rules`
    /// filtering and per-rule config. A rule that emits several distinct
    /// diagnostic kinds reports its primary id here.
    fn id(&self) -> &str;

    /// Whether this rule participates in the per-node walk phase. Lets
    /// the walk skip [`Rule::check_node`] dispatch entirely when no rule
    /// opts in, keeping the hot path free for the common case.
    fn visits_nodes(&self) -> bool {
        false
    }

    /// Whole-program pass over the AST (plus optional source), run
    /// before the node walk.
    fn check_program(
        &mut self,
        _program: &[SNode],
        _ctx: &RuleCtx<'_>,
        _out: &mut Vec<LintDiagnostic>,
    ) {
    }

    /// Per-node pass, run for each node visited during the walk. Gated
    /// behind [`Rule::visits_nodes`].
    fn check_node(&mut self, _node: &SNode, _ctx: &RuleCtx<'_>, _out: &mut Vec<LintDiagnostic>) {}

    /// Post-walk pass, run once after the walk completes.
    fn finalize(&mut self, _ctx: &RuleCtx<'_>, _out: &mut Vec<LintDiagnostic>) {}
}

/// Build the registry of built-in rules.
///
/// Order is significant: the linter does not sort diagnostics, so
/// callers see findings in the order the rules run. This preserves the
/// historical sequence — source-aware formatting rules first, then the
/// AST-structural rules — so output is byte-identical to the previous
/// hardcoded dispatch.
pub(crate) fn builtin_rules() -> Vec<Box<dyn Rule>> {
    let rules: Vec<Box<dyn Rule>> = vec![
        Box::new(LegacyDocComments),
        Box::new(BlankLineBetweenItems),
        Box::new(TrailingComma),
        Box::new(ImportOrder),
        Box::new(PreferOptionalShorthand),
        Box::new(UnnecessaryParentheses),
        Box::new(DeprecatedLlmOptions),
        Box::new(ReminderLifecycle),
        Box::new(ReminderProviderCount),
        Box::new(ReminderRoleHint),
    ];
    // Ids address rules for per-rule config and `disable_rules`, so they
    // must be unique. Checked in debug builds so a colliding id surfaces
    // the moment a new rule is added.
    if cfg!(debug_assertions) {
        let mut ids: Vec<&str> = rules.iter().map(|rule| rule.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "built-in lint rule ids must be unique");
    }
    rules
}

/// Declare a built-in rule that is a whole-program check delegating to
/// an existing free function. The third token selects the delegate's
/// shape: `src` rules take `(source, program, out)` and no-op without
/// source; `ast` rules take `(program, out)`; `srconly` rules take
/// `(source, out)` and no-op without source.
macro_rules! program_rule {
    ($name:ident, $id:literal, src, $func:path) => {
        struct $name;
        impl Rule for $name {
            fn id(&self) -> &str {
                $id
            }
            fn check_program(
                &mut self,
                program: &[SNode],
                ctx: &RuleCtx<'_>,
                out: &mut Vec<LintDiagnostic>,
            ) {
                if let Some(src) = ctx.source {
                    $func(src, program, out);
                }
            }
        }
    };
    ($name:ident, $id:literal, ast, $func:path) => {
        struct $name;
        impl Rule for $name {
            fn id(&self) -> &str {
                $id
            }
            fn check_program(
                &mut self,
                program: &[SNode],
                _ctx: &RuleCtx<'_>,
                out: &mut Vec<LintDiagnostic>,
            ) {
                $func(program, out);
            }
        }
    };
    ($name:ident, $id:literal, srconly, $func:path) => {
        struct $name;
        impl Rule for $name {
            fn id(&self) -> &str {
                $id
            }
            fn check_program(
                &mut self,
                _program: &[SNode],
                ctx: &RuleCtx<'_>,
                out: &mut Vec<LintDiagnostic>,
            ) {
                if let Some(src) = ctx.source {
                    $func(src, out);
                }
            }
        }
    };
}

program_rule!(
    LegacyDocComments,
    "legacy-doc-comment",
    src,
    crate::harndoc::check_legacy_doc_comments
);
program_rule!(
    BlankLineBetweenItems,
    "blank-line-between-items",
    src,
    crate::rules::blank_lines::check_blank_line_between_items
);
program_rule!(
    ImportOrder,
    "import-order",
    src,
    crate::rules::import_order::check_import_order
);
program_rule!(
    PreferOptionalShorthand,
    "prefer-optional-shorthand",
    src,
    crate::rules::optional_shorthand::check_prefer_optional_shorthand
);
program_rule!(
    UnnecessaryParentheses,
    "unnecessary-parentheses",
    src,
    crate::rules::unnecessary_parentheses::check_unnecessary_parentheses
);
program_rule!(
    DeprecatedLlmOptions,
    "deprecated_llm_options",
    ast,
    crate::rules::deprecated_llm_options::check_deprecated_llm_options
);
program_rule!(
    ReminderLifecycle,
    "reminder-infinite-discardable",
    ast,
    crate::rules::reminder_lifecycle::check_reminder_lifecycle_literals
);
program_rule!(
    ReminderProviderCount,
    "reminder-provider-count",
    ast,
    crate::rules::reminder_provider_count::check_reminder_provider_count
);
program_rule!(
    ReminderRoleHint,
    "reminder-role-hint-capability",
    ast,
    crate::rules::reminder_role_hint::check_reminder_role_hint_capabilities
);

program_rule!(
    TrailingComma,
    "trailing-comma",
    srconly,
    crate::rules::trailing_comma::check_trailing_comma
);

#[cfg(test)]
mod tests {
    use super::builtin_rules;
    use std::collections::HashSet;

    #[test]
    fn builtin_rule_ids_are_unique_and_nonempty() {
        let rules = builtin_rules();
        let mut seen = HashSet::new();
        for rule in &rules {
            let id = rule.id();
            assert!(!id.is_empty(), "rule id must not be empty");
            assert!(seen.insert(id), "duplicate built-in rule id: {id}");
        }
    }
}
