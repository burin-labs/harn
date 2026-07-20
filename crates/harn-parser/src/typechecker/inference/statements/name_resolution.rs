//! Value-position name resolution helpers for the statement checker.

use super::*;

impl TypeChecker {
    pub(super) fn check_value_identifier_resolves(
        &mut self,
        name: &str,
        span: Span,
        scope: &TypeScope,
    ) {
        let Some(imported) = self.imported_names.as_ref() else {
            return;
        };
        if name == "_"
            || scope.get_var(name).is_some()
            || scope.get_fn(name).is_some()
            || scope.resolve_type(name).is_some()
            || scope.get_enum(name).is_some()
            || scope.get_struct(name).is_some()
            || scope.get_interface(name).is_some()
            || builtin_signatures::is_builtin(name)
            || imported.contains(name)
            || scope.is_generic_type_param(name)
            || name.starts_with("__")
            || name.starts_with("hostlib_")
            || matches!(name, "Ok" | "Err" | "Some" | "None")
        {
            return;
        }

        let candidates: Vec<String> = builtin_signatures::iter_builtin_names()
            .map(str::to_string)
            .chain(scope.all_var_names())
            .chain(scope.all_fn_names())
            .chain(imported.iter().cloned())
            .collect();
        let suggestion = crate::diagnostic::renamed_stdlib_symbol(name)
            .map(str::to_string)
            .or_else(|| {
                crate::diagnostic::find_closest_match(
                    name,
                    candidates.iter().map(|s| s.as_str()),
                    2,
                )
                .map(str::to_string)
            });
        let message = match &suggestion {
            Some(s) => format!("value `{name}` is not defined or imported — did you mean `{s}`?"),
            None => format!("value `{name}` is not defined or imported"),
        };
        match suggestion {
            Some(s) => self.error_at_with_help(
                Code::UndefinedVariable,
                message,
                span,
                format!("did you mean `{s}`?"),
            ),
            None => self.error_at(Code::UndefinedVariable, message, span),
        }
    }

    pub(super) fn check_dict_key(&mut self, key: &SNode, scope: &mut TypeScope) {
        if matches!(
            key.node,
            Node::Identifier(_) | Node::StringLiteral(_) | Node::RawStringLiteral(_)
        ) {
            return;
        }
        self.check_node(key, scope);
    }

    pub(super) fn check_match_pattern(&mut self, pattern: &SNode, scope: &mut TypeScope) {
        match &pattern.node {
            Node::Identifier(_) => {}
            Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral => {}
            Node::ListLiteral(elements) => {
                for element in elements {
                    if is_nested_destructure_pattern(element) {
                        self.error_at(
                            Code::InvalidMatchPattern,
                            "nested list/dict patterns are not supported in match arms".into(),
                            element.span,
                        );
                        continue;
                    }
                    self.check_match_pattern(element, scope);
                }
            }
            Node::DictLiteral(entries) => {
                for entry in entries {
                    self.check_dict_key(&entry.key, scope);
                    if is_nested_destructure_pattern(&entry.value) {
                        self.error_at(
                            Code::InvalidMatchPattern,
                            "nested list/dict patterns are not supported in match arms".into(),
                            entry.value.span,
                        );
                        continue;
                    }
                    self.check_match_pattern(&entry.value, scope);
                }
            }
            Node::Spread(inner) if matches!(inner.node, Node::Identifier(_)) => {}
            Node::Spread(inner) => self.check_match_pattern(inner, scope),
            Node::EnumConstruct { .. } | Node::MethodCall { .. } => {}
            Node::FunctionCall { name, .. } => {
                let catalog = scope.lexical_match_pattern_catalog();
                match catalog.resolve_bare_variant(name) {
                    crate::lexical::BareVariantResolution::Unique(owner) => {
                        // The VM resolves bare variants only against locally
                        // declared enums, so accepting an imported one here
                        // would pass `harn check` and then fail at runtime.
                        if scope
                            .get_enum(owner)
                            .is_some_and(|info| info.origin == DeclOrigin::Imported)
                        {
                            let message =
                                crate::lexical::imported_bare_variant_message(name, owner);
                            self.error_at(Code::InvalidMatchPattern, message, pattern.span);
                        }
                    }
                    crate::lexical::BareVariantResolution::Ambiguous(owners) => self.error_at(
                        Code::InvalidMatchPattern,
                        crate::lexical::ambiguous_bare_variant_message(name, owners),
                        pattern.span,
                    ),
                    crate::lexical::BareVariantResolution::NotVariant => {
                        self.check_node(pattern, scope);
                    }
                }
            }
            Node::OrPattern(alternatives) => self.check_or_pattern_alternatives(alternatives),
            _ => self.check_node(pattern, scope),
        }
    }

    fn check_or_pattern_alternatives(&mut self, alternatives: &[SNode]) {
        for alt in alternatives {
            let is_literal = matches!(
                &alt.node,
                Node::StringLiteral(_)
                    | Node::IntLiteral(_)
                    | Node::FloatLiteral(_)
                    | Node::BoolLiteral(_)
                    | Node::NilLiteral
            );
            let is_wildcard = matches!(&alt.node, Node::Identifier(name) if name == "_");
            if !is_literal && !is_wildcard {
                self.error_at(
                    Code::InvalidMatchPattern,
                    "Or-pattern alternatives must be literal patterns \
                     (string, int, float, bool, nil, or `_`). Identifier \
                     bindings and destructuring patterns are not allowed \
                     inside `|`."
                        .into(),
                    alt.span,
                );
            }
        }
    }
}

fn is_nested_destructure_pattern(pattern: &SNode) -> bool {
    matches!(pattern.node, Node::ListLiteral(_) | Node::DictLiteral(_))
}
