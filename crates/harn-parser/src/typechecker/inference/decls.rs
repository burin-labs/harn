//! Function-body and return-statement checking.
//!
//! `check_fn_body` is the standard entry point for any callable body
//! (fn / tool / pipeline / closure) — it bumps `fn_depth` so `try*`
//! diagnostics know they have somewhere to rethrow to. `check_return_type`
//! recursively walks return statements and `if`/`else` arms to verify
//! every reachable return matches the declared return type.

use crate::ast::*;
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
            | Node::DeferStmt { body }
            | Node::MutexBlock { body }
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

    fn check_fn_body_inner(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
        is_stream: bool,
    ) {
        let mut fn_scope = self.scope.child();
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
            fn_scope.define_var(&param.name, param.type_expr.clone());
            fn_scope.clear_nil_widenable(&param.name);
            if let Some(default) = &param.default_value {
                self.check_node(default, &mut fn_scope);
            }
        }
        // Snapshot scope before main pass (which may mutate it with narrowing)
        // so that return-type checking starts from the original parameter types.
        let ret_scope_base = if return_type.is_some() {
            Some(fn_scope.child())
        } else {
            None
        };

        self.check_block(body, &mut fn_scope);

        if is_stream && !matches!(return_type, None | Some(TypeExpr::Stream(_))) {
            if let Some(actual) = return_type {
                self.error_at(
                    format!(
                        "`gen fn` must return Stream<T>, got {}",
                        format_type(actual)
                    ),
                    Span::dummy(),
                );
            }
        }

        // Check return statements against declared return type
        if let Some(ret_type) = return_type {
            let mut ret_scope = ret_scope_base.unwrap();
            for stmt in body {
                self.check_return_type(stmt, ret_type, &mut ret_scope);
            }
        }
    }

    pub(in crate::typechecker) fn check_return_type(
        &mut self,
        snode: &SNode,
        expected: &TypeExpr,
        scope: &mut TypeScope,
    ) {
        let span = snode.span;
        match &snode.node {
            Node::ReturnStmt { value: Some(val) } => {
                let inferred = self.infer_type(val, scope);
                if let Some(actual) = &inferred {
                    if !self.types_compatible(expected, actual, scope) {
                        self.error_at(
                            format!(
                                "return type doesn't match: expected {}, got {}",
                                format_type(expected),
                                format_type(actual)
                            ),
                            span,
                        );
                    }
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
                    self.check_return_type(stmt, expected, &mut then_scope);
                }
                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    refs.apply_falsy(&mut else_scope);
                    for stmt in else_body {
                        self.check_return_type(stmt, expected, &mut else_scope);
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
            _ => {}
        }
    }
}
