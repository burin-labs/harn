use super::*;

impl TypeChecker {
    pub(super) fn stmt_definitely_exits(stmt: &SNode) -> bool {
        stmt_definitely_exits(stmt)
    }

    /// Define the variables introduced by a destructuring pattern, inferring
    /// each binding's type **identically to the hand-written `?.`/`??` form** it
    /// desugars to. `source_ty` is the inferred type of the value being
    /// destructured (the dict/list on the RHS, or each element's type for a
    /// `for`-`in` pattern).
    ///
    /// For a dict field `{ key = default }`, the binding type is computed as
    /// `infer_binary_op_type("??", source?.key, default)` — the same pipeline
    /// the desugared `let key = source?.key ?? default` would produce, including
    /// the untyped-`dict` case where `source?.key` is unknown and the default's
    /// type carries through. A field without a default keeps the optional
    /// `source?.key` type (matching a bare `let key = source?.key`).
    ///
    /// `source_ty == &None` reproduces the previous behavior of leaving every
    /// binding untyped.
    pub(super) fn define_pattern_vars_typed(
        &mut self,
        pattern: &BindingPattern,
        source_ty: &InferredType,
        scope: &mut TypeScope,
        mutable: bool,
    ) {
        let define = |scope: &mut TypeScope, name: &str, ty: InferredType| {
            if mutable {
                scope.define_var_mutable(name, ty);
            } else {
                scope.define_var(name, ty);
            }
            scope.clear_nil_widenable(name);
        };
        match pattern {
            BindingPattern::Identifier(name) => {
                // Not reached from let/var/for-in (those handle identifiers
                // before delegating here), but the whole value is the binding.
                define(scope, name, source_ty.clone());
            }
            BindingPattern::Dict(fields) => {
                for field in fields {
                    let name = field.alias.as_deref().unwrap_or(&field.key);
                    let ty = if field.is_rest {
                        // `...rest` collects the un-destructured keys into a new
                        // dict; a parameterized source keeps its value typing.
                        match source_ty.as_ref().map(|t| self.resolve_alias(t, scope)) {
                            Some(TypeExpr::DictType(k, v)) => Some(TypeExpr::DictType(k, v)),
                            _ => Some(TypeExpr::Named("dict".into())),
                        }
                    } else {
                        // `source?.key` — optional access mirrors the runtime
                        // "missing key binds nil" semantics.
                        let field_ty = source_ty.as_ref().and_then(|t| {
                            self.infer_property_type_from_type(t, &field.key, scope, true)
                        });
                        match &field.default_value {
                            Some(default) => {
                                let default_ty = self.infer_type(default, scope);
                                infer_binary_op_type("??", &field_ty, &default_ty)
                            }
                            None => field_ty,
                        }
                    };
                    define(scope, name, ty);
                }
            }
            BindingPattern::List(elements) => {
                // Homogeneous element type. Positional/tuple-precise element
                // typing is deferred — see destructuring_inference @xfail.
                let elem_ty = source_ty
                    .as_ref()
                    .and_then(|t| self.iterable_item_type(t, scope));
                for elem in elements {
                    let ty = if elem.is_rest {
                        // `...rest` collects the remaining elements into a new
                        // list with the same element type as the source.
                        match &elem_ty {
                            Some(inner) => Some(TypeExpr::List(Box::new(inner.clone()))),
                            None => Some(TypeExpr::Named("list".into())),
                        }
                    } else {
                        match &elem.default_value {
                            Some(default) => {
                                let default_ty = self.infer_type(default, scope);
                                infer_binary_op_type("??", &elem_ty, &default_ty)
                            }
                            None => elem_ty.clone(),
                        }
                    };
                    define(scope, &elem.name, ty);
                }
            }
            BindingPattern::Pair(a, b) => {
                define(scope, a, None);
                define(scope, b, None);
            }
        }
    }

    pub(super) fn check_pattern_defaults(
        &mut self,
        pattern: &BindingPattern,
        scope: &mut TypeScope,
    ) {
        match pattern {
            BindingPattern::Identifier(_) => {}
            BindingPattern::Dict(fields) => {
                for field in fields {
                    if let Some(default) = &field.default_value {
                        self.check_binops(default, scope);
                    }
                }
            }
            BindingPattern::List(elements) => {
                for elem in elements {
                    if let Some(default) = &elem.default_value {
                        self.check_binops(default, scope);
                    }
                }
            }
            BindingPattern::Pair(_, _) => {}
        }
    }

    pub(super) fn is_nil_type(ty: &TypeExpr) -> bool {
        matches!(ty, TypeExpr::Named(name) if name == "nil")
    }

    pub(super) fn union_with_nil(ty: &TypeExpr) -> TypeExpr {
        if Self::is_nil_type(ty) {
            return ty.clone();
        }
        match ty {
            TypeExpr::Union(members) if members.iter().any(Self::is_nil_type) => ty.clone(),
            TypeExpr::Union(members) => {
                let mut widened = members.clone();
                widened.push(TypeExpr::Named("nil".into()));
                TypeExpr::Union(widened)
            }
            other => TypeExpr::Union(vec![other.clone(), TypeExpr::Named("nil".into())]),
        }
    }
}
