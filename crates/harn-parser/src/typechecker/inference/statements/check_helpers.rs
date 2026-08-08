use super::*;

impl TypeChecker {
    /// Walk a property/subscript assignment target to its root identifier.
    pub(super) fn assignment_root_identifier(target: &SNode) -> Option<&str> {
        match &target.node {
            Node::Identifier(name) => Some(name.as_str()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. } => {
                Self::assignment_root_identifier(object)
            }
            _ => None,
        }
    }

    /// Typecheck `EnumName.Variant(args…)` construction. Source syntax lowers
    /// to `MethodCall`, so the live path and the legacy `EnumConstruct` arm
    /// share this helper. Checks variant existence, arity, and each field's
    /// declared payload type (including opaque host names like
    /// `verdict_receipt`).
    pub(super) fn check_enum_variant_construct(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[SNode],
        span: Span,
        scope: &mut TypeScope,
    ) {
        let Some(enum_info) = scope.get_enum(enum_name).cloned() else {
            for arg in args {
                self.check_node(arg, scope);
            }
            return;
        };
        let Some(enum_variant) = enum_info
            .variants
            .iter()
            .find(|enum_variant| enum_variant.name == *variant)
        else {
            self.warning_at(
                Code::InvalidEnumConstruct,
                format!("Unknown variant '{variant}' in enum '{enum_name}'"),
                span,
            );
            for arg in args {
                self.check_node(arg, scope);
            }
            return;
        };
        if args.len() != enum_variant.fields.len() {
            let n = enum_variant.fields.len();
            let arg_word = if n == 1 { "argument" } else { "arguments" };
            self.warning_at(
                Code::OrchestrationArity,
                format!(
                    "{}.{} expects {} {}, got {}",
                    enum_name,
                    variant,
                    n,
                    arg_word,
                    args.len()
                ),
                span,
            );
        }
        let type_param_names: Vec<String> = enum_info
            .type_params
            .iter()
            .map(|tp| tp.name.clone())
            .collect();
        let (type_bindings, binding_errors) = self.infer_typed_param_type_bindings(
            &enum_variant.fields,
            false,
            &type_param_names,
            args,
            scope,
        );
        for (error_span, message) in binding_errors {
            self.error_at(Code::GenericTypeArgumentMismatch, message, error_span);
        }
        let unbound_type_params: std::collections::BTreeSet<String> = type_param_names
            .iter()
            .filter(|name| !type_bindings.contains_key(*name))
            .cloned()
            .collect();
        let mut contextual_args = vec![false; args.len()];
        for (idx, arg) in args.iter().enumerate() {
            let expected_type = enum_variant
                .fields
                .get(idx)
                .and_then(|field| field.type_expr.as_ref())
                .map(|ty| Self::apply_type_bindings(ty, &type_bindings));
            let contextual_expected = expected_type
                .as_ref()
                .filter(|ty| !Self::contains_type_param(ty, &unbound_type_params));
            contextual_args[idx] = self.check_node_with_expected(arg, contextual_expected, scope);
        }
        for (idx, (field, arg)) in enum_variant.fields.iter().zip(args.iter()).enumerate() {
            let Some(expected_type) = &field.type_expr else {
                continue;
            };
            let Some(actual_type) = self.infer_type(arg, scope) else {
                continue;
            };
            let expected = Self::apply_type_bindings(expected_type, &type_bindings);
            if !contextual_args.get(idx).copied().unwrap_or(false)
                && !self.types_compatible(&expected, &actual_type, scope)
            {
                self.type_mismatch_at(
                    Code::ArgumentTypeMismatch,
                    format!("{}.{} argument `{}`", enum_name, variant, field.name),
                    &expected,
                    &actual_type,
                    arg.span,
                    (
                        Some((
                            span,
                            format!("enum variant `{enum_name}.{variant}` expected here"),
                        )),
                        Some(arg.span),
                    ),
                    scope,
                );
            }
        }
        for arg in args.iter().skip(enum_variant.fields.len()) {
            self.check_node(arg, scope);
        }
    }

    pub(super) fn defer_forbidden_transfer(body: &[SNode]) -> Option<(&'static str, Span)> {
        body.iter().find_map(Self::node_defer_forbidden_transfer)
    }

    fn node_defer_forbidden_transfer(node: &SNode) -> Option<(&'static str, Span)> {
        match &node.node {
            Node::ReturnStmt { .. } => Some(("return", node.span)),
            Node::YieldExpr { .. } => Some(("yield", node.span)),
            Node::Closure { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::Pipeline { .. }
            | Node::OverrideDecl { .. } => None,
            _ => crate::visit::immediate_children(node)
                .into_iter()
                .find_map(Self::node_defer_forbidden_transfer),
        }
    }
}
