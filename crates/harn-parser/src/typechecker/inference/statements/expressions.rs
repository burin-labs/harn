use super::*;

impl TypeChecker {
    pub(in crate::typechecker) fn iterable_item_type(
        &self,
        iter_type: &TypeExpr,
        scope: &TypeScope,
    ) -> InferredType {
        let resolved = self.resolve_alias(iter_type, scope);
        let non_nil = without_nil(&resolved)?;
        match self.resolve_alias(&non_nil, scope) {
            TypeExpr::List(inner)
            | TypeExpr::Iter(inner)
            | TypeExpr::Generator(inner)
            | TypeExpr::Stream(inner) => Some(*inner),
            TypeExpr::Applied { name, args } if name == "Iter" && args.len() == 1 => {
                Some(args[0].clone())
            }
            TypeExpr::DictType(key, value) => Some(TypeExpr::Applied {
                name: "Pair".into(),
                args: vec![*key, *value],
            }),
            TypeExpr::Named(name) if name == "string" => Some(TypeExpr::Named("string".into())),
            TypeExpr::Named(name) if name == "range" => Some(TypeExpr::Named("int".into())),
            TypeExpr::Union(members) => {
                let mut item_types = Vec::new();
                for member in members {
                    item_types.push(self.iterable_item_type(&member, scope)?);
                }
                Some(simplify_union(item_types))
            }
            _ => None,
        }
    }

    pub(super) fn check_compound_node_with_expected(
        &mut self,
        snode: &SNode,
        expected: &TypeExpr,
        scope: &mut TypeScope,
    ) -> bool {
        match (&snode.node, self.resolve_alias(expected, scope)) {
            (Node::DictLiteral(entries), TypeExpr::Shape(fields)) => {
                for entry in entries {
                    self.check_node(&entry.key, scope);
                    let expected_field = match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => {
                            fields.iter().find(|field| field.name == *key)
                        }
                        _ => None,
                    };
                    self.check_node_with_expected(
                        &entry.value,
                        expected_field.map(|field| &field.type_expr),
                        scope,
                    );
                }
                true
            }
            (Node::DictLiteral(entries), TypeExpr::DictType(_, value_type)) => {
                for entry in entries {
                    self.check_node(&entry.key, scope);
                    self.check_node_with_expected(&entry.value, Some(&value_type), scope);
                }
                true
            }
            (Node::ListLiteral(items), TypeExpr::List(item_type)) => {
                for item in items {
                    self.check_node_with_expected(item, Some(&item_type), scope);
                }
                true
            }
            (_, TypeExpr::Union(members)) => {
                let mut concrete_members = members
                    .iter()
                    .filter(|member| !self.type_is_nil(member, scope));
                let Some(member) = concrete_members.next() else {
                    self.check_node(snode, scope);
                    return true;
                };
                if concrete_members.next().is_none() {
                    let member = member.clone();
                    return self.check_compound_node_with_expected(snode, &member, scope);
                }
                false
            }
            _ => false,
        }
    }

    pub(super) fn expected_fn_parts(
        &self,
        expected: &TypeExpr,
        scope: &TypeScope,
    ) -> Option<(Vec<TypeExpr>, TypeExpr)> {
        let (params, mut return_type) = match self.resolve_alias(expected, scope) {
            TypeExpr::FnType {
                params,
                return_type,
            } => (params, *return_type),
            TypeExpr::Union(members) => {
                let mut fn_types = members.into_iter().filter_map(|member| match member {
                    TypeExpr::FnType {
                        params,
                        return_type,
                    } => Some((params, *return_type)),
                    member if self.type_is_nil(&member, scope) => None,
                    _ => None,
                });
                let first = fn_types.next()?;
                if fn_types.next().is_some() {
                    return None;
                }
                first
            }
            _ => return None,
        };
        if params
            .iter()
            .any(|param| self.contains_abstract_type(param, scope))
        {
            return None;
        }
        if self.contains_abstract_type(&return_type, scope) {
            return_type = Self::wildcard_type();
        }
        Some((params, return_type))
    }

    pub(in crate::typechecker) fn can_check_contextual_closure(
        &self,
        snode: &SNode,
        expected: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        matches!(snode.node, Node::Closure { .. })
            && self.expected_fn_parts(expected, scope).is_some()
    }

    pub(super) fn check_contextual_closure(
        &mut self,
        snode: &SNode,
        expected: &TypeExpr,
        scope: &mut TypeScope,
    ) -> bool {
        let Node::Closure { params, body, .. } = &snode.node else {
            return false;
        };
        let Some((expected_params, expected_return)) = self.expected_fn_parts(expected, scope)
        else {
            return false;
        };

        let expected_fn = TypeExpr::FnType {
            params: expected_params.clone(),
            return_type: Box::new(expected_return.clone()),
        };
        let actual_fn = TypeExpr::FnType {
            params: params
                .iter()
                .enumerate()
                .map(|(idx, param)| {
                    param
                        .type_expr
                        .clone()
                        .or_else(|| expected_params.get(idx).cloned())
                        .unwrap_or_else(Self::wildcard_type)
                })
                .collect(),
            return_type: Box::new(expected_return.clone()),
        };
        if params.len() != expected_params.len()
            || !self.types_compatible(&expected_fn, &actual_fn, scope)
        {
            self.type_mismatch_at(
                Code::TypeMismatch,
                "closure parameters",
                &expected_fn,
                &actual_fn,
                snode.span,
                (None, Some(snode.span)),
                scope,
            );
        }

        let mut closure_scope = scope.child();
        for (idx, param) in params.iter().enumerate() {
            let param_ty = param.type_expr.clone().or_else(|| {
                expected_params
                    .get(idx)
                    .filter(|ty| !Self::contains_wildcard_type(ty))
                    .cloned()
            });
            let is_typed = param_ty
                .as_ref()
                .is_some_and(|ty| !Self::contains_wildcard_type(ty));
            closure_scope.define_var(&param.name, param_ty);
            if is_typed {
                closure_scope.mark_annotated(&param.name);
            }
            closure_scope.clear_nil_widenable(&param.name);
        }

        self.fn_depth += 1;
        let saved_stream_depth = self.stream_fn_depth;
        let saved_stream_emit_types = self.stream_emit_types.clone();
        self.stream_fn_depth = 0;
        self.stream_emit_types.clear();
        self.expected_return_types
            .push(Some(expected_return.clone()));
        self.check_block(body, &mut closure_scope);
        self.expected_return_types.pop();
        self.stream_fn_depth = saved_stream_depth;
        self.stream_emit_types = saved_stream_emit_types;
        self.fn_depth -= 1;

        let mut ret_scope = closure_scope.clone();
        ret_scope.restore_narrowed_vars();
        for stmt in body {
            self.check_return_type(stmt, &expected_return, snode.span, &mut ret_scope);
        }
        if !matches!(
            body.last().map(|stmt| &stmt.node),
            Some(Node::ReturnStmt { .. })
        ) {
            let actual_return = self
                .infer_closure_body_return(body, &ret_scope)
                .unwrap_or_else(|| TypeExpr::Named("nil".into()));
            if !self.types_compatible(&expected_return, &actual_return, &ret_scope) {
                let value_span = body.last().map(|stmt| stmt.span).unwrap_or(snode.span);
                self.type_mismatch_at(
                    Code::ClosureReturnTypeMismatch,
                    "closure return value",
                    &expected_return,
                    &actual_return,
                    value_span,
                    (
                        Some((snode.span, "closure expected here".to_string())),
                        Some(value_span),
                    ),
                    &ret_scope,
                );
            }
        }

        true
    }

    pub(super) fn check_method_args_with_expected(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        scope: &mut TypeScope,
    ) {
        let expected_args = self.method_expected_arg_types(object, method, args, scope);
        for (idx, arg) in args.iter().enumerate() {
            self.check_node_with_expected(
                arg,
                expected_args
                    .get(idx)
                    .and_then(|expected| expected.as_ref()),
                scope,
            );
        }
    }

    pub(super) fn method_expected_arg_types(
        &self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Vec<Option<TypeExpr>> {
        let mut expected = vec![None; args.len()];
        let Some(object_type) = self.infer_type(object, scope) else {
            return expected;
        };
        let resolved = self.resolve_alias(&object_type, scope);
        let dict_value = Self::dict_value_type(&resolved);
        let item_type = if dict_value.is_some() {
            None
        } else {
            self.iterable_item_type(&resolved, scope)
        };

        let callback = |params: Vec<TypeExpr>, return_type: TypeExpr| TypeExpr::FnType {
            params,
            return_type: Box::new(return_type),
        };
        let wildcard = Self::wildcard_type;

        if let Some(value_type) = dict_value {
            match method {
                "filter" | "any" | "all" | "find" if !expected.is_empty() => {
                    expected[0] = Some(callback(vec![value_type], TypeExpr::Named("bool".into())));
                }
                "map_values" if !expected.is_empty() => {
                    expected[0] = Some(callback(vec![value_type], wildcard()));
                }
                _ => {}
            }
            return expected;
        }

        let Some(item_type) = item_type else {
            return expected;
        };
        match method {
            "map" | "flat_map" | "for_each" if !expected.is_empty() => {
                expected[0] = Some(callback(vec![item_type], wildcard()));
            }
            "filter" | "take_while" | "skip_while" | "any" | "all" | "find"
                if !expected.is_empty() =>
            {
                expected[0] = Some(callback(vec![item_type], TypeExpr::Named("bool".into())));
            }
            "reduce" if expected.len() > 1 => {
                let acc_type = args
                    .first()
                    .and_then(|arg| self.infer_type(arg, scope))
                    .unwrap_or_else(wildcard);
                expected[1] = Some(callback(vec![acc_type.clone(), item_type], acc_type));
            }
            _ => {}
        }
        expected
    }

    pub(super) fn dict_value_type(ty: &TypeExpr) -> InferredType {
        match ty {
            TypeExpr::DictType(_, value) => Some((**value).clone()),
            TypeExpr::Shape(fields) => {
                let members = fields.iter().map(|field| field.type_expr.clone()).collect();
                collapse_members_opt(members, TypeExpr::Union)
            }
            TypeExpr::Named(name) if name == "dict" => Some(Self::wildcard_type()),
            _ => None,
        }
    }
}
