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
use std::collections::BTreeSet;

use super::super::format::format_type;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{FnSignature, InferredType, TypeScope};
use super::super::union::{intersect_types, narrow_to_single, simplify_union};
use super::super::TypeChecker;

#[derive(Clone, Copy)]
pub(in crate::typechecker) struct CallableDeclarationContext<'a> {
    pub span: Span,
    pub scope: &'a TypeScope,
}

#[derive(Clone, Copy)]
pub(in crate::typechecker) struct CallableBodyContract<'a> {
    pub type_params: &'a [TypeParam],
    pub params: &'a [TypedParam],
    pub return_type: &'a Option<TypeExpr>,
    pub type_predicate: Option<&'a TypePredicate>,
    pub where_clauses: &'a [WhereClause],
    pub is_stream: bool,
}

impl TypeChecker {
    /// Check a parameter default before introducing the parameter itself.
    /// Earlier parameters are already in `scope`; the current parameter and
    /// every later one still resolve in the enclosing scope, matching runtime
    /// default evaluation for functions, tools, and closure literals.
    pub(super) fn check_and_define_parameter(
        &mut self,
        param: &TypedParam,
        param_type: InferredType,
        annotated: bool,
        scope: &mut TypeScope,
    ) {
        if let Some(default) = &param.default_value {
            let context_checked =
                self.check_node_with_expected(default, param_type.as_ref(), scope);
            if !context_checked {
                match (param_type.as_ref(), self.infer_type(default, scope)) {
                    (Some(expected), Some(actual))
                        if !self.types_compatible(expected, &actual, scope) =>
                    {
                        self.type_mismatch_at(
                            Code::VariableTypeMismatch,
                            format!("parameter default `{}`", param.name),
                            expected,
                            &actual,
                            default.span,
                            (None, Some(default.span)),
                            scope,
                        );
                    }
                    _ => {}
                }
            }
        }
        scope.define_var(&param.name, param_type);
        if annotated {
            scope.mark_annotated(&param.name);
        }
        scope.clear_nil_widenable(&param.name);
    }

    pub(super) fn check_and_define_declared_parameters(
        &mut self,
        params: &[TypedParam],
        scope: &mut TypeScope,
    ) {
        for param in params {
            let annotated = param
                .type_expr
                .as_ref()
                .is_some_and(|ty| !Self::contains_wildcard_type(ty));
            self.check_and_define_parameter(param, param.type_expr.clone(), annotated, scope);
        }
    }

    pub(in crate::typechecker) fn fn_signature_from_parts(
        params: &[TypedParam],
        return_type: InferredType,
        type_predicate: Option<TypePredicate>,
        definition_span: Option<Span>,
        type_params: &[TypeParam],
        where_clauses: &[WhereClause],
    ) -> FnSignature {
        FnSignature {
            params: params
                .iter()
                .map(|param| (param.name.clone(), param.type_expr.clone()))
                .collect(),
            return_type,
            definition_span,
            type_param_names: type_params
                .iter()
                .map(|type_param| type_param.name.clone())
                .collect(),
            required_params: params
                .iter()
                .position(|param| param.default_value.is_some())
                .unwrap_or_else(|| params.iter().filter(|param| !param.rest).count()),
            where_clauses: where_clauses
                .iter()
                .map(|where_clause| (where_clause.type_name.clone(), where_clause.bound.clone()))
                .collect(),
            has_rest: params.last().is_some_and(|param| param.rest),
            type_predicate,
        }
    }

    pub(in crate::typechecker) fn fn_signature_from_decl<F>(
        inner: &SNode,
        definition_span: Option<Span>,
        infer_return: F,
    ) -> Option<FnSignature>
    where
        F: FnOnce(&[TypedParam], &[SNode]) -> InferredType,
    {
        let Node::FnDecl {
            type_params,
            params,
            return_type,
            type_predicate,
            where_clauses,
            body,
            is_stream,
            ..
        } = &inner.node
        else {
            return None;
        };
        let return_type = Self::callable_return_type(*is_stream, return_type, body)
            .or_else(|| infer_return(params, body));
        Some(Self::fn_signature_from_parts(
            params,
            return_type,
            type_predicate.clone(),
            definition_span,
            type_params,
            where_clauses,
        ))
    }

    pub(in crate::typechecker) fn nongeneric_signature_from_params(
        params: &[TypedParam],
        return_type: InferredType,
        definition_span: Option<Span>,
    ) -> FnSignature {
        Self::fn_signature_from_parts(params, return_type, None, definition_span, &[], &[])
    }

    pub(in crate::typechecker) fn empty_callable_signature(
        definition_span: Option<Span>,
    ) -> FnSignature {
        FnSignature {
            params: Vec::new(),
            return_type: None,
            definition_span,
            type_param_names: Vec::new(),
            required_params: 0,
            where_clauses: Vec::new(),
            has_rest: false,
            type_predicate: None,
        }
    }

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

    /// Infer the return type of a plain (non-stream, non-yield) function whose
    /// return type is unannotated, from its body — so calling an un-annotated
    /// helper recovers a precise type instead of going untyped.
    ///
    /// Sound by construction: every `return` path *and* the value the body
    /// falls through to must be concretely known, otherwise the function stays
    /// untyped (`None`). The inferred type is therefore always a faithful upper
    /// bound of the actual returns, so it can only ever surface real call-site
    /// type errors, never false positives. Recursion is self-guarding — a
    /// self/mutual/forward call resolves to the hoisted placeholder signature
    /// (return `None`), which trips the bail rule and leaves the function
    /// untyped exactly as a checker without return inference would.
    pub(in crate::typechecker) fn infer_unannotated_fn_return(
        &self,
        params: &[TypedParam],
        body: &[SNode],
        enclosing_scope: &TypeScope,
    ) -> InferredType {
        let mut scope = enclosing_scope.child();
        for param in params {
            let param_type = if param.rest {
                param
                    .type_expr
                    .clone()
                    .map(|inner| TypeExpr::List(Box::new(inner)))
            } else {
                param.type_expr.clone()
            };
            scope.define_var(&param.name, param_type);
        }
        let mut returns: Vec<TypeExpr> = Vec::new();
        if !self.collect_block_returns(body, &mut scope, &mut returns) {
            return None;
        }
        // A body that can fall through implicitly returns its trailing value.
        if !self.body_cannot_fall_through(body, &scope) {
            match self.infer_block_type(body, &scope).into_inferred() {
                Some(ty) => returns.push(ty),
                None => return None,
            }
        }
        (!returns.is_empty()).then(|| simplify_union(returns))
    }

    fn collect_block_returns(
        &self,
        body: &[SNode],
        scope: &mut TypeScope,
        out: &mut Vec<TypeExpr>,
    ) -> bool {
        for stmt in body {
            // Thread local `let`/`const` bindings into the scope *before*
            // inferring later returns, mirroring `check_block`'s scoping. Without
            // this, a `return localVar` resolves the name against the outer scope
            // (e.g. to a function of the same name), mis-typing the return.
            self.define_local_binding(stmt, scope);
            if !self.collect_return_types(stmt, scope, out) {
                return false;
            }
        }
        true
    }

    /// Define the names a top-level body statement introduces, so subsequent
    /// `return` expressions resolve locals correctly. Immutable `const`
    /// bindings carry their inferred type; mutable `let` and destructured
    /// bindings are shadowed as unknown (`None`) — a `let` can be reassigned to
    /// another type, so trusting its initializer would be unsound. Anything
    /// that depends on an unknown local then makes the function stay dynamic
    /// via the bail rule.
    fn define_local_binding(&self, stmt: &SNode, scope: &mut TypeScope) {
        match &stmt.node {
            Node::LetBinding { pattern, .. } => match pattern {
                BindingPattern::Identifier(name) => scope.define_var(name, None),
                other => Self::shadow_pattern_names(other, scope),
            },
            Node::ConstBinding {
                pattern,
                type_ann,
                value,
                ..
            } => match pattern {
                BindingPattern::Identifier(name) => {
                    let ty = type_ann.clone().or_else(|| self.infer_type(value, scope));
                    scope.define_var(name, ty);
                    scope.define_flow_alias(name, value.as_ref().clone());
                }
                other => Self::shadow_pattern_names(other, scope),
            },
            _ => {}
        }
    }

    fn shadow_pattern_names(pattern: &BindingPattern, scope: &mut TypeScope) {
        match pattern {
            BindingPattern::Identifier(name) => scope.define_var(name, None),
            BindingPattern::Dict(fields) => {
                for field in fields {
                    scope.define_var(field.alias.as_deref().unwrap_or(&field.key), None);
                }
            }
            BindingPattern::List(elements) => {
                for element in elements {
                    scope.define_var(&element.name, None);
                }
            }
            BindingPattern::Pair(a, b) => {
                scope.define_var(a, None);
                scope.define_var(b, None);
            }
        }
    }

    /// Collect the value types of every `return` in `snode` that exits the
    /// *enclosing* function. Mirrors `check_return_type`'s node coverage
    /// exactly (closures and nested `fn`s are NOT traversed — their returns
    /// belong to themselves). Returns `false` if any return value is not
    /// concretely inferable, signalling the caller to stay untyped.
    fn collect_return_types(
        &self,
        snode: &SNode,
        scope: &mut TypeScope,
        out: &mut Vec<TypeExpr>,
    ) -> bool {
        match &snode.node {
            Node::ReturnStmt { value: Some(val) } => match self.infer_type(val, scope) {
                Some(ty) => {
                    out.push(ty);
                    true
                }
                None => false,
            },
            Node::ReturnStmt { value: None } => {
                out.push(TypeExpr::Named("nil".into()));
                true
            }
            Node::IfElse {
                then_body,
                else_body,
                ..
            } => {
                let mut then_scope = scope.child();
                if !self.collect_block_returns(then_body, &mut then_scope, out) {
                    return false;
                }
                match else_body {
                    Some(eb) => {
                        let mut else_scope = scope.child();
                        self.collect_block_returns(eb, &mut else_scope, out)
                    }
                    None => true,
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
                    self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
                    if !self.collect_block_returns(&arm.body, &mut arm_scope, out) {
                        return false;
                    }
                }
                true
            }
            Node::Block(body)
            | Node::TryExpr { body }
            | Node::CostRoute { body, .. }
            | Node::MutexBlock { body, .. }
            | Node::DeadlineBlock { body, .. }
            | Node::Retry { body, .. }
            | Node::WhileLoop { body, .. } => {
                let mut block_scope = scope.child();
                self.collect_block_returns(body, &mut block_scope, out)
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
                }
                self.collect_block_returns(body, &mut loop_scope, out)
            }
            Node::GuardStmt { else_body, .. } => {
                let mut else_scope = scope.child();
                self.collect_block_returns(else_body, &mut else_scope, out)
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
                if !self.collect_block_returns(body, &mut try_scope, out) {
                    return false;
                }
                let mut catch_scope = scope.child();
                if let Some(var) = error_var {
                    catch_scope.define_var(var, error_type.clone());
                }
                if !self.collect_block_returns(catch_body, &mut catch_scope, out) {
                    return false;
                }
                match finally_body {
                    Some(fb) => {
                        let mut finally_scope = scope.child();
                        self.collect_block_returns(fb, &mut finally_scope, out)
                    }
                    None => true,
                }
            }
            // Leaf statements and nested callables (Closure / FnDecl) contain no
            // `return` that exits this function.
            _ => true,
        }
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
        contract: CallableBodyContract<'_>,
        body: &[SNode],
        declaration: CallableDeclarationContext<'_>,
    ) {
        self.fn_depth += 1;
        let saved_stream_depth = self.stream_fn_depth;
        let saved_stream_emit_types = self.stream_emit_types.clone();
        if contract.is_stream {
            self.stream_fn_depth += 1;
            self.stream_emit_types
                .push(Self::stream_emit_type(contract.return_type));
        } else {
            self.stream_fn_depth = 0;
            self.stream_emit_types.clear();
        }
        self.check_fn_body_inner(contract, body, declaration);
        if contract.is_stream {
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
        enclosing_scope: &TypeScope,
    ) {
        let mut body_scope = enclosing_scope.child();
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
            self.check_and_define_parameter(
                param,
                param_type,
                param.type_expr.is_some(),
                &mut body_scope,
            );
        }
        Self::mark_closure_mutated_captures(&mut body_scope, body);
        self.expected_return_types.push(return_type.clone());
        self.check_block_with_expected_tail(body, return_type.as_ref(), &mut body_scope);
        self.expected_return_types.pop();
        self.fn_depth -= 1;

        if let Some(ret_type) = return_type {
            let mut ret_scope = body_scope.clone();
            ret_scope.restore_narrowed_vars();
            for stmt in body {
                self.check_return_type(stmt, ret_type, expected_span, &mut ret_scope);
            }
            if !self.body_cannot_fall_through(body, &ret_scope) {
                let actual = self.infer_block_type(body, &ret_scope);
                let Some(actual) = actual.into_inferred() else {
                    return;
                };
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
        contract: CallableBodyContract<'_>,
        body: &[SNode],
        declaration: CallableDeclarationContext<'_>,
    ) {
        let mut fn_scope = declaration.scope.child();
        // Register generic type parameters so they are treated as compatible
        // with any concrete type during type checking.
        for tp in contract.type_params {
            fn_scope.generic_type_params.insert(tp.name.clone());
        }
        // Store where-clause constraints for definition-site checking. A type
        // parameter can appear in more than one clause (`where T: A, T: B`) or
        // carry additive bounds (`where T: A + B`); accumulate them all rather
        // than letting a later clause clobber an earlier one.
        for wc in contract.where_clauses {
            let bounds = fn_scope
                .where_constraints
                .entry(wc.type_name.clone())
                .or_default();
            if !bounds.contains(&wc.bound) {
                bounds.push(wc.bound.clone());
            }
        }
        for param in contract.params {
            let param_type = if param.rest {
                param
                    .type_expr
                    .clone()
                    .map(|inner| TypeExpr::List(Box::new(inner)))
            } else {
                param.type_expr.clone()
            };
            self.check_and_define_parameter(
                param,
                param_type,
                param.type_expr.is_some(),
                &mut fn_scope,
            );
        }
        Self::mark_closure_mutated_captures(&mut fn_scope, body);
        self.expected_return_types
            .push(contract.return_type.clone());
        self.check_block_with_expected_tail(body, contract.return_type.as_ref(), &mut fn_scope);
        self.expected_return_types.pop();

        if let Some(predicate) = contract.type_predicate {
            if self.validate_type_predicate(
                predicate,
                contract.type_params,
                contract.params,
                body,
                &fn_scope,
                contract.is_stream,
            ) {
                self.validated_type_predicates
                    .insert((declaration.span.start, declaration.span.end));
            }
        }

        if contract.is_stream && !matches!(contract.return_type, None | Some(TypeExpr::Stream(_))) {
            if let Some(actual) = contract.return_type {
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
        if let Some(ret_type) = contract.return_type {
            let mut ret_scope = fn_scope.clone();
            ret_scope.restore_narrowed_vars();
            for stmt in body {
                self.check_return_type(stmt, ret_type, declaration.span, &mut ret_scope);
            }
            if !contract.is_stream
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
                    declaration.span,
                );
            }
        }
    }

    fn validate_type_predicate(
        &mut self,
        predicate: &TypePredicate,
        type_params: &[TypeParam],
        params: &[TypedParam],
        body: &[SNode],
        scope: &TypeScope,
        is_stream: bool,
    ) -> bool {
        let Some(parameter) = params
            .iter()
            .find(|parameter| parameter.name == predicate.parameter)
        else {
            self.error_at(
                Code::InvalidTypePredicate,
                format!(
                    "type predicate names `{}`, but the function has no parameter with that name",
                    predicate.parameter
                ),
                predicate.span,
            );
            return false;
        };
        if is_stream || parameter.rest {
            self.error_at(
                Code::InvalidTypePredicate,
                "a type predicate must name a regular parameter on a non-stream function"
                    .to_string(),
                predicate.span,
            );
            return false;
        }
        let Some(parameter_type) = &parameter.type_expr else {
            self.error_at(
                Code::InvalidTypePredicate,
                format!(
                    "type predicate parameter `{}` needs an explicit input type",
                    predicate.parameter
                ),
                predicate.span,
            );
            return false;
        };
        let generic_names = type_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<BTreeSet<_>>();
        if Self::contains_type_param(&predicate.type_expr, &generic_names) {
            self.error_at(
                Code::InvalidTypePredicate,
                "a type predicate cannot target a generic type parameter because Harn cannot prove every call substitution".to_string(),
                predicate.span,
            );
            return false;
        }
        if !self.types_compatible(parameter_type, &predicate.type_expr, scope) {
            self.error_at(
                Code::InvalidTypePredicate,
                format!(
                    "predicate type `{}` is not a subtype of parameter `{}` type `{}`",
                    format_type(&predicate.type_expr),
                    predicate.parameter,
                    format_type(parameter_type)
                ),
                predicate.span,
            );
            return false;
        }
        let Some((last, prefix)) = body.split_last() else {
            self.invalid_predicate_body(predicate, None);
            return false;
        };
        if !prefix.iter().all(|statement| {
            matches!(
                statement.node,
                Node::ConstBinding {
                    pattern: BindingPattern::Identifier(_),
                    ..
                }
            )
        }) {
            self.invalid_predicate_body(predicate, Some(last.span));
            return false;
        }
        let Node::ReturnStmt { value: Some(value) } = &last.node else {
            self.invalid_predicate_body(predicate, Some(last.span));
            return false;
        };

        if self.predicate_uses_unvalidated_local_contract(value, scope) {
            self.error_at(
                Code::InvalidTypePredicate,
                "the return condition uses a local type predicate that has not been validated yet"
                    .to_string(),
                value.span,
            );
            return false;
        }

        let refinements = self.extract_refinements(value, scope);
        let truthy_type = refinements
            .truthy
            .iter()
            .find(|(name, _)| name == &predicate.parameter)
            .and_then(|(_, ty)| ty.as_ref());
        let positive_is_proved = truthy_type
            .is_some_and(|actual| self.types_compatible(&predicate.type_expr, actual, scope));
        if !positive_is_proved {
            self.error_at(
                Code::InvalidTypePredicate,
                format!(
                    "the return condition does not prove `{}` is `{}` when it is true",
                    predicate.parameter,
                    format_type(&predicate.type_expr)
                ),
                value.span,
            );
            return false;
        }
        if predicate.one_sided {
            return true;
        }

        let target = self.resolve_alias(&predicate.type_expr, scope);
        let falsy_excludes_target = refinements
            .falsy
            .iter()
            .find(|(name, _)| name == &predicate.parameter)
            .and_then(|(_, ty)| ty.as_ref())
            .is_some_and(|actual| intersect_types(actual, &target).is_none())
            || refinements.falsy_ruled_out.iter().any(|(name, tag)| {
                name == &predicate.parameter
                    && narrow_to_single(std::slice::from_ref(&target), tag)
                        .is_some_and(|narrowed| narrowed == target)
            })
            || self.predicate_false_exclusion_is_proved(
                value,
                &predicate.parameter,
                &target,
                scope,
            );
        if !falsy_excludes_target {
            self.error_at(
                Code::InvalidTypePredicate,
                format!(
                    "the return condition does not exclude `{}` from `{}` when it is false; use `implies {} is {}` for a one-sided predicate",
                    format_type(&predicate.type_expr),
                    predicate.parameter,
                    predicate.parameter,
                    format_type(&predicate.type_expr)
                ),
                value.span,
            );
            return false;
        }
        true
    }

    fn predicate_uses_unvalidated_local_contract(
        &self,
        condition: &SNode,
        scope: &TypeScope,
    ) -> bool {
        let (condition, _) = self.resolve_flow_alias_node(condition, scope);
        match &condition.node {
            Node::FunctionCall { name, args, .. } => {
                let unvalidated = scope.get_fn(name).is_some_and(|signature| {
                    signature.type_predicate.is_some()
                        && signature.definition_span.is_some_and(|span| {
                            !self
                                .validated_type_predicates
                                .contains(&(span.start, span.end))
                        })
                });
                unvalidated
                    || args
                        .iter()
                        .any(|arg| self.predicate_uses_unvalidated_local_contract(arg, scope))
            }
            Node::MethodCall { object, args, .. } => {
                self.predicate_uses_unvalidated_local_contract(object, scope)
                    || args
                        .iter()
                        .any(|arg| self.predicate_uses_unvalidated_local_contract(arg, scope))
            }
            Node::BinaryOp { left, right, .. } => {
                self.predicate_uses_unvalidated_local_contract(left, scope)
                    || self.predicate_uses_unvalidated_local_contract(right, scope)
            }
            Node::UnaryOp { operand, .. } => {
                self.predicate_uses_unvalidated_local_contract(operand, scope)
            }
            _ => false,
        }
    }

    fn predicate_false_exclusion_is_proved(
        &self,
        condition: &SNode,
        parameter: &str,
        target: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        let (condition, _) = self.resolve_flow_alias_node(condition, scope);
        let same_target = |candidate: &TypeExpr| {
            self.types_compatible(target, candidate, scope)
                && self.types_compatible(candidate, target, scope)
        };
        match &condition.node {
            Node::FunctionCall { name, args, .. }
                if (name == "schema_is" || name == "is_type") && args.len() == 2 =>
            {
                matches!(&args[0].node, Node::Identifier(name) if name == parameter)
                    && schema_type_expr_from_node(&args[1], scope)
                        .is_some_and(|ty| same_target(&ty))
            }
            Node::FunctionCall {
                name,
                args,
                type_args,
                ..
            } => {
                let Some(signature) = scope.get_fn(name) else {
                    return false;
                };
                let Some(predicate) = &signature.type_predicate else {
                    return false;
                };
                if predicate.one_sided {
                    return false;
                }
                let Some(index) = signature
                    .params
                    .iter()
                    .position(|(name, _)| name == &predicate.parameter)
                else {
                    return false;
                };
                let Some(subject) = args.get(index) else {
                    return false;
                };
                if !matches!(&subject.node, Node::Identifier(name) if name == parameter) {
                    return false;
                }
                let bindings =
                    self.infer_function_call_type_bindings(signature, type_args, args, scope);
                let candidate = super::super::substitute_type_expr(&predicate.type_expr, &bindings);
                same_target(&self.resolve_alias(&candidate, scope))
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                let Node::Identifier(alias) = &object.node else {
                    return false;
                };
                let Some(binding) = self.namespace_imports.get(alias) else {
                    return false;
                };
                let Some(predicate) = binding.member_type_predicates.get(method) else {
                    return false;
                };
                if predicate.one_sided {
                    return false;
                }
                let Some(index) = binding
                    .member_param_names
                    .get(method)
                    .and_then(|names| names.iter().position(|name| name == &predicate.parameter))
                else {
                    return false;
                };
                args.get(index).is_some_and(
                    |subject| matches!(&subject.node, Node::Identifier(name) if name == parameter),
                ) && same_target(&predicate.type_expr)
            }
            Node::BinaryOp { op, left, right } if op == "||" => {
                self.predicate_false_exclusion_is_proved(left, parameter, target, scope)
                    || self.predicate_false_exclusion_is_proved(right, parameter, target, scope)
            }
            Node::BinaryOp { op, left, right } if op == "&&" => {
                self.predicate_false_exclusion_is_proved(left, parameter, target, scope)
                    && self.predicate_false_exclusion_is_proved(right, parameter, target, scope)
            }
            _ => false,
        }
    }

    fn invalid_predicate_body(&mut self, predicate: &TypePredicate, span: Option<Span>) {
        self.error_at(
            Code::InvalidTypePredicate,
            "a type predicate body must contain only const aliases followed by one return condition"
                .to_string(),
            span.unwrap_or(predicate.span),
        );
    }

    fn return_type_allows_implicit_nil(&self, expected: &TypeExpr, scope: &TypeScope) -> bool {
        self.types_compatible(expected, &TypeExpr::Named("nil".into()), scope)
    }

    pub(in crate::typechecker) fn body_cannot_fall_through(
        &self,
        body: &[SNode],
        scope: &TypeScope,
    ) -> bool {
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
                        self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
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
                if self.can_check_contextual_closure(val, expected, scope) {
                    return;
                }
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
                ..
            } => {
                let refs = self.extract_refinements(condition, scope);
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
                    self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
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
                let refs = self.extract_refinements(condition, scope);
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
                let refs = self.extract_refinements(condition, scope);
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
