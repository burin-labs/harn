//! Function-body and return-statement checking.
//!
//! `check_fn_body` is the standard entry point for any callable body
//! (fn / tool / pipeline / closure) — it bumps `fn_depth` so `try*`
//! diagnostics know they have somewhere to rethrow to. `check_return_type`
//! recursively walks return statements and `if`/`else` arms to verify
//! every reachable return matches the declared return type.

use crate::ast::*;
use crate::diagnostic_codes::Code;
use harn_lexer::Span;

use super::super::format::format_type;
use super::super::scope::TypeScope;
use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn callable_return_type(
        is_stream: bool,
        return_type: &Option<TypeExpr>,
        body: &[SNode],
    ) -> Option<TypeExpr> {
        if is_stream {
            return Some(
                return_type
                    .clone()
                    .unwrap_or_else(|| TypeExpr::Stream(Box::new(TypeExpr::Named("any".into())))),
            );
        }
        if Self::body_contains_yield(body) {
            return Some(
                return_type.clone().unwrap_or_else(|| {
                    TypeExpr::Generator(Box::new(TypeExpr::Named("any".into())))
                }),
            );
        }
        return_type.clone()
    }

    pub(in crate::typechecker) fn body_contains_yield(nodes: &[SNode]) -> bool {
        nodes
            .iter()
            .any(|node| Self::node_contains_yield(&node.node))
    }

    fn node_contains_yield(node: &Node) -> bool {
        match node {
            Node::YieldExpr { .. } => true,
            Node::FnDecl { .. } | Node::Closure { .. } => false,
            Node::Block(body)
            | Node::SpawnExpr { body }
            | Node::Retry { body, .. }
            | Node::CostRoute { body, .. }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body, .. }
            | Node::Parallel { body, .. }
            | Node::TryExpr { body } => Self::body_contains_yield(body),
            Node::IfElse {
                then_body,
                else_body,
                ..
            } => {
                Self::body_contains_yield(then_body)
                    || else_body
                        .as_ref()
                        .is_some_and(|body| Self::body_contains_yield(body))
            }
            Node::ForIn { body, .. } | Node::WhileLoop { body, .. } => {
                Self::body_contains_yield(body)
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                Self::body_contains_yield(body)
                    || Self::body_contains_yield(catch_body)
                    || finally_body
                        .as_ref()
                        .is_some_and(|body| Self::body_contains_yield(body))
            }
            Node::MatchExpr { arms, .. } => {
                arms.iter().any(|arm| Self::body_contains_yield(&arm.body))
            }
            _ => false,
        }
    }

    pub(in crate::typechecker) fn check_fn_body(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
        is_stream: bool,
        expected_span: Span,
    ) {
        self.fn_depth += 1;
        let saved_stream_depth = self.stream_fn_depth;
        let saved_stream_emit_types = self.stream_emit_types.clone();
        if is_stream {
            self.stream_fn_depth += 1;
            self.stream_emit_types
                .push(Self::stream_emit_type(return_type));
        } else {
            self.stream_fn_depth = 0;
            self.stream_emit_types.clear();
        }
        self.check_fn_body_inner(
            type_params,
            params,
            return_type,
            body,
            where_clauses,
            is_stream,
            expected_span,
        );
        if is_stream {
            self.stream_emit_types.pop();
        }
        self.stream_fn_depth = saved_stream_depth;
        self.stream_emit_types = saved_stream_emit_types;
        self.fn_depth -= 1;
    }

    fn stream_emit_type(return_type: &Option<TypeExpr>) -> Option<TypeExpr> {
        match return_type {
            Some(TypeExpr::Stream(inner)) => Some((**inner).clone()),
            _ => None,
        }
    }

    pub(in crate::typechecker) fn check_value_returning_body(
        &mut self,
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        expected_span: Span,
        result_label: &str,
        declaration_label: &str,
    ) {
        let mut body_scope = TypeScope::child_of(&self.scope);
        self.fn_depth += 1;
        for param in params {
            let param_type = if param.rest {
                param
                    .type_expr
                    .clone()
                    .map(|inner| TypeExpr::List(Box::new(inner)))
            } else {
                param.type_expr.clone()
            };
            let has_annotation = param.type_expr.is_some();
            body_scope.define_var(&param.name, param_type);
            if has_annotation {
                body_scope.mark_annotated(&param.name);
            }
            body_scope.clear_nil_widenable(&param.name);
            if let Some(default) = &param.default_value {
                self.check_node(default, &mut body_scope);
            }
        }
        self.check_block(body, &mut body_scope);
        self.fn_depth -= 1;

        if let Some(ret_type) = return_type {
            let mut ret_scope = body_scope.clone();
            ret_scope.restore_narrowed_vars();
            for stmt in body {
                self.check_return_type(stmt, ret_type, expected_span, &mut ret_scope);
            }
            if !self.body_cannot_fall_through(body, &ret_scope) {
                let actual = self
                    .infer_block_type(body, &ret_scope)
                    .unwrap_or_else(|| TypeExpr::Named("nil".into()));
                if !self.types_compatible(ret_type, &actual, &ret_scope) {
                    let value_span = body.last().map(|stmt| stmt.span).unwrap_or(expected_span);
                    self.type_mismatch_at(
                        Code::ReturnTypeMismatch,
                        result_label,
                        ret_type,
                        &actual,
                        value_span,
                        (
                            Some((expected_span, declaration_label.to_string())),
                            Some(value_span),
                        ),
                        &ret_scope,
                    );
                }
            }
        }
    }

    fn check_fn_body_inner(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
        is_stream: bool,
        expected_span: Span,
    ) {
        let mut fn_scope = TypeScope::child_of(&self.scope);
        // Register generic type parameters so they are treated as compatible
        // with any concrete type during type checking.
        for tp in type_params {
            fn_scope.generic_type_params.insert(tp.name.clone());
        }
        // Store where-clause constraints for definition-site checking
        for wc in where_clauses {
            fn_scope
                .where_constraints
                .insert(wc.type_name.clone(), wc.bound.clone());
        }
        for param in params {
            let param_type = if param.rest {
                param
                    .type_expr
                    .clone()
                    .map(|inner| TypeExpr::List(Box::new(inner)))
            } else {
                param.type_expr.clone()
            };
            let has_annotation = param.type_expr.is_some();
            fn_scope.define_var(&param.name, param_type);
            if has_annotation {
                fn_scope.mark_annotated(&param.name);
            }
            fn_scope.clear_nil_widenable(&param.name);
            if let Some(default) = &param.default_value {
                self.check_node(default, &mut fn_scope);
            }
        }
        self.check_block(body, &mut fn_scope);

        if is_stream && !matches!(return_type, None | Some(TypeExpr::Stream(_))) {
            if let Some(actual) = return_type {
                self.error_at(
                    Code::ReturnTypeMismatch,
                    format!(
                        "`gen fn` must return Stream<T>, found {}",
                        format_type(actual)
                    ),
                    Span::dummy(),
                );
            }
        }

        // Check return statements against the declared return type using the
        // post-body scope so locally-bound `let` values are visible, with any
        // outstanding narrowings rolled back so a parameter typed (e.g.) `T?`
        // is still seen as `T?` here even when the body narrowed it inside a
        // conditional that fell through.
        if let Some(ret_type) = return_type {
            let mut ret_scope = fn_scope.clone();
            ret_scope.restore_narrowed_vars();
            for stmt in body {
                self.check_return_type(stmt, ret_type, expected_span, &mut ret_scope);
            }
            if !is_stream
                && !Self::body_contains_yield(body)
                && !self.body_cannot_fall_through(body, &ret_scope)
                && !self.return_type_allows_implicit_nil(ret_type, &ret_scope)
            {
                self.error_at(
                    Code::ReturnTypeMismatch,
                    format!(
                        "function can fall through without returning {}",
                        format_type(ret_type)
                    ),
                    expected_span,
                );
            }
        }
    }

    fn return_type_allows_implicit_nil(&self, expected: &TypeExpr, scope: &TypeScope) -> bool {
        self.types_compatible(expected, &TypeExpr::Named("nil".into()), scope)
    }

    fn body_cannot_fall_through(&self, body: &[SNode], scope: &TypeScope) -> bool {
        body.iter()
            .any(|stmt| self.stmt_cannot_fall_through(stmt, scope))
    }

    fn stmt_cannot_fall_through(&self, stmt: &SNode, scope: &TypeScope) -> bool {
        if Self::block_definitely_exits(std::slice::from_ref(stmt)) {
            return true;
        }
        match &stmt.node {
            Node::MatchExpr { value, arms } => {
                self.match_is_exhaustive(value, arms, scope)
                    && arms.iter().all(|arm| {
                        let mut arm_scope = scope.child();
                        let value_type = self.infer_type(value, scope);
                        self.define_match_pattern_bindings(
                            &arm.pattern,
                            value_type.as_ref(),
                            &mut arm_scope,
                        );
                        self.body_cannot_fall_through(&arm.body, &arm_scope)
                    })
            }
            Node::Block(body)
            | Node::TryExpr { body }
            | Node::CostRoute { body, .. }
            | Node::MutexBlock { body, .. }
            | Node::DeadlineBlock { body, .. }
            | Node::Retry { body, .. } => self.body_cannot_fall_through(body, scope),
            Node::TryCatch {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                finally_body
                    .as_ref()
                    .is_some_and(|body| self.body_cannot_fall_through(body, scope))
                    || (self.body_cannot_fall_through(body, scope)
                        && self.body_cannot_fall_through(catch_body, scope))
            }
            _ => matches!(self.infer_type(stmt, scope), Some(TypeExpr::Never)),
        }
    }

    pub(in crate::typechecker) fn check_return_type(
        &mut self,
        snode: &SNode,
        expected: &TypeExpr,
        expected_span: Span,
        scope: &mut TypeScope,
    ) {
        match &snode.node {
            Node::ReturnStmt { value: Some(val) } => {
                let inferred = self.infer_type(val, scope);
                if let Some(actual) = &inferred {
                    if !self.types_compatible(expected, actual, scope) {
                        self.type_mismatch_at(
                            Code::ReturnTypeMismatch,
                            "return value",
                            expected,
                            actual,
                            val.span,
                            (
                                Some((expected_span, "return type declared here".to_string())),
                                Some(val.span),
                            ),
                            scope,
                        );
                    }
                }
                // Returning an `owned<T>` binding by name silently disables
                // the auto-drop: the value escapes the scope where the
                // synthetic `defer { drop(x) }` would have fired. Surface
                // this as `HARN-OWN-003` so the author can either change the
                // return signature to `owned<T>` (declaring an ownership
                // transfer) or pick a different value.
                if let Node::Identifier(name) = &val.node {
                    if let Some(Some(declared)) = scope.get_var(name) {
                        if matches!(declared, TypeExpr::Owned(_))
                            && !matches!(expected, TypeExpr::Owned(_))
                        {
                            self.warning_at(
                                Code::OwnershipEscape,
                                format!(
                                    "owned binding `{name}` escapes its scope via `return`; \
                                     either return `owned<…>` to transfer ownership or drop \
                                     the value before returning"
                                ),
                                val.span,
                            );
                        }
                    }
                }
            }
            Node::ReturnStmt { value: None } => {
                let actual = TypeExpr::Named("nil".into());
                if !self.types_compatible(expected, &actual, scope) {
                    self.type_mismatch_at(
                        Code::ReturnTypeMismatch,
                        "return value",
                        expected,
                        &actual,
                        snode.span,
                        (
                            Some((expected_span, "return type declared here".to_string())),
                            Some(snode.span),
                        ),
                        scope,
                    );
                }
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let refs = Self::extract_refinements(condition, scope);
                let mut then_scope = scope.child();
                refs.apply_truthy(&mut then_scope);
                for stmt in then_body {
                    self.check_return_type(stmt, expected, expected_span, &mut then_scope);
                }
                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    refs.apply_falsy(&mut else_scope);
                    for stmt in else_body {
                        self.check_return_type(stmt, expected, expected_span, &mut else_scope);
                    }
                    // Post-branch narrowing for return type checking
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
            Node::MatchExpr { value, arms } => {
                let value_type = self.infer_type(value, scope);
                for arm in arms {
                    let mut arm_scope = scope.child();
                    self.define_match_pattern_bindings(
                        &arm.pattern,
                        value_type.as_ref(),
                        &mut arm_scope,
                    );
                    for stmt in &arm.body {
                        self.check_return_type(stmt, expected, expected_span, &mut arm_scope);
                    }
                }
            }
            Node::Block(body)
            | Node::TryExpr { body }
            | Node::CostRoute { body, .. }
            | Node::MutexBlock { body, .. }
            | Node::DeadlineBlock { body, .. }
            | Node::Retry { body, .. } => {
                let mut block_scope = scope.child();
                for stmt in body {
                    self.check_return_type(stmt, expected, expected_span, &mut block_scope);
                }
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
                ..
            } => {
                let mut try_scope = scope.child();
                for stmt in body {
                    self.check_return_type(stmt, expected, expected_span, &mut try_scope);
                }

                let mut catch_scope = scope.child();
                if let Some(var) = error_var {
                    catch_scope.define_var(var, error_type.clone());
                    catch_scope.clear_nil_widenable(var);
                }
                for stmt in catch_body {
                    self.check_return_type(stmt, expected, expected_span, &mut catch_scope);
                }

                if let Some(finally_body) = finally_body {
                    let mut finally_scope = scope.child();
                    for stmt in finally_body {
                        self.check_return_type(stmt, expected, expected_span, &mut finally_scope);
                    }
                }
            }
            Node::WhileLoop { condition, body } => {
                let refs = Self::extract_refinements(condition, scope);
                let mut loop_scope = scope.child();
                refs.apply_truthy(&mut loop_scope);
                for stmt in body {
                    self.check_return_type(stmt, expected, expected_span, &mut loop_scope);
                }
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let mut loop_scope = scope.child();
                if let crate::ast::BindingPattern::Identifier(variable) = pattern {
                    let elem_type = self
                        .infer_type(iterable, scope)
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope));
                    loop_scope.define_var(variable, elem_type);
                    loop_scope.clear_nil_widenable(variable);
                }
                for stmt in body {
                    self.check_return_type(stmt, expected, expected_span, &mut loop_scope);
                }
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let refs = Self::extract_refinements(condition, scope);
                let mut else_scope = scope.child();
                refs.apply_falsy(&mut else_scope);
                for stmt in else_body {
                    self.check_return_type(stmt, expected, expected_span, &mut else_scope);
                }
            }
            _ => {}
        }
    }
}
