use super::*;

impl TypeChecker {
    pub(super) fn warn_unreachable_nil_coalesce_fallback(
        &mut self,
        left: &SNode,
        right: &SNode,
        left_type: Option<&TypeExpr>,
        scope: &TypeScope,
    ) {
        if !Self::nil_coalesce_left_is_source_typed_producer(left, scope) {
            return;
        }
        let Some(left_type) = left_type else {
            return;
        };
        let resolved = self.resolve_alias(left_type, scope);
        if contains_nil(&resolved) {
            return;
        }
        let Some(non_nil) = without_nil(&resolved) else {
            return;
        };
        if matches!(&non_nil, TypeExpr::Named(name) if is_gradual_type_name(name)) {
            return;
        }
        let fix_span = Span {
            start: left.span.end,
            end: right.span.end,
            line: left.span.end_line,
            column: left
                .span
                .column
                .saturating_add(left.span.end.saturating_sub(left.span.start)),
            end_line: right.span.end_line,
        };
        self.lint_warning_at_with_fix(
            Code::LintNilCoalesceUnreachableFallback,
            "nil-coalesce-unreachable-fallback",
            format!(
                "`??` fallback is unreachable because the left expression has non-nil type `{}`",
                format_type(&resolved)
            ),
            fix_span,
            "drop the unreachable fallback or make the left expression explicitly nilable"
                .to_string(),
            vec![FixEdit {
                span: fix_span,
                replacement: String::new(),
            }],
        );
    }

    fn nil_coalesce_left_is_source_typed_producer(left: &SNode, scope: &TypeScope) -> bool {
        let Node::FunctionCall { name, .. } = &left.node else {
            return false;
        };
        let Some(sig) = scope.get_fn(name) else {
            return false;
        };
        sig.definition_span.is_some() && sig.return_type.is_some()
    }
}
