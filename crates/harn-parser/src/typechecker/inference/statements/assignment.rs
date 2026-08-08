use super::*;

impl TypeChecker {
    pub(super) fn check_assignment(
        &mut self,
        target: &SNode,
        value: &SNode,
        op: &Option<String>,
        span: Span,
        scope: &mut TypeScope,
    ) {
        let path_slot_type = if matches!(&target.node, Node::Identifier(_)) {
            None
        } else {
            self.assignment_path_slot_type(target, scope)
        };
        let expected_value_type = if op.is_none() {
            if let Node::Identifier(name) = &target.node {
                scope.get_var(name).cloned().flatten()
            } else {
                path_slot_type.clone()
            }
        } else {
            None
        };
        let context_checked =
            self.check_node_with_expected(value, expected_value_type.as_ref(), scope);

        if !matches!(&target.node, Node::Identifier(_)) {
            if let Some(root) = Self::assignment_root_identifier(target) {
                if scope.get_var(root).is_some() && !scope.is_mutable(root) {
                    self.warning_at(
                        Code::ImmutableAssignment,
                        format!(
                            "Cannot mutate '{root}' through an immutable binding (declared with 'const'); use 'let' for a mutable binding"
                        ),
                        span,
                    );
                }
            }
        }
        if let Node::Identifier(name) = &target.node {
            let mut widened_slot_type: Option<TypeExpr> = None;
            if scope.get_var(name).is_some() && !scope.is_mutable(name) {
                self.warning_at(
                    Code::ImmutableAssignment,
                    format!(
                        "Cannot assign to '{name}': variable is immutable (declared with 'const'); use 'let' for a mutable binding"
                    ),
                    span,
                );
            }

            if let Some(Some(var_type)) = scope.get_var(name) {
                let value_type = self.infer_type(value, scope);
                let assigned = if let Some(op) = op {
                    let var_inferred = scope.get_var(name).cloned().flatten();
                    infer_binary_op_type(op, &var_inferred, &value_type)
                } else {
                    value_type
                };
                if !context_checked {
                    if let Some(actual) = &assigned {
                        let check_type = scope
                            .narrowed_original(name)
                            .and_then(|ty| ty.as_ref())
                            .unwrap_or(var_type);
                        if !self.types_compatible(check_type, actual, scope) {
                            if scope.is_mutable(name)
                                && scope.is_nil_widenable(name)
                                && Self::is_nil_type(check_type)
                                && !Self::is_nil_type(actual)
                            {
                                widened_slot_type = Some(Self::union_with_nil(actual));
                            } else {
                                self.type_mismatch_at(
                                    Code::AssignmentTypeMismatch,
                                    format!("assignment to `{name}`"),
                                    check_type,
                                    actual,
                                    value.span,
                                    (
                                        Some((
                                            target.span,
                                            format!("`{name}` has this expected type"),
                                        )),
                                        Some(value.span),
                                    ),
                                    scope,
                                );
                            }
                        }
                    }
                }
            }

            let original = scope
                .narrowed_vars
                .remove(name)
                .or_else(|| scope.narrowed_original(name).cloned());
            if let Some(original) = original {
                if let Some(widened) = widened_slot_type.as_ref() {
                    scope.define_var(name, Some(widened.clone()));
                } else {
                    scope.define_var(name, original);
                }
            }
            if let Some(widened) = widened_slot_type {
                scope.define_var(name, Some(widened));
                scope.clear_nil_widenable(name);
            }
            scope.define_schema_binding(name, None);
            scope.clear_unknown_ruled_out(name);
            scope.clear_narrowed_paths_rooted_at(name);
            scope.clear_unknown_ruled_out_paths_rooted_at(name);
        } else if let Some(base) = Self::assignment_target_root(target) {
            if let Some(slot_type) = &path_slot_type {
                let value_type = self.infer_type(value, scope);
                let assigned = if let Some(op) = op {
                    let slot_read = self.infer_type(target, scope);
                    infer_binary_op_type(op, &slot_read, &value_type)
                } else {
                    value_type
                };
                if !context_checked {
                    if let Some(actual) = &assigned {
                        if !self.types_compatible(slot_type, actual, scope) {
                            let label = self.render_assignment_target(target);
                            self.type_mismatch_at(
                                Code::AssignmentTypeMismatch,
                                format!("assignment to `{label}`"),
                                slot_type,
                                actual,
                                value.span,
                                (
                                    Some((
                                        target.span,
                                        format!("`{label}` has this expected type"),
                                    )),
                                    Some(value.span),
                                ),
                                scope,
                            );
                        }
                    }
                }
            }
            scope.clear_narrowed_paths_rooted_at(base);
            scope.clear_unknown_ruled_out_paths_rooted_at(base);
        }
        if op.is_none() {
            self.narrow_after_assignment(target, value, scope);
        }
    }

    fn narrow_after_assignment(&self, target: &SNode, value: &SNode, scope: &mut TypeScope) {
        let Some(value_ty) = self.infer_type(value, scope) else {
            return;
        };
        if contains_nil(&self.resolve_alias(&value_ty, scope)) {
            return;
        }
        match &target.node {
            Node::Identifier(name) => {
                let Some(Some(slot_ty)) = scope.get_var(name) else {
                    return;
                };
                let resolved = self.resolve_alias(slot_ty, scope);
                if !contains_nil(&resolved) {
                    return;
                }
                if let Some(narrowed) = without_nil(&resolved) {
                    let original = slot_ty.clone();
                    scope.define_var(name, Some(narrowed));
                    scope.narrowed_vars.insert(name.clone(), Some(original));
                }
            }
            Node::PropertyAccess { .. } | Node::OptionalPropertyAccess { .. } => {
                if let Some(key) = reference_path_key(target) {
                    scope.set_narrowed_path(&key, PathNarrowing::Remove("nil".into()));
                }
            }
            _ => {}
        }
    }
}
