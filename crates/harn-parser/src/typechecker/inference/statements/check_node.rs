use super::*;

impl TypeChecker {
    pub(in crate::typechecker) fn check_node(&mut self, snode: &SNode, scope: &mut TypeScope) {
        let span = snode.span;
        match &snode.node {
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let context_checked =
                    self.check_node_with_expected(value, type_ann.as_ref(), scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if !context_checked {
                            if let Some(actual) = &inferred {
                                if !self.types_compatible(expected, actual, scope) {
                                    self.type_mismatch_at(
                                        Code::VariableTypeMismatch,
                                        format!("let binding `{name}`"),
                                        expected,
                                        actual,
                                        value.span,
                                        (
                                            Some((span, "expected type declared here".to_string())),
                                            Some(value.span),
                                        ),
                                        scope,
                                    );
                                }
                            }
                        }
                    }
                    // Collect inlay hint when type is inferred (no annotation)
                    if type_ann.is_none() && !is_discard_name(name) {
                        if let Some(ref ty) = inferred {
                            if !is_obvious_type(value, ty) {
                                self.hints.push(InlayHintInfo {
                                    line: span.line,
                                    column: span.column + "let ".len() + name.len(),
                                    label: format!(": {}", format_type(ty)),
                                });
                            }
                        }
                    }
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var(name, ty);
                    if type_ann.is_some() {
                        scope.mark_annotated(name);
                    }
                    scope.clear_nil_widenable(name);
                    scope.define_schema_binding(name, schema_type_expr_from_node(value, scope));
                    // Strict types: mark variables assigned from boundary APIs
                    if self.strict_types {
                        if let Some(boundary) = Self::detect_boundary_source(value, scope) {
                            let has_concrete_ann =
                                type_ann.as_ref().is_some_and(Self::is_concrete_type);
                            if !has_concrete_ann {
                                scope.mark_untyped_source(name, &boundary);
                            }
                        }
                    }
                } else {
                    self.check_pattern_defaults(pattern, scope);
                    self.define_pattern_vars_typed(pattern, &inferred, scope, false);
                }
            }

            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let context_checked =
                    self.check_node_with_expected(value, type_ann.as_ref(), scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if !context_checked {
                            if let Some(actual) = &inferred {
                                if !self.types_compatible(expected, actual, scope) {
                                    self.type_mismatch_at(
                                        Code::VariableTypeMismatch,
                                        format!("var binding `{name}`"),
                                        expected,
                                        actual,
                                        value.span,
                                        (
                                            Some((span, "expected type declared here".to_string())),
                                            Some(value.span),
                                        ),
                                        scope,
                                    );
                                }
                            }
                        }
                    }
                    if type_ann.is_none() && !is_discard_name(name) {
                        if let Some(ref ty) = inferred {
                            if !is_obvious_type(value, ty) {
                                self.hints.push(InlayHintInfo {
                                    line: span.line,
                                    column: span.column + "var ".len() + name.len(),
                                    label: format!(": {}", format_type(ty)),
                                });
                            }
                        }
                    }
                    let inferred_is_nil =
                        type_ann.is_none() && inferred.as_ref().is_some_and(Self::is_nil_type);
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var_mutable(name, ty);
                    if type_ann.is_some() {
                        scope.mark_annotated(name);
                    }
                    if inferred_is_nil {
                        scope.mark_nil_widenable(name);
                    } else {
                        scope.clear_nil_widenable(name);
                    }
                    scope.define_schema_binding(name, schema_type_expr_from_node(value, scope));
                    // Strict types: mark variables assigned from boundary APIs
                    if self.strict_types {
                        if let Some(boundary) = Self::detect_boundary_source(value, scope) {
                            let has_concrete_ann =
                                type_ann.as_ref().is_some_and(Self::is_concrete_type);
                            if !has_concrete_ann {
                                scope.mark_untyped_source(name, &boundary);
                            }
                        }
                    }
                } else {
                    self.check_pattern_defaults(pattern, scope);
                    self.define_pattern_vars_typed(pattern, &inferred, scope, true);
                }
            }

            Node::ConstBinding {
                name,
                type_ann,
                value,
            } => {
                // Walk and infer the value just like a let-binding so
                // existing diagnostics (undefined names, type mismatches)
                // still fire. The bounded const-eval pass below runs on
                // top of that — its failures land as HARN-MET-* /
                // HARN-CST-* diagnostics.
                let context_checked =
                    self.check_node_with_expected(value, type_ann.as_ref(), scope);
                let inferred = self.infer_type(value, scope);
                if let Some(expected) = type_ann {
                    if !context_checked {
                        if let Some(actual) = &inferred {
                            if !self.types_compatible(expected, actual, scope) {
                                self.type_mismatch_at(
                                    Code::VariableTypeMismatch,
                                    format!("const binding `{name}`"),
                                    expected,
                                    actual,
                                    value.span,
                                    (
                                        Some((span, "expected type declared here".to_string())),
                                        Some(value.span),
                                    ),
                                    scope,
                                );
                            }
                        }
                    }
                }
                let ty = type_ann.clone().or(inferred);
                scope.define_var(name, ty);
                if type_ann.is_some() {
                    scope.mark_annotated(name);
                }
                scope.clear_nil_widenable(name);

                // Run the bounded sandbox interpreter. A successful fold
                // registers the value for later const initializers in
                // the same module; a failure emits a diagnostic keyed
                // off the failure kind so editor/CLI integrations can
                // dispatch on it.
                match crate::const_eval::const_eval(value, &self.const_env) {
                    Ok(folded) => {
                        self.const_env.insert(name.clone(), folded);
                    }
                    Err(err) => {
                        use crate::const_eval::ConstEvalErrorKind as K;
                        let message =
                            format!("const `{name}` initializer rejected: {}", err.detail);
                        match err.kind {
                            K::Disallowed => self.error_at(
                                Code::ConstEvalDisallowedExpression,
                                message,
                                err.span,
                            ),
                            K::StepLimit => {
                                self.error_at(Code::ConstEvalStepLimit, message, err.span);
                            }
                            K::RecursionLimit => {
                                self.error_at(Code::ConstEvalRecursionLimit, message, err.span);
                            }
                            K::SandboxViolation => {
                                self.error_at(Code::ConstEvalSandboxViolation, message, err.span);
                            }
                            K::RuntimeError => {
                                self.error_at(Code::ConstEvalRuntimeError, message, err.span);
                            }
                        }
                    }
                }
            }

            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_stream,
                ..
            } => {
                let callable_return_type =
                    Self::callable_return_type(*is_stream, return_type, body)
                        .or_else(|| self.infer_unannotated_fn_return(params, body));
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: callable_return_type,
                    definition_span: Some(span),
                    type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                    required_params,
                    where_clauses: where_clauses
                        .iter()
                        .map(|wc| (wc.type_name.clone(), wc.bound.clone()))
                        .collect(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_fn_decl_variance(type_params, params, return_type.as_ref(), name, span);
                self.check_fn_body(
                    type_params,
                    params,
                    return_type,
                    body,
                    where_clauses,
                    *is_stream,
                    span,
                );
            }

            Node::ToolDecl {
                name,
                params,
                return_type,
                body,
                ..
            } => {
                // Register the tool like a function for type checking purposes
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: return_type.clone(),
                    definition_span: Some(span),
                    type_param_names: Vec::new(),
                    required_params,
                    where_clauses: Vec::new(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_value_returning_body(
                    params,
                    return_type,
                    body,
                    span,
                    "tool result",
                    "tool return type declared here",
                );
            }

            Node::SkillDecl { name, fields, .. } => {
                // Skills lower to `skill_define(skill_registry(), name, { ... })`.
                // The bound variable holds a registry dict. Type-check each
                // field expression so references to tools/pipelines/fns get
                // checked like any other expression.
                for (_key, value) in fields {
                    self.check_node(value, scope);
                }
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
            }

            Node::EvalPackDecl {
                binding_name,
                fields,
                body,
                summarize,
                ..
            } => {
                for (_key, value) in fields {
                    self.check_node(value, scope);
                }
                scope.define_var(binding_name, Some(TypeExpr::Named("dict".into())));
                scope.clear_nil_widenable(binding_name);

                if !body.is_empty() || summarize.is_some() {
                    let mut eval_scope = scope.child();
                    eval_scope.define_var("id", Some(TypeExpr::Named("string".into())));
                    eval_scope.clear_nil_widenable("id");
                    eval_scope.define_var("version", Some(TypeExpr::Named("int".into())));
                    eval_scope.clear_nil_widenable("version");
                    for (field_name, value) in fields {
                        let field_type = self.infer_type(value, scope);
                        eval_scope.define_var(field_name, field_type);
                        eval_scope.clear_nil_widenable(field_name);
                    }
                    self.check_block(body, &mut eval_scope);
                    if let Some(summary_body) = summarize {
                        self.check_block(summary_body, &mut eval_scope);
                    }
                }
            }

            Node::FunctionCall {
                name,
                type_args,
                args,
            } => {
                self.check_call(name, type_args, args, scope, span);
                // `assert(cond, msg?)` throws when `cond` is falsy, so after the
                // call the truthy refinement of `cond` holds — narrow the
                // continuing scope like `require`/guard. Lets the idiomatic
                // `assert(x != nil)` then `x - 1` type-check.
                if name == "assert" {
                    if let Some(cond) = args.first() {
                        self.extract_refinements(cond, scope).apply_truthy(scope);
                    }
                }
                // Strict types: schema_expect clears untyped source status
                if self.strict_types && name == "schema_expect" && args.len() >= 2 {
                    if let Node::Identifier(var_name) = &args[0].node {
                        scope.clear_untyped_source(var_name);
                        if let Some(schema_type) = schema_type_expr_from_node(&args[1], scope) {
                            scope.define_var(var_name, Some(schema_type));
                        }
                    }
                }
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.check_node(condition, scope);
                let refs = self.extract_refinements_with_lint(condition, scope);

                let mut then_scope = scope.child();
                refs.apply_truthy(&mut then_scope);
                // Strict types: schema_is/is_type in condition clears
                // untyped source in then-branch
                if self.strict_types {
                    if let Node::FunctionCall { name, args, .. } = &condition.node {
                        if (name == "schema_is" || name == "is_type") && args.len() == 2 {
                            if let Node::Identifier(var_name) = &args[0].node {
                                then_scope.clear_untyped_source(var_name);
                            }
                        }
                    }
                }
                self.check_block(then_body, &mut then_scope);

                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    refs.apply_falsy(&mut else_scope);
                    self.check_block(else_body, &mut else_scope);

                    // Post-branch narrowing: if one branch definitely exits,
                    // apply the other branch's refinements to the outer scope
                    if Self::block_definitely_exits(then_body)
                        && !Self::block_definitely_exits(else_body)
                    {
                        refs.apply_falsy(scope);
                    } else if Self::block_definitely_exits(else_body)
                        && !Self::block_definitely_exits(then_body)
                    {
                        refs.apply_truthy(scope);
                    }
                } else {
                    // No else: if then-body always exits, apply falsy after
                    if Self::block_definitely_exits(then_body) {
                        refs.apply_falsy(scope);
                    }
                }
            }

            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.check_node(iterable, scope);
                let mut loop_scope = scope.child();
                let iter_type = self.infer_type(iterable, scope);
                if let BindingPattern::Identifier(variable) = pattern {
                    let elem_type = iter_type
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope));
                    loop_scope.define_var(variable, elem_type);
                    loop_scope.clear_nil_widenable(variable);
                } else if let BindingPattern::Pair(a, b) = pattern {
                    // Pair destructuring: `for (k, v) in iter` — extract K, V
                    // from the yielded Pair<K, V>.
                    let (ka, vb) = iter_type
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope))
                        .and_then(|ty| {
                            if let TypeExpr::Applied { name, args } = ty {
                                (name == "Pair" && args.len() == 2)
                                    .then(|| (Some(args[0].clone()), Some(args[1].clone())))
                            } else {
                                None
                            }
                        })
                        .unwrap_or((None, None));
                    loop_scope.define_var(a, ka);
                    loop_scope.define_var(b, vb);
                    loop_scope.clear_nil_widenable(a);
                    loop_scope.clear_nil_widenable(b);
                } else {
                    // Each iteration binds the element; destructure against its
                    // type so dict/list patterns in `for`-`in` gain the same
                    // inference as in `let`/`var`.
                    let elem_source = iter_type
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope));
                    self.check_pattern_defaults(pattern, &mut loop_scope);
                    self.define_pattern_vars_typed(pattern, &elem_source, &mut loop_scope, false);
                }
                self.check_block(body, &mut loop_scope);
            }

            Node::WhileLoop { condition, body } => {
                self.check_node(condition, scope);
                let refs = self.extract_refinements_with_lint(condition, scope);
                let mut loop_scope = scope.child();
                refs.apply_truthy(&mut loop_scope);
                self.check_block(body, &mut loop_scope);
            }

            Node::RequireStmt { condition, message } => {
                self.check_node(condition, scope);
                if let Some(message) = message {
                    self.check_node(message, scope);
                }
                // `require cond` throws when `cond` is falsy, so after it the
                // truthy refinement holds — narrow the continuing scope just
                // like a guard's else-diverges case (e.g. `require x != nil`
                // then `x + 1`).
                self.extract_refinements(condition, scope)
                    .apply_truthy(scope);
            }

            Node::TryCatch {
                has_catch: _,
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
                ..
            } => {
                let mut try_scope = scope.child();
                self.check_block(body, &mut try_scope);
                let mut catch_scope = scope.child();
                if let Some(var) = error_var {
                    catch_scope.define_var(var, error_type.clone());
                    catch_scope.clear_nil_widenable(var);
                }
                self.check_block(catch_body, &mut catch_scope);
                if let Some(fb) = finally_body {
                    let mut finally_scope = scope.child();
                    self.check_block(fb, &mut finally_scope);
                }
            }

            Node::TryExpr { body } => {
                let mut try_scope = scope.child();
                self.check_block(body, &mut try_scope);
            }

            Node::TryStar { operand } => {
                if self.fn_depth == 0 {
                    self.error_at(Code::TryOutsideFunction,
                        "try* requires an enclosing function (fn, tool, or pipeline) so the rethrow has a target".to_string(),
                        span,
                    );
                }
                self.check_node(operand, scope);
            }

            Node::ReturnStmt {
                value: Some(val), ..
            } => {
                let expected_return = self.expected_return_types.last().and_then(|ty| ty.clone());
                self.check_node_with_expected(val, expected_return.as_ref(), scope);
            }

            Node::Assignment {
                target, value, op, ..
            } => {
                let expected_value_type = if op.is_none() {
                    if let Node::Identifier(name) = &target.node {
                        scope.get_var(name).cloned().flatten()
                    } else {
                        None
                    }
                } else {
                    None
                };
                let context_checked =
                    self.check_node_with_expected(value, expected_value_type.as_ref(), scope);
                if let Node::Identifier(name) = &target.node {
                    let mut widened_slot_type: Option<TypeExpr> = None;
                    // Compile-time immutability check
                    if scope.get_var(name).is_some() && !scope.is_mutable(name) {
                        self.warning_at(Code::ImmutableAssignment,
                            format!(
                                "Cannot assign to '{name}': variable is immutable (declared with 'let')"
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
                                // Check against the original (pre-narrowing) type if narrowed
                                let check_type = scope
                                    .narrowed_vars
                                    .get(name)
                                    .and_then(|t| t.as_ref())
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

                    // Invalidate narrowing on reassignment: restore original type
                    if let Some(original) = scope.narrowed_vars.remove(name) {
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
                    // Reassigning the base drops any path narrowing (and path
                    // exhaustiveness ledger) that read through it —
                    // `entry.arguments` is stale once `entry` is.
                    scope.clear_narrowed_paths_rooted_at(name);
                    scope.clear_unknown_ruled_out_paths_rooted_at(name);
                } else if let Some(base) = Self::assignment_target_root(target) {
                    // Mutating a path (`entry.arguments = ...`, `o.a[i] = ...`)
                    // can invalidate any narrowing rooted at the same base, so
                    // conservatively drop them all.
                    scope.clear_narrowed_paths_rooted_at(base);
                    scope.clear_unknown_ruled_out_paths_rooted_at(base);
                }
                // Assignment narrowing: a statically non-nil value flowing into
                // a nilable binding/path narrows it to non-nil for subsequent
                // reads, until the next reassignment. Runs after the clears
                // above so it isn't immediately wiped. Only for plain `=`.
                if op.is_none() {
                    self.narrow_after_assignment(target, value, scope);
                }
            }

            Node::TypeDecl {
                name,
                type_params,
                type_expr,
            } => {
                scope.type_aliases.insert(
                    name.clone(),
                    TypeAliasInfo {
                        type_params: type_params.clone(),
                        body: type_expr.clone(),
                    },
                );
                self.check_type_alias_decl_variance(type_params, type_expr, name, span);
            }

            Node::EnumDecl {
                name,
                type_params,
                variants,
                ..
            } => {
                scope.enums.insert(
                    name.clone(),
                    EnumDeclInfo {
                        type_params: type_params.clone(),
                        variants: variants.clone(),
                    },
                );
                self.check_enum_decl_variance(type_params, variants, name, span);
            }

            Node::StructDecl {
                name,
                type_params,
                fields,
                ..
            } => {
                scope.structs.insert(
                    name.clone(),
                    StructDeclInfo {
                        type_params: type_params.clone(),
                        fields: fields.clone(),
                    },
                );
                self.check_struct_decl_variance(type_params, fields, name, span);
            }

            Node::InterfaceDecl {
                name,
                type_params,
                associated_types,
                methods,
            } => {
                scope.interfaces.insert(
                    name.clone(),
                    InterfaceDeclInfo {
                        type_params: type_params.clone(),
                        associated_types: associated_types.clone(),
                        methods: methods.clone(),
                    },
                );
                self.check_interface_decl_variance(type_params, methods, name, span);
            }

            Node::ImplBlock {
                type_name, methods, ..
            } => {
                // Register impl methods for interface satisfaction checking
                let sigs: Vec<ImplMethodSig> = methods
                    .iter()
                    .filter_map(|m| {
                        if let Node::FnDecl {
                            name,
                            params,
                            return_type,
                            ..
                        } = &m.node
                        {
                            let non_self: Vec<_> =
                                params.iter().filter(|p| p.name != "self").collect();
                            let param_count = non_self.len();
                            let param_types: Vec<Option<TypeExpr>> =
                                non_self.iter().map(|p| p.type_expr.clone()).collect();
                            Some(ImplMethodSig {
                                name: name.clone(),
                                param_count,
                                param_types,
                                return_type: return_type.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();
                scope.impl_methods.insert(type_name.clone(), sigs);
                for method_sn in methods {
                    self.check_node(method_sn, scope);
                }
            }

            Node::TryOperator { operand } => {
                self.check_node(operand, scope);
            }

            Node::MatchExpr { value, arms } => {
                self.check_node(value, scope);
                let value_type = self.infer_type(value, scope);
                for arm in arms {
                    self.check_node(&arm.pattern, scope);
                    // Check for incompatible literal pattern types —
                    // once per alternative inside an OrPattern so
                    // mixed-type or-patterns still surface the warning.
                    if let Some(ref vt) = value_type {
                        let value_type_name = format_type(vt);
                        for leaf in pattern_alternatives(&arm.pattern) {
                            let mismatch = match &leaf.node {
                                Node::StringLiteral(_) => !self.types_compatible(
                                    vt,
                                    &TypeExpr::Named("string".into()),
                                    scope,
                                ),
                                Node::IntLiteral(_) => {
                                    !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("int".into()),
                                        scope,
                                    ) && !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("float".into()),
                                        scope,
                                    )
                                }
                                Node::FloatLiteral(_) => {
                                    !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("float".into()),
                                        scope,
                                    ) && !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("int".into()),
                                        scope,
                                    )
                                }
                                Node::BoolLiteral(_) => !self.types_compatible(
                                    vt,
                                    &TypeExpr::Named("bool".into()),
                                    scope,
                                ),
                                _ => false,
                            };
                            if mismatch {
                                let pattern_type = match &leaf.node {
                                    Node::StringLiteral(_) => "string",
                                    Node::IntLiteral(_) => "int",
                                    Node::FloatLiteral(_) => "float",
                                    Node::BoolLiteral(_) => "bool",
                                    _ => unreachable!(),
                                };
                                self.warning_at(Code::InvalidMatchPattern,
                                    format!(
                                        "Match pattern type mismatch: matching {value_type_name} against {pattern_type} literal"
                                    ),
                                    leaf.span,
                                );
                            }
                        }
                    }
                    let mut arm_scope = scope.child();
                    // Narrow the matched value's type in each arm. For an
                    // OrPattern we narrow once per alternative and combine
                    // the results into a union, so `"pass" | "fail"` on a
                    // `"pass" | "fail" | "skip"` union refines to
                    // `"pass" | "fail"` inside the arm.
                    if let Node::Identifier(var_name) = &value.node {
                        if let Some(Some(TypeExpr::Union(members))) = scope.get_var(var_name) {
                            let narrowed = narrow_union_by_arm_pattern(&arm.pattern, members);
                            if let Some(narrowed_type) = narrowed {
                                arm_scope.define_var(var_name, Some(narrowed_type));
                            }
                        }
                    }

                    // Discriminator narrowing on `match obj.<tag> { "v" -> ... }`:
                    // when the matched value is a property access on a tagged
                    // shape union and the arm is a literal pattern (or an
                    // or-pattern of literals) matching the union's
                    // auto-detected discriminant, narrow `obj` to the
                    // matching variant(s) inside the arm body.
                    if let Node::PropertyAccess { object, property } = &value.node {
                        if let Node::Identifier(obj_var) = &object.node {
                            if let Some(Some(raw_type)) = scope.get_var(obj_var).cloned() {
                                let resolved = self.resolve_alias(&raw_type, scope);
                                if let TypeExpr::Union(members) = resolved {
                                    let members = resolve_union_shape_members(&members, scope);
                                    if discriminant_field(&members).as_deref()
                                        == Some(property.as_str())
                                    {
                                        let narrowed = narrow_shape_union_by_arm_pattern(
                                            &arm.pattern,
                                            &members,
                                            property,
                                        );
                                        if let Some(t) = narrowed {
                                            arm_scope.define_var(obj_var, Some(t));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Bind the arm's pattern variables (list/dict destructuring,
                    // including `[a, ...rest]`) with their refined types so the
                    // guard and body are type-checked against them, not against
                    // gradual `unknown`. Enum/variant patterns are excluded: a
                    // variant field can be an unsubstituted generic parameter
                    // (e.g. the `E` in `Result.Err(e)`), and binding it as a
                    // concrete type here yields false positives — proper enum
                    // binding needs type-argument substitution from the matched
                    // value, which is not wired through this path.
                    if !matches!(
                        &arm.pattern.node,
                        Node::EnumConstruct { .. } | Node::MethodCall { .. }
                    ) {
                        self.define_match_pattern_bindings(
                            &arm.pattern,
                            value_type.as_ref(),
                            &mut arm_scope,
                        );
                    }
                    // `match type_of(subject) { "T" -> … }` narrows the subject
                    // in the arm — independent of the pattern-binding gate above.
                    self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
                    if let Some(ref guard) = arm.guard {
                        self.check_node(guard, &mut arm_scope);
                    }
                    self.check_block(&arm.body, &mut arm_scope);
                }
                self.check_match_exhaustiveness(value, arms, scope, span);
            }

            // Recurse into nested expressions + validate binary op types
            Node::BinaryOp { op, left, right } => {
                self.check_node(left, scope);
                if op == "&&" || op == "||" {
                    let refs = self.extract_refinements(left, scope);
                    let mut right_scope = scope.child();
                    if op == "&&" {
                        refs.apply_truthy(&mut right_scope);
                    } else {
                        refs.apply_falsy(&mut right_scope);
                    }
                    self.check_node(right, &mut right_scope);
                    return;
                }
                self.check_node(right, scope);
                // Validate operator/type compatibility
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                // A nil-able operand to an arithmetic / concatenation operator
                // is a definite runtime fault (`nil + 1`, `nil * 2`, … all
                // throw). Surface it at check time. Equality (`==`/`!=`) is
                // nil-safe and the short-circuit logical ops already returned
                // above. A *pure* `nil` operand is left to the `named_pair` arm
                // below (it reports "can't add nil and int"); here we only
                // catch the union case (`int?`, where `named_pair` is `None`),
                // skipping gradual remainders (`any?`). Assignment / guard
                // narrowing has already run, so a value proven non-nil by an
                // earlier `=` or `!= nil` check is not flagged.
                if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%" | "**") {
                    for (ty, operand) in [(&lt, left), (&rt, right)] {
                        let Some(ty) = ty else { continue };
                        let resolved = self.resolve_alias(ty, scope);
                        if !contains_nil(&resolved) {
                            continue;
                        }
                        let Some(non_nil) = without_nil(&resolved) else {
                            continue;
                        };
                        if matches!(&non_nil, TypeExpr::Named(n) if is_gradual_type_name(n)) {
                            continue;
                        }
                        self.error_at(
                            Code::InvalidBinaryOperator,
                            format!(
                                "operand of '{op}' may be nil (type {}); handle nil first or \
                                 provide a default with `??`",
                                format_type(&resolved)
                            ),
                            operand.span,
                        );
                    }
                }
                let named_pair = match (&lt, &rt) {
                    // Gradual operands (`any`/`unknown`/`_`) are compatible with
                    // every operator; the actual check happens at runtime.
                    // `number` is the `int | float` alias, so treat it as a
                    // union operand (falls through to the gradual arm) instead
                    // of a concrete name — matching how an explicit
                    // `int | float` operand is already handled.
                    (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r)))
                        if !is_gradual_type_name(l)
                            && !is_gradual_type_name(r)
                            && l != "number"
                            && r != "number" =>
                    {
                        Some((l, r))
                    }
                    _ => None,
                };
                if let Some((l, r)) = named_pair {
                    match op.as_str() {
                        "-" | "/" | "%" if !super::super::binary_ops::numeric_binop_ok(l, r) => {
                            self.error_at(
                                Code::InvalidBinaryOperator,
                                format!("can't use '{op}' on {l} and {r} (needs numeric operands)"),
                                span,
                            );
                        }
                        "**" => {
                            // Exponentiation is int/float only; `decimal` has no
                            // `**` at runtime (convert explicitly if needed).
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.error_at(
                                    Code::InvalidBinaryOperator,
                                    format!(
                                        "can't use '{op}' on {l} and {r} (needs numeric operands)"
                                    ),
                                    span,
                                );
                            }
                        }
                        "*" => {
                            let is_numeric = super::super::binary_ops::numeric_binop_ok(l, r);
                            let is_string_repeat =
                                (l == "string" && r == "int") || (l == "int" && r == "string");
                            if !is_numeric && !is_string_repeat {
                                self.error_at(
                                    Code::InvalidBinaryOperator,
                                    format!("can't multiply {l} and {r} (try string * int)"),
                                    span,
                                );
                            }
                        }
                        "+" => {
                            let valid = super::super::binary_ops::numeric_binop_ok(l, r)
                                || matches!(
                                    (l.as_str(), r.as_str()),
                                    ("string", "string") | ("list", "list") | ("dict", "dict")
                                );
                            if !valid {
                                let msg = format!("can't add {l} and {r}");
                                // Offer interpolation fix when one side is string
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(
                                        Code::StringInterpolationRewrite,
                                        msg,
                                        span,
                                        fix,
                                    );
                                } else {
                                    self.error_at(Code::InvalidBinaryOperator, msg, span);
                                }
                            }
                        }
                        "<" | ">" | "<=" | ">=" => {
                            let comparable = ["int", "float", "string", "decimal"];
                            if !comparable.contains(&l.as_str())
                                || !comparable.contains(&r.as_str())
                            {
                                self.warning_at(
                                    Code::InvalidBinaryOperator,
                                    format!(
                                        "Comparison '{op}' may not be meaningful for types {l} and {r}"
                                    ),
                                    span,
                                );
                            } else if (l == "string") != (r == "string")
                                || (l == "decimal") != (r == "decimal")
                            {
                                // `decimal` only orders against `decimal` at
                                // runtime (a decimal-vs-int/float comparison is
                                // unordered and yields false); flag the mix like
                                // the string-vs-non-string case.
                                self.warning_at(Code::InvalidBinaryOperator,
                                    format!(
                                        "Comparing {l} with {r} using '{op}' may give unexpected results"
                                    ),
                                    span,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            Node::UnaryOp { operand, .. } => {
                self.check_node(operand, scope);
            }
            Node::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.check_node(object, scope);
                self.check_method_receiver(object, method, scope, span, false);
                if self.check_harness_method_call(object, method, args, scope, span) {
                    return;
                }
                self.check_method_args_with_expected(object, method, args, scope);
                self.check_generic_method_bound(object, method, scope, span);
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.check_unnecessary_safe_method_call(snode, object, scope);
                self.check_node(object, scope);
                self.check_method_receiver(object, method, scope, span, true);
                if self.check_harness_method_call(object, method, args, scope, span) {
                    return;
                }
                self.check_method_args_with_expected(object, method, args, scope);
                self.check_generic_method_bound(object, method, scope, span);
            }
            Node::PropertyAccess { object, property } => {
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Property);
                self.check_property_access(object, property, scope, span, false);
                self.check_node(object, scope);
            }
            Node::OptionalPropertyAccess { object, property } => {
                self.check_unnecessary_safe_property_access(snode, object, property, scope);
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Property);
                self.check_property_access(object, property, scope, span, true);
                self.check_node(object, scope);
            }
            Node::SubscriptAccess { object, index } => {
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Subscript);
                self.check_subscript_access(object, scope, span, false);
                self.check_node(object, scope);
                self.check_node(index, scope);
            }
            Node::OptionalSubscriptAccess { object, index } => {
                self.check_unnecessary_safe_subscript_access(snode, object, scope);
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Subscript);
                self.check_subscript_access(object, scope, span, true);
                self.check_node(object, scope);
                self.check_node(index, scope);
            }
            Node::SliceAccess { object, start, end } => {
                self.check_node(object, scope);
                if let Some(s) = start {
                    self.check_node(s, scope);
                }
                if let Some(e) = end {
                    self.check_node(e, scope);
                }
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.check_node(condition, scope);
                let refs = self.extract_refinements_with_lint(condition, scope);

                let mut true_scope = scope.child();
                refs.apply_truthy(&mut true_scope);
                self.check_node(true_expr, &mut true_scope);

                let mut false_scope = scope.child();
                refs.apply_falsy(&mut false_scope);
                self.check_node(false_expr, &mut false_scope);
            }

            Node::ThrowStmt { value } => {
                self.check_node(value, scope);
                // A `throw` in the tail of a `type_of`-narrowing chain claims
                // exhaustiveness on the enclosing `unknown`-typed variable.
                // Warn if the claim isn't actually complete.
                self.check_unknown_exhaustiveness(scope, snode.span, "throw");
            }

            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.check_node(condition, scope);
                let refs = self.extract_refinements_with_lint(condition, scope);

                let mut else_scope = scope.child();
                refs.apply_falsy(&mut else_scope);
                self.check_block(else_body, &mut else_scope);

                // After guard, condition is true — apply truthy refinements
                // to the OUTER scope (guard's else-body must exit)
                refs.apply_truthy(scope);
            }

            Node::SpawnExpr { body } => {
                let mut spawn_scope = scope.child();
                self.check_block(body, &mut spawn_scope);
            }

            Node::HitlExpr { kind, args } => {
                self.check_hitl_expr(*kind, args, scope, span);
            }

            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                self.check_node(expr, scope);
                for (key, value) in options {
                    // `max_concurrent` must resolve to `int`; other keys
                    // are rejected by the parser, so no need to match
                    // here. Still type-check the expression so bad
                    // references surface a diagnostic.
                    self.check_node(value, scope);
                    if key == "max_concurrent" {
                        if let Some(ty) = self.infer_type(value, scope) {
                            if !matches!(ty, TypeExpr::Named(ref n) if n == "int") {
                                self.error_at(
                                    Code::OrchestrationType,
                                    format!(
                                        "`max_concurrent` on `parallel` must be int, got {ty:?}"
                                    ),
                                    value.span,
                                );
                            }
                        }
                    }
                }
                let mut par_scope = scope.child();
                if let Some(var) = variable {
                    let var_type = match mode {
                        ParallelMode::Count => Some(TypeExpr::Named("int".into())),
                        ParallelMode::Each | ParallelMode::EachStream | ParallelMode::Settle => {
                            match self.infer_type(expr, scope) {
                                Some(TypeExpr::List(inner)) => Some(*inner),
                                _ => None,
                            }
                        }
                    };
                    par_scope.define_var(var, var_type);
                    par_scope.clear_nil_widenable(var);
                }
                self.check_block(body, &mut par_scope);
            }

            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.check_node(&case.channel, scope);
                    let mut case_scope = scope.child();
                    case_scope.define_var(&case.variable, None);
                    case_scope.clear_nil_widenable(&case.variable);
                    self.check_block(&case.body, &mut case_scope);
                }
                if let Some((dur, body)) = timeout {
                    self.check_node(dur, scope);
                    let mut timeout_scope = scope.child();
                    self.check_block(body, &mut timeout_scope);
                }
                if let Some(body) = default_body {
                    let mut default_scope = scope.child();
                    self.check_block(body, &mut default_scope);
                }
            }

            Node::DeadlineBlock { duration, body } => {
                self.check_node(duration, scope);
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::MutexBlock { key, body } => {
                if let Some(key) = key {
                    self.check_node(key, scope);
                }
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::ScopeBlock { body } | Node::DeferStmt { body } => {
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::Retry { count, body } => {
                self.check_node(count, scope);
                let mut retry_scope = scope.child();
                self.check_block(body, &mut retry_scope);
            }

            Node::CostRoute { options, body } => {
                for (key, value) in options {
                    if matches!(
                        key.as_str(),
                        "fallback_strategy" | "strategy" | "quality" | "min_quality"
                    ) && matches!(value.node, Node::Identifier(_))
                    {
                        continue;
                    }
                    self.check_node(value, scope);
                }
                let mut route_scope = scope.child();
                self.check_block(body, &mut route_scope);
            }

            Node::Closure { params, body, .. } => {
                let mut closure_scope = scope.child();
                for p in params {
                    closure_scope.define_var(&p.name, p.type_expr.clone());
                    if p.type_expr
                        .as_ref()
                        .is_some_and(|ty| !Self::contains_wildcard_type(ty))
                    {
                        closure_scope.mark_annotated(&p.name);
                    }
                    closure_scope.clear_nil_widenable(&p.name);
                }
                self.fn_depth += 1;
                let saved_stream_depth = self.stream_fn_depth;
                let saved_stream_emit_types = self.stream_emit_types.clone();
                self.stream_fn_depth = 0;
                self.stream_emit_types.clear();
                self.expected_return_types.push(None);
                self.check_block(body, &mut closure_scope);
                self.expected_return_types.pop();
                self.stream_fn_depth = saved_stream_depth;
                self.stream_emit_types = saved_stream_emit_types;
                self.fn_depth -= 1;
            }

            Node::ListLiteral(elements) => {
                for elem in elements {
                    self.check_node(elem, scope);
                }
            }

            Node::DictLiteral(entries) => {
                for entry in entries {
                    self.check_node(&entry.key, scope);
                    self.check_node(&entry.value, scope);
                }
            }

            Node::RangeExpr { start, end, .. } => {
                self.check_node(start, scope);
                self.check_node(end, scope);
            }

            Node::Spread(inner) => {
                self.check_node(inner, scope);
            }

            Node::Block(stmts) => {
                let mut block_scope = scope.child();
                self.check_block(stmts, &mut block_scope);
            }

            Node::YieldExpr { value } => {
                if self.stream_fn_depth > 0 {
                    self.error_at(
                        Code::OrchestrationType,
                        "`yield` is not a stream emit; use `emit` inside `gen fn`".to_string(),
                        span,
                    );
                }
                if let Some(v) = value {
                    self.check_node(v, scope);
                }
            }

            Node::EmitExpr { value } => {
                self.check_node(value, scope);
                if self.stream_fn_depth == 0 {
                    self.error_at(
                        Code::OrchestrationType,
                        "`emit` can only be used inside a `gen fn`".to_string(),
                        span,
                    );
                } else if let Some(Some(expected)) = self.stream_emit_types.last().cloned() {
                    if let Some(actual) = self.infer_type(value, scope) {
                        if !self.types_compatible(&expected, &actual, scope) {
                            self.type_mismatch_at(
                                Code::ReturnTypeMismatch,
                                "`emit` value",
                                &expected,
                                &actual,
                                span,
                                (
                                    Some((span, "stream emit type expected here".to_string())),
                                    Some(value.span),
                                ),
                                scope,
                            );
                        }
                    }
                }
            }

            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                for entry in fields {
                    self.check_node(&entry.key, scope);
                }
                if let Some(struct_info) = scope.get_struct(struct_name).cloned() {
                    let type_bindings = self.infer_struct_bindings(&struct_info, fields, scope);
                    let type_param_set: std::collections::BTreeSet<String> = struct_info
                        .type_params
                        .iter()
                        .map(|tp| tp.name.clone())
                        .collect();
                    let unbound_type_params: std::collections::BTreeSet<String> = type_param_set
                        .iter()
                        .filter(|name| !type_bindings.contains_key(*name))
                        .cloned()
                        .collect();
                    let mut contextual_fields = vec![false; fields.len()];
                    for (idx, entry) in fields.iter().enumerate() {
                        let expected_type = match &entry.key.node {
                            Node::StringLiteral(key) | Node::Identifier(key) => struct_info
                                .fields
                                .iter()
                                .find(|field| field.name == *key)
                                .and_then(|field| field.type_expr.as_ref())
                                .map(|ty| Self::apply_type_bindings(ty, &type_bindings)),
                            _ => None,
                        };
                        let contextual_expected = expected_type
                            .as_ref()
                            .filter(|ty| !Self::contains_type_param(ty, &unbound_type_params));
                        contextual_fields[idx] =
                            self.check_node_with_expected(&entry.value, contextual_expected, scope);
                    }
                    // Warn on unknown fields
                    for entry in fields {
                        if let Node::StringLiteral(key) | Node::Identifier(key) = &entry.key.node {
                            if !struct_info.fields.iter().any(|field| field.name == *key) {
                                self.warning_at(
                                    Code::UnknownField,
                                    format!("Unknown field '{key}' in struct '{struct_name}'"),
                                    entry.key.span,
                                );
                            }
                        }
                    }
                    // Warn on missing required fields
                    let provided: Vec<String> = fields
                        .iter()
                        .filter_map(|e| match &e.key.node {
                            Node::StringLiteral(k) | Node::Identifier(k) => Some(k.clone()),
                            _ => None,
                        })
                        .collect();
                    for field in &struct_info.fields {
                        if !field.optional && !provided.contains(&field.name) {
                            self.warning_at(
                                Code::FieldTypeMismatch,
                                format!(
                                    "Missing field '{}' in struct '{}' construction",
                                    field.name, struct_name
                                ),
                                span,
                            );
                        }
                    }
                    for field in &struct_info.fields {
                        let Some(expected_type) = &field.type_expr else {
                            continue;
                        };
                        let Some((entry_idx, entry)) =
                            fields.iter().enumerate().find(|(_, entry)| {
                                matches!(&entry.key.node, Node::StringLiteral(key) | Node::Identifier(key) if key == &field.name)
                        }) else {
                            continue;
                        };
                        let Some(actual_type) = self.infer_type(&entry.value, scope) else {
                            continue;
                        };
                        let expected = Self::apply_type_bindings(expected_type, &type_bindings);
                        if !contextual_fields.get(entry_idx).copied().unwrap_or(false)
                            && !self.types_compatible(&expected, &actual_type, scope)
                        {
                            self.type_mismatch_at(
                                Code::FieldTypeMismatch,
                                format!("field `{}` in struct `{struct_name}`", field.name),
                                &expected,
                                &actual_type,
                                entry.value.span,
                                (
                                    Some((span, format!("struct `{struct_name}` expected here"))),
                                    Some(entry.value.span),
                                ),
                                scope,
                            );
                        }
                    }
                } else {
                    for entry in fields {
                        self.check_node(&entry.value, scope);
                    }
                    let suggestion = crate::diagnostic::find_closest_match(
                        struct_name,
                        scope.all_struct_names().iter().map(|name| name.as_str()),
                        2,
                    )
                    .map(|candidate| candidate.to_string());
                    let message = match &suggestion {
                        Some(candidate) => format!(
                            "unknown struct type `{struct_name}` — did you mean `{candidate}`?"
                        ),
                        None => format!("unknown struct type `{struct_name}`"),
                    };
                    match suggestion {
                        Some(candidate) => self.error_at_with_help(
                            Code::UnknownTypeName,
                            message,
                            span,
                            format!("declare `struct {candidate} {{ ... }}` or fix the type name"),
                        ),
                        None => self.error_at_with_help(
                            Code::UnknownTypeName,
                            message,
                            span,
                            format!(
                                "declare `struct {struct_name} {{ ... }}` before constructing it"
                            ),
                        ),
                    }
                }
            }

            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                if let Some(enum_info) = scope.get_enum(enum_name).cloned() {
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
                    let type_param_set: std::collections::BTreeSet<String> = enum_info
                        .type_params
                        .iter()
                        .map(|tp| tp.name.clone())
                        .collect();
                    let mut type_bindings = BTreeMap::new();
                    for (field, arg) in enum_variant.fields.iter().zip(args.iter()) {
                        let Some(expected_type) = &field.type_expr else {
                            continue;
                        };
                        let Some(actual_type) = self.infer_type(arg, scope) else {
                            continue;
                        };
                        if let Err(message) = Self::extract_type_bindings(
                            expected_type,
                            &actual_type,
                            &type_param_set,
                            &mut type_bindings,
                        ) {
                            self.error_at(Code::GenericTypeArgumentMismatch, message, arg.span);
                        }
                    }
                    let unbound_type_params: std::collections::BTreeSet<String> = type_param_set
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
                        contextual_args[idx] =
                            self.check_node_with_expected(arg, contextual_expected, scope);
                    }
                    for (idx, (field, arg)) in
                        enum_variant.fields.iter().zip(args.iter()).enumerate()
                    {
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
                                        format!(
                                            "enum variant `{enum_name}.{variant}` expected here"
                                        ),
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
                } else {
                    for arg in args {
                        self.check_node(arg, scope);
                    }
                }
            }

            Node::InterpolatedString(segments) => {
                self.check_interpolation_segments(segments, scope);
            }

            Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::Identifier(_)
            | Node::DurationLiteral(_)
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::ReturnStmt { value: None }
            | Node::ImportDecl { .. }
            | Node::SelectiveImport { .. } => {}

            // Declarations already handled above; catch remaining variants
            // that have no meaningful type-check behavior.
            Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
                let mut decl_scope = scope.child();
                self.fn_depth += 1;
                self.check_block(body, &mut decl_scope);
                self.fn_depth -= 1;
            }
            Node::AttributedDecl { attributes, inner } => {
                self.check_attributes(attributes, inner);
                self.check_node(inner, scope);
            }

            // Or-patterns are only meaningful as a match-arm pattern.
            // Enforce the literal-only restriction here: an alternative
            // that is not a literal pattern (string, int, float, bool,
            // nil, or the wildcard `_`) would silently degrade
            // exhaustiveness to "assume wildcard" and make VM lowering
            // surface its own errors. Rejecting early keeps diagnostics
            // local to the offending alternative.
            Node::OrPattern(alternatives) => {
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
                    self.check_node(alt, scope);
                }
            }
        }
    }

    /// Type-check the expression holes inside an interpolated string
    /// (`"... ${expr} ..."`). Each `${...}` hole is captured by the lexer
    /// as raw source text, so we re-lex and re-parse it at its original
    /// position and run it through the normal expression walk — name
    /// resolution, argument/return type checking, nil-flow, etc. all apply
    /// exactly as they would outside the string. A hole that fails to
    /// lex/parse is left to the bytecode compiler, which raises a precise
    /// `invalid interpolation` error at compile time; surfacing a second,
    /// lower-quality parse diagnostic here would only add noise.
    fn check_interpolation_segments(
        &mut self,
        segments: &[harn_lexer::StringSegment],
        scope: &mut TypeScope,
    ) {
        for seg in segments {
            let harn_lexer::StringSegment::Expression(src, line, col) = seg else {
                continue;
            };
            let mut lexer = harn_lexer::Lexer::with_position(src, *line, *col);
            let Ok(tokens) = lexer.tokenize() else {
                continue;
            };
            let mut parser = crate::Parser::new(tokens);
            let Ok(expr) = parser.parse_single_expression() else {
                continue;
            };
            self.check_node(&expr, scope);
        }
    }

    /// After a plain `target = value` assignment, narrow the target to non-nil
    /// when `value` is statically non-nil and the target's declared type is
    /// nilable. This is control-flow narrowing for assignment (Rust/TS/Flow do
    /// the same): once `x` has been assigned a concrete value, reads of `x`
    /// should not be treated as possibly-nil until the next reassignment.
    /// Variables narrow via `narrowed_vars`; reference paths (`obj.field`) via
    /// a `Remove("nil")` path narrowing — the same machinery a `!= nil` guard
    /// uses, so reads pick it up automatically.
    fn narrow_after_assignment(&self, target: &SNode, value: &SNode, scope: &mut TypeScope) {
        let Some(value_ty) = self.infer_type(value, scope) else {
            return;
        };
        // A value that might itself be nil can't narrow anything.
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
                    // Remember the declared type so the next reassignment (or a
                    // scope merge) can restore it, exactly like guard narrowing.
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
