//! Function-body and return-statement checking.
//!
//! `check_fn_body` is the standard entry point for any callable body
//! (fn / tool / pipeline / closure) — it bumps `fn_depth` so `try*`
//! diagnostics know they have somewhere to rethrow to. `check_return_type`
//! recursively walks return statements and `if`/`else` arms to verify
//! every reachable return matches the declared return type.

use crate::ast::*;

use super::super::format::format_type;
use super::super::scope::TypeScope;
use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn check_fn_body(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
    ) {
        self.fn_depth += 1;
        self.check_fn_body_inner(type_params, params, return_type, body, where_clauses);
        self.fn_depth -= 1;
    }

    fn check_fn_body_inner(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
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
