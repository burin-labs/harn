//! Per-statement / per-expression diagnostic walk.
//!
//! `check_node` is the workhorse `match` over `Node` variants — one arm
//! per syntactic construct, each emitting whatever diagnostics that
//! construct's static rules call for. `check_block` chains it across a
//! sequence of statements while tracking unreachable-code detection.
//!
//! Inline pattern helpers (`define_pattern_vars`,
//! `check_pattern_defaults`) and `check_attributes` live here because
//! they are only called from `check_node`'s arms.

use std::collections::BTreeMap;

use harn_lexer::Span;

use crate::ast::*;
use crate::builtin_signatures;

use super::super::binary_ops::infer_binary_op_type;
use super::super::exits::stmt_definitely_exits;
use super::super::format::{format_type, is_obvious_type, shape_mismatch_detail};
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{
    EnumDeclInfo, FnSignature, ImplMethodSig, InterfaceDeclInfo, StructDeclInfo, TypeAliasInfo,
    TypeScope,
};
use super::super::union::{
    discriminant_field, narrow_shape_union_by_tag, narrow_to_single, DiscriminantValue,
};
use super::super::{InlayHintInfo, TypeChecker};
use super::flow::{pattern_alternatives, resolve_union_shape_members};

impl TypeChecker {
    pub(in crate::typechecker) fn check_block(&mut self, stmts: &[SNode], scope: &mut TypeScope) {
        let mut definitely_exited = false;
        for stmt in stmts {
            if definitely_exited {
                self.warning_at("unreachable code".to_string(), stmt.span);
                break; // warn once per block
            }
            self.check_node(stmt, scope);
            if Self::stmt_definitely_exits(stmt) {
                definitely_exited = true;
            }
        }
    }

    /// Check whether a single statement definitely exits (delegates to the free function).
    fn stmt_definitely_exits(stmt: &SNode) -> bool {
        stmt_definitely_exits(stmt)
    }

    /// Define variables from a destructuring pattern in the given scope (as unknown type).
    fn define_pattern_vars(pattern: &BindingPattern, scope: &mut TypeScope, mutable: bool) {
        let define = |scope: &mut TypeScope, name: &str| {
            if mutable {
                scope.define_var_mutable(name, None);
            } else {
                scope.define_var(name, None);
            }
            scope.clear_nil_widenable(name);
        };
        match pattern {
            BindingPattern::Identifier(name) => {
                define(scope, name);
            }
            BindingPattern::Dict(fields) => {
                for field in fields {
                    let name = field.alias.as_deref().unwrap_or(&field.key);
                    define(scope, name);
                }
            }
            BindingPattern::List(elements) => {
                for elem in elements {
                    define(scope, &elem.name);
                }
            }
            BindingPattern::Pair(a, b) => {
                define(scope, a);
                define(scope, b);
            }
        }
    }

    /// Type-check default value expressions within a destructuring pattern.
    fn check_pattern_defaults(&mut self, pattern: &BindingPattern, scope: &mut TypeScope) {
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

    fn is_nil_type(ty: &TypeExpr) -> bool {
        matches!(ty, TypeExpr::Named(name) if name == "nil")
    }

    fn union_with_nil(ty: &TypeExpr) -> TypeExpr {
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

    pub(in crate::typechecker) fn check_node(&mut self, snode: &SNode, scope: &mut TypeScope) {
        let span = snode.span;
        match &snode.node {
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.check_node(value, scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if let Some(actual) = &inferred {
                            if !self.types_compatible(expected, actual, scope) {
                                let mut msg = format!(
                                    "'{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                );
                                if let Some(detail) = shape_mismatch_detail(expected, actual) {
                                    msg.push_str(&format!(" ({})", detail));
                                }
                                self.error_at(msg, span);
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
                    Self::define_pattern_vars(pattern, scope, false);
                }
            }

            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.check_node(value, scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if let Some(actual) = &inferred {
                            if !self.types_compatible(expected, actual, scope) {
                                let mut msg = format!(
                                    "'{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                );
                                if let Some(detail) = shape_mismatch_detail(expected, actual) {
                                    msg.push_str(&format!(" ({})", detail));
                                }
                                self.error_at(msg, span);
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
                    Self::define_pattern_vars(pattern, scope, true);
                }
            }

            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                ..
            } => {
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: return_type.clone(),
                    type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                    required_params,
                    where_clauses: where_clauses
                        .iter()
                        .map(|wc| (wc.type_name.clone(), wc.bound.clone()))
                        .collect(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig.clone());
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_fn_decl_variance(type_params, params, return_type.as_ref(), name, span);
                self.check_fn_body(type_params, params, return_type, body, where_clauses);
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
                    type_param_names: Vec::new(),
                    required_params,
                    where_clauses: Vec::new(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_fn_body(&[], params, return_type, body, &[]);
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

            Node::FunctionCall { name, args } => {
                self.check_call(name, args, scope, span);
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
                let refs = Self::extract_refinements(condition, scope);

                let mut then_scope = scope.child();
                refs.apply_truthy(&mut then_scope);
                // Strict types: schema_is/is_type in condition clears
                // untyped source in then-branch
                if self.strict_types {
                    if let Node::FunctionCall { name, args } = &condition.node {
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
                    // Infer loop variable type from iterable
                    let elem_type = match iter_type {
                        Some(TypeExpr::List(inner)) => Some(*inner),
                        Some(TypeExpr::Iter(inner)) => Some(*inner),
                        Some(TypeExpr::Applied { ref name, ref args })
                            if name == "Iter" && args.len() == 1 =>
                        {
                            Some(args[0].clone())
                        }
                        Some(TypeExpr::Named(n)) if n == "string" => {
                            Some(TypeExpr::Named("string".into()))
                        }
                        // Iterating a range always yields ints.
                        Some(TypeExpr::Named(n)) if n == "range" => {
                            Some(TypeExpr::Named("int".into()))
                        }
                        _ => None,
                    };
                    loop_scope.define_var(variable, elem_type);
                    loop_scope.clear_nil_widenable(variable);
                } else if let BindingPattern::Pair(a, b) = pattern {
                    // Pair destructuring: `for (k, v) in iter` — extract K, V
                    // from the yielded Pair<K, V>.
                    let (ka, vb) = match &iter_type {
                        Some(TypeExpr::Iter(inner)) => {
                            if let TypeExpr::Applied { name, args } = inner.as_ref() {
                                if name == "Pair" && args.len() == 2 {
                                    (Some(args[0].clone()), Some(args[1].clone()))
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            }
                        }
                        Some(TypeExpr::Applied { name, args })
                            if name == "Iter" && args.len() == 1 =>
                        {
                            if let TypeExpr::Applied { name: n2, args: a2 } = &args[0] {
                                if n2 == "Pair" && a2.len() == 2 {
                                    (Some(a2[0].clone()), Some(a2[1].clone()))
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            }
                        }
                        _ => (None, None),
                    };
                    loop_scope.define_var(a, ka);
                    loop_scope.define_var(b, vb);
                    loop_scope.clear_nil_widenable(a);
                    loop_scope.clear_nil_widenable(b);
                } else {
                    self.check_pattern_defaults(pattern, &mut loop_scope);
                    Self::define_pattern_vars(pattern, &mut loop_scope, false);
                }
                self.check_block(body, &mut loop_scope);
            }

            Node::WhileLoop { condition, body } => {
                self.check_node(condition, scope);
                let refs = Self::extract_refinements(condition, scope);
                let mut loop_scope = scope.child();
                refs.apply_truthy(&mut loop_scope);
                self.check_block(body, &mut loop_scope);
            }

            Node::RequireStmt { condition, message } => {
                self.check_node(condition, scope);
                if let Some(message) = message {
                    self.check_node(message, scope);
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
                    self.error_at(
                        "try* requires an enclosing function (fn, tool, or pipeline) so the rethrow has a target".to_string(),
                        span,
                    );
                }
                self.check_node(operand, scope);
            }

            Node::ReturnStmt {
                value: Some(val), ..
            } => {
                self.check_node(val, scope);
            }

            Node::Assignment {
                target, value, op, ..
            } => {
                self.check_node(value, scope);
                if let Node::Identifier(name) = &target.node {
                    let mut widened_slot_type: Option<TypeExpr> = None;
                    // Compile-time immutability check
                    if scope.get_var(name).is_some() && !scope.is_mutable(name) {
                        self.warning_at(
                            format!(
                                "Cannot assign to '{}': variable is immutable (declared with 'let')",
                                name
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
                                    self.error_at(
                                        format!(
                                            "can't assign {} to '{}' (declared as {})",
                                            format_type(actual),
                                            name,
                                            format_type(check_type)
                                        ),
                                        span,
                                    );
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
                                self.warning_at(
                                    format!(
                                        "Match pattern type mismatch: matching {} against {} literal",
                                        value_type_name, pattern_type
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
                self.check_node(right, scope);
                // Validate operator/type compatibility
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                if let (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) = (&lt, &rt) {
                    match op.as_str() {
                        "-" | "/" | "%" | "**" => {
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.error_at(
                                    format!(
                                        "can't use '{}' on {} and {} (needs numeric operands)",
                                        op, l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        "*" => {
                            let numeric = ["int", "float"];
                            let is_numeric =
                                numeric.contains(&l.as_str()) && numeric.contains(&r.as_str());
                            let is_string_repeat =
                                (l == "string" && r == "int") || (l == "int" && r == "string");
                            if !is_numeric && !is_string_repeat {
                                self.error_at(
                                    format!("can't multiply {} and {} (try string * int)", l, r),
                                    span,
                                );
                            }
                        }
                        "+" => {
                            let valid = matches!(
                                (l.as_str(), r.as_str()),
                                ("int" | "float", "int" | "float")
                                    | ("string", "string")
                                    | ("list", "list")
                                    | ("dict", "dict")
                            );
                            if !valid {
                                let msg = format!("can't add {} and {}", l, r);
                                // Offer interpolation fix when one side is string
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(msg, span, fix);
                                } else {
                                    self.error_at(msg, span);
                                }
                            }
                        }
                        "<" | ">" | "<=" | ">=" => {
                            let comparable = ["int", "float", "string"];
                            if !comparable.contains(&l.as_str())
                                || !comparable.contains(&r.as_str())
                            {
                                self.warning_at(
                                    format!(
                                        "Comparison '{}' may not be meaningful for types {} and {}",
                                        op, l, r
                                    ),
                                    span,
                                );
                            } else if (l == "string") != (r == "string") {
                                self.warning_at(
                                    format!(
                                        "Comparing {} with {} using '{}' may give unexpected results",
                                        l, r, op
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
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.check_node(object, scope);
                for arg in args {
                    self.check_node(arg, scope);
                }
                // Definition-site generic checking: if the object's type is a
                // constrained generic param (where T: Interface), verify the
                // method exists in the bound interface.
                if let Some(TypeExpr::Named(type_name)) = self.infer_type(object, scope) {
                    if scope.is_generic_type_param(&type_name) {
                        if let Some(iface_name) = scope.get_where_constraint(&type_name) {
                            if let Some(iface_methods) = scope.get_interface(iface_name) {
                                let has_method =
                                    iface_methods.methods.iter().any(|m| m.name == *method);
                                if !has_method {
                                    self.warning_at(
                                        format!(
                                            "Method '{}' not found in interface '{}' (constraint on '{}')",
                                            method, iface_name, type_name
                                        ),
                                        span,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                if self.strict_types {
                    // Direct property access on boundary function result
                    if let Node::FunctionCall { name, args } = &object.node {
                        if builtin_signatures::is_untyped_boundary_source(name) {
                            let has_schema = (name == "llm_call" || name == "llm_completion")
                                && Self::llm_call_has_typed_schema_option(args, scope);
                            if !has_schema {
                                self.warning_at_with_help(
                                    format!(
                                        "Direct property access on unvalidated `{}()` result",
                                        name
                                    ),
                                    span,
                                    "assign to a variable and validate with schema_expect() or a type annotation first".to_string(),
                                );
                            }
                        }
                    }
                    // Property access on known untyped variable
                    if let Node::Identifier(name) = &object.node {
                        if let Some(source) = scope.is_untyped_source(name) {
                            self.warning_at_with_help(
                                format!(
                                    "Accessing property on unvalidated value '{}' from `{}`",
                                    name, source
                                ),
                                span,
                                "validate with schema_expect(), schema_is() in an if-condition, or add a shape type annotation".to_string(),
                            );
                        }
                    }
                }
                self.check_node(object, scope);
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                if self.strict_types {
                    if let Node::FunctionCall { name, args } = &object.node {
                        if builtin_signatures::is_untyped_boundary_source(name) {
                            let has_schema = (name == "llm_call" || name == "llm_completion")
                                && Self::llm_call_has_typed_schema_option(args, scope);
                            if !has_schema {
                                self.warning_at_with_help(
                                    format!(
                                        "Direct subscript access on unvalidated `{}()` result",
                                        name
                                    ),
                                    span,
                                    "assign to a variable and validate with schema_expect() or a type annotation first".to_string(),
                                );
                            }
                        }
                    }
                    if let Node::Identifier(name) = &object.node {
                        if let Some(source) = scope.is_untyped_source(name) {
                            self.warning_at_with_help(
                                format!(
                                    "Subscript access on unvalidated value '{}' from `{}`",
                                    name, source
                                ),
                                span,
                                "validate with schema_expect(), schema_is() in an if-condition, or add a shape type annotation".to_string(),
                            );
                        }
                    }
                }
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
                let refs = Self::extract_refinements(condition, scope);

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
                let refs = Self::extract_refinements(condition, scope);

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
                        ParallelMode::Each | ParallelMode::Settle => {
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

            Node::MutexBlock { body } | Node::DeferStmt { body } => {
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::Retry { count, body } => {
                self.check_node(count, scope);
                let mut retry_scope = scope.child();
                self.check_block(body, &mut retry_scope);
            }

            Node::Closure { params, body, .. } => {
                let mut closure_scope = scope.child();
                for p in params {
                    closure_scope.define_var(&p.name, p.type_expr.clone());
                    closure_scope.clear_nil_widenable(&p.name);
                }
                self.fn_depth += 1;
                self.check_block(body, &mut closure_scope);
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
                if let Some(v) = value {
                    self.check_node(v, scope);
                }
            }

            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                for entry in fields {
                    self.check_node(&entry.key, scope);
                    self.check_node(&entry.value, scope);
                }
                if let Some(struct_info) = scope.get_struct(struct_name).cloned() {
                    let type_bindings = self.infer_struct_bindings(&struct_info, fields, scope);
                    // Warn on unknown fields
                    for entry in fields {
                        if let Node::StringLiteral(key) | Node::Identifier(key) = &entry.key.node {
                            if !struct_info.fields.iter().any(|field| field.name == *key) {
                                self.warning_at(
                                    format!("Unknown field '{}' in struct '{}'", key, struct_name),
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
                        let Some(entry) = fields.iter().find(|entry| {
                            matches!(&entry.key.node, Node::StringLiteral(key) | Node::Identifier(key) if key == &field.name)
                        }) else {
                            continue;
                        };
                        let Some(actual_type) = self.infer_type(&entry.value, scope) else {
                            continue;
                        };
                        let expected = Self::apply_type_bindings(expected_type, &type_bindings);
                        if !self.types_compatible(&expected, &actual_type, scope) {
                            self.error_at(
                                format!(
                                    "Field '{}' in struct '{}' expects {}, got {}",
                                    field.name,
                                    struct_name,
                                    format_type(&expected),
                                    format_type(&actual_type)
                                ),
                                entry.value.span,
                            );
                        }
                    }
                } else {
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
                            message,
                            span,
                            format!("declare `struct {candidate} {{ ... }}` or fix the type name"),
                        ),
                        None => self.error_at_with_help(
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
                for arg in args {
                    self.check_node(arg, scope);
                }
                if let Some(enum_info) = scope.get_enum(enum_name).cloned() {
                    let Some(enum_variant) = enum_info
                        .variants
                        .iter()
                        .find(|enum_variant| enum_variant.name == *variant)
                    else {
                        self.warning_at(
                            format!("Unknown variant '{}' in enum '{}'", variant, enum_name),
                            span,
                        );
                        return;
                    };
                    if args.len() != enum_variant.fields.len() {
                        self.warning_at(
                            format!(
                                "{}.{} expects {} argument(s), got {}",
                                enum_name,
                                variant,
                                enum_variant.fields.len(),
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
                            self.error_at(message, arg.span);
                        }
                    }
                    for (field, arg) in enum_variant.fields.iter().zip(args.iter()) {
                        let Some(expected_type) = &field.type_expr else {
                            continue;
                        };
                        let Some(actual_type) = self.infer_type(arg, scope) else {
                            continue;
                        };
                        let expected = Self::apply_type_bindings(expected_type, &type_bindings);
                        if !self.types_compatible(&expected, &actual_type, scope) {
                            self.error_at(
                                format!(
                                    "{}.{} expects {}: {}, got {}",
                                    enum_name,
                                    variant,
                                    field.name,
                                    format_type(&expected),
                                    format_type(&actual_type)
                                ),
                                arg.span,
                            );
                        }
                    }
                }
            }

            Node::InterpolatedString(_) => {}

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

    /// Validate attribute usage and emit warnings for unknown attributes.
    /// Recognized attribute names: `deprecated`, `test`, `complexity`,
    /// `acp_tool`, `acp_skill`, `invariant`, `deterministic`, `semantic`,
    /// `archivist`, `retroactive`. All other names produce a warning so
    /// misspellings surface early without breaking compilation.
    ///
    /// Flow predicate cross-attribute rules (epic #571 / #579):
    /// - A bare `@invariant` (no arguments) is the Flow predicate marker.
    ///   It must be paired with exactly one of `@deterministic`/`@semantic`
    ///   and an `@archivist(...)` provenance block. The handler-IR
    ///   `@invariant("name", ...)` form (positional args) is a separate
    ///   feature validated in `harn_ir` and is left untouched here.
    /// - `@deterministic` and `@semantic` are mutually exclusive.
    /// - `@archivist(...)` and `@retroactive` only make sense on Flow
    ///   predicate functions; we warn if they appear without `@invariant`.
    pub(in crate::typechecker) fn check_attributes(
        &mut self,
        attributes: &[Attribute],
        inner: &SNode,
    ) {
        for attr in attributes {
            match attr.name.as_str() {
                "deprecated" | "test" | "complexity" | "acp_tool" | "acp_skill" | "invariant"
                | "deterministic" | "semantic" | "archivist" | "retroactive" => {}
                other => {
                    self.warning_at(format!("unknown attribute `@{}`", other), attr.span);
                }
            }
            // `@test` marks test pipelines discovered by `harn test`.
            if attr.name == "test" && !matches!(inner.node, Node::Pipeline { .. }) {
                self.warning_at(
                    "`@test` only applies to pipeline declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_tool" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    "`@acp_tool` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_skill" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    "`@acp_skill` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(
                attr.name.as_str(),
                "deterministic" | "semantic" | "archivist" | "retroactive"
            ) && !matches!(inner.node, Node::FnDecl { .. })
            {
                self.warning_at(
                    format!("`@{}` only applies to function declarations", attr.name),
                    attr.span,
                );
            }
            if attr.name == "invariant"
                && !matches!(
                    inner.node,
                    Node::FnDecl { .. } | Node::ToolDecl { .. } | Node::Pipeline { .. }
                )
            {
                self.warning_at(
                    "`@invariant` only applies to function, tool, or pipeline declarations"
                        .to_string(),
                    attr.span,
                );
            }
        }

        // Flow predicate companion-attribute rules. These only apply when a
        // bare `@invariant` (no arguments) is present — that's the Flow
        // predicate marker. Handler-IR-style `@invariant("name", ...)` keeps
        // its existing semantics validated by `harn_ir`.
        let flow_invariant = attributes
            .iter()
            .find(|a| a.name == "invariant" && a.args.is_empty());
        let deterministic = attributes.iter().find(|a| a.name == "deterministic");
        let semantic = attributes.iter().find(|a| a.name == "semantic");
        let archivist = attributes.iter().find(|a| a.name == "archivist");
        let retroactive = attributes.iter().find(|a| a.name == "retroactive");

        if let (Some(det), Some(sem)) = (deterministic, semantic) {
            self.warning_at(
                "`@deterministic` and `@semantic` are mutually exclusive; \
                 a Flow predicate is one mode or the other"
                    .to_string(),
                Span::merge(sem.span, det.span),
            );
        }

        if let Some(inv) = flow_invariant {
            if deterministic.is_none() && semantic.is_none() {
                self.warning_at(
                    "Flow `@invariant` requires exactly one of `@deterministic` \
                     (default) or `@semantic`"
                        .to_string(),
                    inv.span,
                );
            }
            if archivist.is_none() {
                self.warning_at(
                    "Flow `@invariant` is missing `@archivist(...)` provenance \
                     (evidence, confidence, source_date, coverage_examples)"
                        .to_string(),
                    inv.span,
                );
            }
        } else {
            if let Some(arch) = archivist {
                self.warning_at(
                    "`@archivist(...)` only applies to Flow predicates marked \
                     with `@invariant`"
                        .to_string(),
                    arch.span,
                );
            }
            if let Some(retro) = retroactive {
                self.warning_at(
                    "`@retroactive` only applies to Flow predicates marked \
                     with `@invariant`"
                        .to_string(),
                    retro.span,
                );
            }
        }

        if let Some(arch) = archivist {
            self.validate_archivist_args(arch);
        }
    }

    /// Sanity-check the shape of an `@archivist(...)` block.
    ///
    /// Recognized arguments (all optional individually, but `evidence`
    /// must be present for the block to carry meaningful provenance):
    /// - `evidence`: list of URL strings (the linter only checks that the
    ///   key exists; deep validation lives in the Archivist persona).
    /// - `confidence`: float between 0.0 and 1.0
    /// - `source_date`: string (ISO-8601 date)
    /// - `coverage_examples`: list of strings
    ///
    /// Unknown keys produce a warning so typos surface early.
    pub(in crate::typechecker) fn validate_archivist_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["evidence", "confidence", "source_date", "coverage_examples"];

        let mut has_evidence = false;
        for arg in &attr.args {
            let Some(name) = arg.name.as_deref() else {
                self.warning_at(
                    "`@archivist(...)` arguments must be named (e.g. \
                     `evidence: [...], confidence: 0.9`)"
                        .to_string(),
                    arg.span,
                );
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    format!(
                        "unknown `@archivist` argument `{name}`; expected one of \
                         {KNOWN_KEYS:?}"
                    ),
                    arg.span,
                );
                continue;
            }
            if name == "evidence" {
                has_evidence = true;
            }
            // Confidence must be a number between 0 and 1 when supplied as a
            // literal. Bare identifiers (e.g. a constant reference) are
            // allowed and validated at runtime.
            if name == "confidence" {
                match &arg.value.node {
                    Node::FloatLiteral(f) if (0.0..=1.0).contains(f) => {}
                    Node::IntLiteral(i) if *i == 0 || *i == 1 => {}
                    Node::Identifier(_) => {}
                    _ => {
                        self.warning_at(
                            "`@archivist(confidence: ...)` must be a float in \
                             [0.0, 1.0]"
                                .to_string(),
                            arg.span,
                        );
                    }
                }
            }
        }

        if !has_evidence {
            self.warning_at(
                "`@archivist(...)` should declare `evidence: [...]` so \
                 predicates can be audited"
                    .to_string(),
                attr.span,
            );
        }
    }
}

/// Narrow a union-typed match value by a single arm pattern. Returns
/// the narrowed type, or `None` when the pattern is not a recognised
/// type-narrowing literal. For `OrPattern`, the per-alternative
/// narrowings are combined into a union (deduped) so a two-alternative
/// arm on a three-member literal union refines to a two-member union.
fn narrow_union_by_arm_pattern(pattern: &SNode, members: &[TypeExpr]) -> Option<TypeExpr> {
    let leaves = pattern_alternatives(pattern);
    let mut collected: Vec<TypeExpr> = Vec::new();
    for leaf in &leaves {
        let narrowed = narrow_union_leaf(&leaf.node, members)?;
        match narrowed {
            TypeExpr::Union(inner) => {
                for m in inner {
                    if !collected.contains(&m) {
                        collected.push(m);
                    }
                }
            }
            other => {
                if !collected.contains(&other) {
                    collected.push(other);
                }
            }
        }
    }
    match collected.len() {
        0 => None,
        1 => Some(collected.into_iter().next().unwrap()),
        _ => Some(TypeExpr::Union(collected)),
    }
}

fn narrow_union_leaf(node: &Node, members: &[TypeExpr]) -> Option<TypeExpr> {
    // Literal pattern against a union containing the exact literal
    // value — narrow to that literal. This is what makes
    // `"pos" | "neg"` on a `"pos" | "neg" | "zero"` union refine
    // correctly: each alternative picks out its literal member.
    match node {
        Node::StringLiteral(s)
            if members
                .iter()
                .any(|m| matches!(m, TypeExpr::LitString(lit) if lit == s)) =>
        {
            return Some(TypeExpr::LitString(s.clone()));
        }
        Node::IntLiteral(v)
            if members
                .iter()
                .any(|m| matches!(m, TypeExpr::LitInt(lit) if lit == v)) =>
        {
            return Some(TypeExpr::LitInt(*v));
        }
        _ => {}
    }
    let type_name = match node {
        Node::NilLiteral => "nil",
        Node::StringLiteral(_) => "string",
        Node::IntLiteral(_) => "int",
        Node::FloatLiteral(_) => "float",
        Node::BoolLiteral(_) => "bool",
        _ => return None,
    };
    narrow_to_single(members, type_name)
}

/// Narrow a tagged shape union by a single arm pattern on its
/// discriminant. For `OrPattern`, the matched shape variants are
/// combined into a union so `"ping" | "pong" -> …` refines `obj` to
/// `{kind:"ping",…} | {kind:"pong",…}` inside the arm.
fn narrow_shape_union_by_arm_pattern(
    pattern: &SNode,
    members: &[TypeExpr],
    property: &str,
) -> Option<TypeExpr> {
    let leaves = pattern_alternatives(pattern);
    let mut matched: Vec<TypeExpr> = Vec::new();
    for leaf in &leaves {
        let tag = match &leaf.node {
            Node::StringLiteral(s) => DiscriminantValue::Str(s.clone()),
            Node::IntLiteral(v) => DiscriminantValue::Int(*v),
            _ => return None,
        };
        let (shape, _) = narrow_shape_union_by_tag(members, property, &tag)?;
        if !matched.contains(&shape) {
            matched.push(shape);
        }
    }
    match matched.len() {
        0 => None,
        1 => Some(matched.into_iter().next().unwrap()),
        _ => Some(TypeExpr::Union(matched)),
    }
}
