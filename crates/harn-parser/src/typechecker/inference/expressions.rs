//! Pure expression-typing inference (no diagnostics).
//!
//! `infer_type` is the central dispatcher: given an expression node and a
//! scope it returns the inferred [`InferredType`] (`None` = gradual /
//! unknown). The supporting helpers (`infer_block_type`,
//! `infer_list_literal_type`, `infer_try_error_type`) factor out shape
//! analysis for compound expressions.
//!
//! No method here is allowed to emit a diagnostic — the inference walk
//! also runs from contexts that should stay silent (e.g. probing a
//! `Ternary`'s arm types for `infer_type`'s union merge).

use std::collections::BTreeMap;

use crate::ast::*;
use crate::builtin_signatures;
use crate::diagnostic_codes::Code;
use harn_lexer::{FixEdit, Span};

use super::super::binary_ops::infer_binary_op_type;
use super::super::scope::{builtin_return_type, InferredType, TypeScope};
use super::super::union::simplify_union;
use super::super::TypeChecker;

const UNNECESSARY_SAFE_NAVIGATION_RULE: &str = "unnecessary-safe-navigation";

enum SafeNavigationKind<'a> {
    Subscript,
    Property(&'a str),
    Method,
}

impl TypeChecker {
    pub(in crate::typechecker) fn infer_try_error_type(
        &self,
        stmts: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let mut inferred: Vec<TypeExpr> = Vec::new();
        for stmt in stmts {
            match &stmt.node {
                Node::ThrowStmt { value } => {
                    if let Some(ty) = self.infer_type(value, scope) {
                        inferred.push(ty);
                    }
                }
                Node::TryOperator { operand } => {
                    if let Some(TypeExpr::Applied { name, args }) = self.infer_type(operand, scope)
                    {
                        if name == "Result" && args.len() == 2 {
                            inferred.push(args[1].clone());
                        }
                    }
                }
                Node::IfElse {
                    then_body,
                    else_body,
                    ..
                } => {
                    if let Some(ty) = self.infer_try_error_type(then_body, scope) {
                        inferred.push(ty);
                    }
                    if let Some(else_body) = else_body {
                        if let Some(ty) = self.infer_try_error_type(else_body, scope) {
                            inferred.push(ty);
                        }
                    }
                }
                Node::Block(body)
                | Node::TryExpr { body }
                | Node::SpawnExpr { body }
                | Node::Retry { body, .. }
                | Node::CostRoute { body, .. }
                | Node::WhileLoop { body, .. }
                | Node::DeferStmt { body }
                | Node::MutexBlock { body }
                | Node::DeadlineBlock { body, .. }
                | Node::Pipeline { body, .. }
                | Node::OverrideDecl { body, .. } => {
                    if let Some(ty) = self.infer_try_error_type(body, scope) {
                        inferred.push(ty);
                    }
                }
                _ => {}
            }
        }
        if inferred.is_empty() {
            None
        } else {
            Some(simplify_union(inferred))
        }
    }

    pub(in crate::typechecker) fn infer_list_literal_type(
        &self,
        items: &[SNode],
        scope: &TypeScope,
    ) -> TypeExpr {
        let mut inferred: Option<TypeExpr> = None;
        for item in items {
            let Some(item_type) = self.infer_type(item, scope) else {
                return TypeExpr::Named("list".into());
            };
            inferred = Some(match inferred {
                None => item_type,
                Some(current) if current == item_type => current,
                Some(TypeExpr::Union(mut members)) => {
                    if !members.contains(&item_type) {
                        members.push(item_type);
                    }
                    TypeExpr::Union(members)
                }
                Some(current) => TypeExpr::Union(vec![current, item_type]),
            });
        }
        inferred
            .map(|item_type| TypeExpr::List(Box::new(item_type)))
            .unwrap_or_else(|| TypeExpr::Named("list".into()))
    }

    fn infer_match_expr_type(
        &self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
    ) -> InferredType {
        let value_type = self.infer_type(value, scope);
        let mut arm_types = Vec::new();
        for arm in arms {
            let mut arm_scope = scope.child();
            self.define_match_pattern_bindings(&arm.pattern, value_type.as_ref(), &mut arm_scope);
            if let Some(arm_type) = self.infer_block_type(&arm.body, &arm_scope) {
                arm_types.push(arm_type);
            }
        }
        match (arms.is_empty(), arm_types.len()) {
            (true, _) => Some(TypeExpr::Never),
            (false, 0) => None,
            (false, 1) => arm_types.pop(),
            (false, _) => Some(simplify_union(arm_types)),
        }
    }

    fn define_match_pattern_bindings(
        &self,
        pattern: &SNode,
        value_type: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) {
        match &pattern.node {
            Node::Identifier(name) if name != "_" => {
                scope.define_var(name, value_type.cloned());
            }
            Node::ListLiteral(elements) => {
                let item_type = value_type.and_then(|ty| match self.resolve_alias(ty, scope) {
                    TypeExpr::List(inner) => Some(*inner),
                    _ => None,
                });
                for element in elements {
                    if let Node::Identifier(name) = &element.node {
                        if name != "_" {
                            scope.define_var(name, item_type.clone());
                        }
                    }
                }
            }
            Node::DictLiteral(entries) => {
                for entry in entries {
                    let Some(key) = (match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => Some(key.as_str()),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let Node::Identifier(name) = &entry.value.node else {
                        continue;
                    };
                    if name == "_" {
                        continue;
                    }
                    let binding_type =
                        value_type.and_then(|ty| match self.resolve_alias(ty, scope) {
                            TypeExpr::Shape(fields) => fields
                                .into_iter()
                                .find(|field| field.name == key)
                                .map(|field| field.type_expr),
                            TypeExpr::DictType(_, value) => Some(*value),
                            _ => None,
                        });
                    scope.define_var(name, binding_type);
                }
            }
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                self.define_enum_pattern_bindings(enum_name, variant, args, scope);
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                if let Node::Identifier(enum_name) = &object.node {
                    self.define_enum_pattern_bindings(enum_name, method, args, scope);
                }
            }
            _ => {}
        }
    }

    fn define_enum_pattern_bindings(
        &self,
        enum_name: &str,
        variant: &str,
        args: &[SNode],
        scope: &mut TypeScope,
    ) {
        let Some(enum_info) = scope.get_enum(enum_name) else {
            return;
        };
        let Some(variant_info) = enum_info.variants.iter().find(|v| v.name == variant) else {
            return;
        };
        let bindings: Vec<(String, InferredType)> = args
            .iter()
            .zip(&variant_info.fields)
            .filter_map(|(arg, field)| match &arg.node {
                Node::Identifier(name) if name != "_" => {
                    Some((name.clone(), field.type_expr.clone()))
                }
                _ => None,
            })
            .collect();
        for (name, ty) in bindings {
            scope.define_var(&name, ty);
        }
    }

    /// Infer the type of an expression.
    pub(in crate::typechecker) fn infer_type(
        &self,
        snode: &SNode,
        scope: &TypeScope,
    ) -> InferredType {
        match &snode.node {
            Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
            Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
            Node::StringLiteral(_) | Node::InterpolatedString(_) => {
                Some(TypeExpr::Named("string".into()))
            }
            Node::BoolLiteral(_) => Some(TypeExpr::Named("bool".into())),
            Node::NilLiteral => Some(TypeExpr::Named("nil".into())),
            Node::ListLiteral(items) => Some(self.infer_list_literal_type(items, scope)),
            // `a to b` (and `a to b exclusive`) produce a lazy Range value.
            // Expose it as a named `range` type; for-in and method resolution
            // special-case this type where needed.
            Node::RangeExpr { .. } => Some(TypeExpr::Named("range".into())),
            Node::HitlExpr { kind, args } => Some(self.hitl_expr_inferred_type(*kind, args, scope)),
            Node::DictLiteral(entries) => {
                // Infer shape type when all keys are string literals
                let mut fields = Vec::new();
                for entry in entries {
                    let key = match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => key.clone(),
                        _ => return Some(TypeExpr::Named("dict".into())),
                    };
                    let val_type = self
                        .infer_type(&entry.value, scope)
                        .unwrap_or_else(Self::wildcard_type);
                    fields.push(ShapeField {
                        name: key,
                        type_expr: val_type,
                        optional: false,
                    });
                }
                if !fields.is_empty() {
                    Some(TypeExpr::Shape(fields))
                } else {
                    Some(TypeExpr::Named("dict".into()))
                }
            }
            Node::Closure { params, body, .. } => {
                // If all params are typed and we can infer a return type, produce FnType
                let all_typed = params.iter().all(|p| p.type_expr.is_some());
                if all_typed && !params.is_empty() {
                    let param_types: Vec<TypeExpr> =
                        params.iter().filter_map(|p| p.type_expr.clone()).collect();
                    // Infer return type in a scope that includes the
                    // closure's typed params; otherwise the body's
                    // last-expression reference to a param is treated
                    // as an unbound name and the closure falls back to
                    // the opaque `closure` type, which has no body
                    // checks against an expected `fn(...)` slot.
                    let mut closure_scope = scope.child();
                    for param in params {
                        closure_scope.define_var(&param.name, param.type_expr.clone());
                    }
                    let ret = body
                        .last()
                        .and_then(|last| self.infer_type(last, &closure_scope));
                    if let Some(ret_type) = ret {
                        return Some(TypeExpr::FnType {
                            params: param_types,
                            return_type: Box::new(ret_type),
                        });
                    }
                }
                Some(TypeExpr::Named("closure".into()))
            }

            Node::Identifier(name) => {
                if let Some(ty) = scope.get_var(name).cloned().flatten() {
                    return Some(ty);
                }
                // When a bare identifier names a top-level or nested function,
                // treat the reference as an `fn(...) -> R` value. Prior to this,
                // `Identifier` fell through to `None` for functions, which made
                // function references in dict/list literals collapse to `nil`
                // and silently break assignability against typed slots.
                if let Some(sig) = scope.get_fn(name).cloned() {
                    let params = sig
                        .params
                        .into_iter()
                        .map(|(_, ty)| ty.unwrap_or_else(Self::wildcard_type))
                        .collect();
                    let return_type = sig.return_type.unwrap_or(TypeExpr::Named("nil".into()));
                    return Some(TypeExpr::FnType {
                        params,
                        return_type: Box::new(return_type),
                    });
                }
                None
            }

            Node::FunctionCall {
                name,
                type_args,
                args,
            } => {
                if name == "schema_of" && args.len() == 1 {
                    if let Node::Identifier(alias) = &args[0].node {
                        if let Some(resolved) = scope.resolve_type(alias) {
                            return Some(TypeExpr::Applied {
                                name: "Schema".into(),
                                args: vec![resolved.clone()],
                            });
                        }
                    }
                }
                // Struct constructor calls return the struct type
                if let Some(struct_info) = scope.get_struct(name) {
                    return Some(Self::applied_type_or_name(
                        name,
                        struct_info
                            .type_params
                            .iter()
                            .map(|_| Self::wildcard_type())
                            .collect(),
                    ));
                }
                if name == "Ok" {
                    let ok_type = args
                        .first()
                        .and_then(|arg| self.infer_type(arg, scope))
                        .unwrap_or_else(Self::wildcard_type);
                    return Some(TypeExpr::Applied {
                        name: "Result".into(),
                        args: vec![ok_type, Self::wildcard_type()],
                    });
                }
                if name == "Err" {
                    let err_type = args
                        .first()
                        .and_then(|arg| self.infer_type(arg, scope))
                        .unwrap_or_else(Self::wildcard_type);
                    return Some(TypeExpr::Applied {
                        name: "Result".into(),
                        args: vec![Self::wildcard_type(), err_type],
                    });
                }
                // Check user-defined function return types
                if let Some(sig) = scope.get_fn(name).cloned() {
                    let mut return_type = sig.return_type.clone();
                    if let Some(ty) = return_type.take() {
                        if sig.type_param_names.is_empty() {
                            return Some(ty);
                        }
                        let mut bindings = BTreeMap::new();
                        let type_param_set: std::collections::BTreeSet<String> =
                            sig.type_param_names.iter().cloned().collect();
                        if type_args.len() == sig.type_param_names.len() {
                            for (param_name, type_arg) in sig.type_param_names.iter().zip(type_args)
                            {
                                bindings.insert(param_name.clone(), type_arg.clone());
                            }
                        }
                        for (arg, (_param_name, param_type)) in args.iter().zip(sig.params.iter()) {
                            if let Some(param_ty) = param_type {
                                let _ = self.bind_from_arg_node(
                                    param_ty,
                                    arg,
                                    &type_param_set,
                                    &mut bindings,
                                    scope,
                                );
                            }
                        }
                        // Type parameters that the call site never pinned (e.g.
                        // `pick_keys({})` with an empty literal) would otherwise
                        // surface as a phantom `Named("V")` in downstream
                        // checks. Default them to the wildcard so they behave
                        // like an unconstrained generic in the result type.
                        for tp in &sig.type_param_names {
                            bindings
                                .entry(tp.clone())
                                .or_insert_with(Self::wildcard_type);
                        }
                        return Some(Self::apply_type_bindings(&ty, &bindings));
                    }
                    return None;
                }
                if Self::builtin_preserves_first_arg_type(name) {
                    if let Some(first_type) =
                        args.first().and_then(|arg| self.infer_type(arg, scope))
                    {
                        return Some(first_type);
                    }
                }
                // Generic builtins (llm_call, schema_parse/check/expect):
                // bind T by walking each arg node against the param
                // TypeExpr, then apply bindings to the declared return
                // type. Falls through to `builtin_return_type` when no T
                // can be bound (e.g. llm_call without an output_schema
                // option).
                if let Some(sig) = builtin_signatures::lookup(name).filter(|s| s.is_generic()) {
                    let type_param_names = sig.type_param_names();
                    let type_param_set: std::collections::BTreeSet<String> =
                        type_param_names.iter().cloned().collect();
                    let mut bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
                    if type_args.len() == type_param_names.len() {
                        for (param_name, type_arg) in type_param_names.iter().zip(type_args) {
                            bindings.insert(param_name.clone(), type_arg.clone());
                        }
                    }
                    let param_types = sig.param_type_exprs();
                    for (arg, param_ty) in args.iter().zip(param_types.iter()) {
                        let _ = self.bind_from_arg_node(
                            param_ty,
                            arg,
                            &type_param_set,
                            &mut bindings,
                            scope,
                        );
                    }
                    let all_bound = type_param_names.iter().all(|tp| bindings.contains_key(tp));
                    if all_bound {
                        return Some(Self::apply_type_bindings(
                            &sig.return_type_expr(),
                            &bindings,
                        ));
                    }
                }
                // Check builtin return types
                builtin_return_type(name)
            }

            Node::BinaryOp { op, left, right } => {
                if op == "|>" {
                    return self.infer_pipe_type(left, right, scope);
                }
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                infer_binary_op_type(op, &lt, &rt)
            }

            Node::UnaryOp { op, operand } => {
                let t = self.infer_type(operand, scope);
                match op.as_str() {
                    "!" => Some(TypeExpr::Named("bool".into())),
                    "-" => t, // negation preserves type
                    _ => None,
                }
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let refs = Self::extract_refinements(condition, scope);

                let mut true_scope = scope.child();
                refs.apply_truthy(&mut true_scope);
                let tt = self.infer_type(true_expr, &true_scope);

                let mut false_scope = scope.child();
                refs.apply_falsy(&mut false_scope);
                let ft = self.infer_type(false_expr, &false_scope);

                match (&tt, &ft) {
                    (Some(a), Some(b)) if a == b => tt,
                    (Some(a), Some(b)) => Some(TypeExpr::Union(vec![a.clone(), b.clone()])),
                    (Some(_), None) => tt,
                    (None, Some(_)) => ft,
                    (None, None) => None,
                }
            }

            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                if let Some(enum_info) = scope.get_enum(enum_name) {
                    Some(self.infer_enum_type(enum_name, enum_info, variant, args, scope))
                } else {
                    Some(TypeExpr::Named(enum_name.clone()))
                }
            }

            Node::PropertyAccess { object, property } => {
                self.infer_property_access_type(object, property, scope, false)
            }
            Node::OptionalPropertyAccess { object, property } => {
                self.infer_property_access_type(object, property, scope, true)
            }

            Node::SubscriptAccess { object, index } => {
                self.infer_subscript_access_type(object, index, scope, false)
            }
            Node::OptionalSubscriptAccess { object, index } => {
                self.infer_subscript_access_type(object, index, scope, true)
            }
            Node::SliceAccess { object, .. } => {
                // Slicing a list returns the same list type; slicing a string returns string
                let obj_type = self.infer_type(object, scope);
                match &obj_type {
                    Some(TypeExpr::List(_)) => obj_type,
                    Some(TypeExpr::Named(n)) if n == "list" => obj_type,
                    Some(TypeExpr::Named(n)) if n == "string" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    _ => None,
                }
            }
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let optional_access = matches!(&snode.node, Node::OptionalMethodCall { .. });
                if let Node::Identifier(name) = &object.node {
                    if let Some(enum_info) = scope.get_enum(name) {
                        return Some(self.infer_enum_type(name, enum_info, method, args, scope));
                    }
                    if name == "Result" && (method == "Ok" || method == "Err") {
                        let ok_type = if method == "Ok" {
                            args.first()
                                .and_then(|arg| self.infer_type(arg, scope))
                                .unwrap_or_else(Self::wildcard_type)
                        } else {
                            Self::wildcard_type()
                        };
                        let err_type = if method == "Err" {
                            args.first()
                                .and_then(|arg| self.infer_type(arg, scope))
                                .unwrap_or_else(Self::wildcard_type)
                        } else {
                            Self::wildcard_type()
                        };
                        return Some(TypeExpr::Applied {
                            name: "Result".into(),
                            args: vec![ok_type, err_type],
                        });
                    }
                }
                let obj_type = self.infer_type(object, scope);
                let include_optional_nil = optional_access
                    && obj_type
                        .as_ref()
                        .is_some_and(|ty| self.type_may_include_nil(ty, scope));
                let result = |ty| Self::optional_method_result_type(ty, include_optional_nil);
                // Iter<T> receiver: combinators preserve or transform T; sinks
                // materialize. This must come before the shared-method match
                // below so `.map` / `.filter` / etc. on an iter return Iter,
                // not list.
                let iter_elem_type: Option<TypeExpr> = match &obj_type {
                    Some(TypeExpr::Iter(inner)) => Some((**inner).clone()),
                    Some(TypeExpr::Named(n)) if n == "iter" => Some(TypeExpr::Named("any".into())),
                    _ => None,
                };
                if let Some(t) = iter_elem_type {
                    let pair = |k: TypeExpr, v: TypeExpr| TypeExpr::Applied {
                        name: "Pair".into(),
                        args: vec![k, v],
                    };
                    let iter_of = |ty: TypeExpr| TypeExpr::Iter(Box::new(ty));
                    match method.as_str() {
                        "iter" => return Some(result(iter_of(t))),
                        "map" | "flat_map" => {
                            // Closure-return inference is not threaded here;
                            // fall back to a coarse `iter<any>` — matches the
                            // list-return style the rest of the checker uses.
                            return Some(result(TypeExpr::Named("iter".into())));
                        }
                        "filter" | "take" | "skip" | "take_while" | "skip_while" => {
                            return Some(result(iter_of(t)));
                        }
                        "zip" => {
                            return Some(result(iter_of(pair(t, TypeExpr::Named("any".into())))));
                        }
                        "enumerate" => {
                            return Some(result(iter_of(pair(TypeExpr::Named("int".into()), t))));
                        }
                        "chain" => return Some(result(iter_of(t))),
                        "chunks" | "windows" => {
                            return Some(result(iter_of(TypeExpr::List(Box::new(t)))));
                        }
                        // Sinks
                        "to_list" => return Some(result(TypeExpr::List(Box::new(t)))),
                        "to_set" => {
                            return Some(result(TypeExpr::Applied {
                                name: "set".into(),
                                args: vec![t],
                            }))
                        }
                        "to_dict" => return Some(result(TypeExpr::Named("dict".into()))),
                        "count" => return Some(result(TypeExpr::Named("int".into()))),
                        "sum" => {
                            return Some(result(TypeExpr::Union(vec![
                                TypeExpr::Named("int".into()),
                                TypeExpr::Named("float".into()),
                            ])))
                        }
                        "min" | "max" | "first" | "last" | "find" => {
                            return Some(result(TypeExpr::Union(vec![
                                t,
                                TypeExpr::Named("nil".into()),
                            ])));
                        }
                        "any" | "all" => return Some(result(TypeExpr::Named("bool".into()))),
                        "for_each" => return Some(result(TypeExpr::Named("nil".into()))),
                        "reduce" => return None,
                        _ => {}
                    }
                }
                // list<T> / dict / set / string .iter() → iter<T>. Other
                // combinator methods on list/dict/set/string keep their
                // existing eager typings (the runtime still materializes
                // them). Only the explicit .iter() bridge returns Iter.
                if method == "iter" {
                    match &obj_type {
                        Some(TypeExpr::List(inner)) => {
                            return Some(result(TypeExpr::Iter(Box::new((**inner).clone()))));
                        }
                        Some(TypeExpr::Generator(inner)) | Some(TypeExpr::Stream(inner)) => {
                            return Some(result(TypeExpr::Iter(Box::new((**inner).clone()))));
                        }
                        Some(TypeExpr::DictType(k, v)) => {
                            return Some(result(TypeExpr::Iter(Box::new(TypeExpr::Applied {
                                name: "Pair".into(),
                                args: vec![(**k).clone(), (**v).clone()],
                            }))));
                        }
                        Some(TypeExpr::Named(n))
                            if n == "list" || n == "dict" || n == "set" || n == "string" =>
                        {
                            return Some(result(TypeExpr::Named("iter".into())));
                        }
                        _ => {}
                    }
                }
                let is_dict = matches!(&obj_type, Some(TypeExpr::Named(n)) if n == "dict")
                    || matches!(&obj_type, Some(TypeExpr::DictType(..)))
                    || matches!(&obj_type, Some(TypeExpr::Shape(_)));
                match method.as_str() {
                    // Shared: bool-returning methods
                    "contains" | "starts_with" | "ends_with" | "empty" | "has" | "any" | "all" => {
                        Some(result(TypeExpr::Named("bool".into())))
                    }
                    // Shared: int-returning methods
                    "count" | "index_of" => Some(result(TypeExpr::Named("int".into()))),
                    // String methods
                    "trim" | "lowercase" | "uppercase" | "reverse" | "replace" | "substring"
                    | "pad_left" | "pad_right" | "repeat" | "join" => {
                        Some(result(TypeExpr::Named("string".into())))
                    }
                    "split" | "chars" => Some(result(TypeExpr::Named("list".into()))),
                    // filter returns dict for dicts, list for lists
                    "filter" => {
                        if is_dict {
                            Some(result(TypeExpr::Named("dict".into())))
                        } else {
                            Some(result(TypeExpr::Named("list".into())))
                        }
                    }
                    // List methods
                    "map" | "flat_map" | "sort" => Some(result(TypeExpr::Named("list".into()))),
                    "window" | "each_cons" | "sliding_window" => match &obj_type {
                        Some(TypeExpr::List(inner)) => Some(result(TypeExpr::List(Box::new(
                            TypeExpr::List(Box::new((**inner).clone())),
                        )))),
                        _ => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "reduce" | "find" | "first" | "last" => None,
                    // Dict methods
                    "keys" | "values" | "entries" => Some(result(TypeExpr::Named("list".into()))),
                    "merge" | "map_values" | "rekey" | "map_keys" => {
                        // Rekey/map_keys transform keys; resulting dict still keys-by-string.
                        // Preserve the value-type parameter when known so downstream code can
                        // still rely on dict<string, V> typing after a key-rename.
                        if let Some(TypeExpr::DictType(_, v)) = &obj_type {
                            Some(result(TypeExpr::DictType(
                                Box::new(TypeExpr::Named("string".into())),
                                v.clone(),
                            )))
                        } else {
                            Some(result(TypeExpr::Named("dict".into())))
                        }
                    }
                    // Conversions
                    "to_string" => Some(result(TypeExpr::Named("string".into()))),
                    "to_int" => Some(result(TypeExpr::Named("int".into()))),
                    "to_float" => Some(result(TypeExpr::Named("float".into()))),
                    _ => None,
                }
            }

            // TryOperator on Result<T, E> produces T
            Node::TryOperator { operand } => match self.infer_type(operand, scope) {
                Some(TypeExpr::Applied { name, args }) if name == "Result" && args.len() == 2 => {
                    Some(args[0].clone())
                }
                Some(TypeExpr::Named(name)) if name == "Result" => None,
                _ => None,
            },

            // Exit expressions produce the bottom type.
            Node::ThrowStmt { .. }
            | Node::ReturnStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt => Some(TypeExpr::Never),

            // If/else as expression: merge branch types. An `if` with no
            // `else` falls through to `nil` on the falsy path, so the
            // expression type is `T | nil` (not just `T`) — otherwise
            // `let x: int = if cond { 1 }` would type-check but run-time
            // produce `nil` and crash on the first `int` use.
            Node::IfElse {
                then_body,
                else_body,
                ..
            } => {
                let then_type = self.infer_block_type(then_body, scope);
                match else_body {
                    Some(eb) => {
                        let else_type = self.infer_block_type(eb, scope);
                        match (then_type, else_type) {
                            (Some(TypeExpr::Never), Some(TypeExpr::Never)) => Some(TypeExpr::Never),
                            (Some(TypeExpr::Never), Some(other))
                            | (Some(other), Some(TypeExpr::Never)) => Some(other),
                            (Some(t), Some(e)) if t == e => Some(t),
                            (Some(t), Some(e)) => Some(simplify_union(vec![t, e])),
                            (Some(t), None) => Some(t),
                            (None, _) => None,
                        }
                    }
                    None => match then_type {
                        Some(TypeExpr::Never) => Some(TypeExpr::Named("nil".into())),
                        Some(t) => Some(simplify_union(vec![t, TypeExpr::Named("nil".into())])),
                        None => None,
                    },
                }
            }

            Node::TryExpr { body } => {
                let ok_type = self
                    .infer_block_type(body, scope)
                    .unwrap_or_else(Self::wildcard_type);
                let inferred_err_type = self.infer_try_error_type(body, scope);
                if let TypeExpr::Applied { name, args } = &ok_type {
                    if name == "Result" && args.len() == 2 {
                        let err_type = inferred_err_type
                            .map(|thrown| simplify_union(vec![args[1].clone(), thrown]))
                            .unwrap_or_else(|| args[1].clone());
                        return Some(TypeExpr::Applied {
                            name: "Result".into(),
                            args: vec![args[0].clone(), err_type],
                        });
                    }
                }
                let err_type = inferred_err_type.unwrap_or_else(Self::wildcard_type);
                Some(TypeExpr::Applied {
                    name: "Result".into(),
                    args: vec![ok_type, err_type],
                })
            }

            Node::MatchExpr { value, arms } => self.infer_match_expr_type(value, arms, scope),

            Node::Parallel { mode, body, .. } => {
                let item_type = self
                    .infer_block_type(body, scope)
                    .unwrap_or_else(Self::wildcard_type);
                match mode {
                    ParallelMode::Count | ParallelMode::Each => {
                        Some(TypeExpr::List(Box::new(item_type)))
                    }
                    ParallelMode::EachStream => Some(TypeExpr::Stream(Box::new(item_type))),
                    ParallelMode::Settle => Some(TypeExpr::Named("dict".into())),
                }
            }

            // `try* EXPR` evaluates to EXPR's value on success; rethrow on
            // error never returns. Type is therefore EXPR's inferred type.
            Node::TryStar { operand } => self.infer_type(operand, scope),

            Node::CostRoute { body, .. } => self.infer_block_type(body, scope),

            Node::StructConstruct {
                struct_name,
                fields,
            } => scope
                .get_struct(struct_name)
                .map(|struct_info| self.infer_struct_type(struct_name, struct_info, fields, scope)),

            _ => None,
        }
    }

    fn infer_property_access_type(
        &self,
        object: &SNode,
        property: &str,
        scope: &TypeScope,
        optional: bool,
    ) -> InferredType {
        if !optional {
            // EnumName.Variant -> infer as the enum type.
            if let Node::Identifier(name) = &object.node {
                if let Some(enum_info) = scope.get_enum(name) {
                    return Some(self.infer_enum_type(name, enum_info, property, &[], scope));
                }
            }
        }
        let obj_type = self.infer_type(object, scope)?;
        self.infer_property_type_from_type(&obj_type, property, scope, optional)
    }

    fn infer_property_type_from_type(
        &self,
        ty: &TypeExpr,
        property: &str,
        scope: &TypeScope,
        optional: bool,
    ) -> InferredType {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) if name == "nil" => {
                optional.then(|| TypeExpr::Named("nil".into()))
            }
            TypeExpr::Named(name)
                if matches!(name.as_str(), "any" | "unknown" | "_")
                    || scope.is_generic_type_param(name) =>
            {
                None
            }
            TypeExpr::Named(name) if name == "list" => {
                Self::list_property_type(None, property, optional)
            }
            TypeExpr::Named(name) if name == "string" => {
                Self::string_property_type(property, optional)
            }
            TypeExpr::Named(name) if name == "dict" => None,
            TypeExpr::Named(name) if name == "Harness" => match property {
                "stdio" => Some(TypeExpr::Named("HarnessStdio".into())),
                "clock" => Some(TypeExpr::Named("HarnessClock".into())),
                "fs" => Some(TypeExpr::Named("HarnessFs".into())),
                "env" => Some(TypeExpr::Named("HarnessEnv".into())),
                "random" => Some(TypeExpr::Named("HarnessRandom".into())),
                "net" => Some(TypeExpr::Named("HarnessNet".into())),
                "system" => Some(TypeExpr::Named("HarnessSystem".into())),
                _ if optional => Some(TypeExpr::Named("nil".into())),
                _ => None,
            },
            TypeExpr::Named(name) if scope.get_struct(name).is_some() => {
                self.struct_property_type(name, &[], property, scope, optional)
            }
            TypeExpr::Named(name) if scope.get_enum(name).is_some() => {
                Self::enum_property_type(property, optional)
            }
            TypeExpr::Union(members) => {
                let mut inferred = Vec::new();
                for member in members {
                    if self.type_is_nil(member, scope) {
                        if optional {
                            inferred.push(TypeExpr::Named("nil".into()));
                        } else {
                            return None;
                        }
                    } else if let Some(member_type) =
                        self.infer_property_type_from_type(member, property, scope, optional)
                    {
                        inferred.push(member_type);
                    } else {
                        return None;
                    }
                }
                (!inferred.is_empty()).then(|| simplify_union(inferred))
            }
            TypeExpr::Intersection(members) => {
                for member in members {
                    if let Some(member_type) =
                        self.infer_property_type_from_type(member, property, scope, optional)
                    {
                        return Some(member_type);
                    }
                }
                optional.then(|| TypeExpr::Named("nil".into()))
            }
            TypeExpr::Shape(fields) => Self::shape_property_type(fields, property, optional),
            TypeExpr::List(inner) => {
                Self::list_property_type(Some(inner.as_ref()), property, optional)
            }
            TypeExpr::DictType(_, value) => Some(*value.clone()),
            TypeExpr::Applied { name, args } if name == "Pair" && args.len() == 2 => match property
            {
                "first" => Some(args[0].clone()),
                "second" => Some(args[1].clone()),
                _ if optional => Some(TypeExpr::Named("nil".into())),
                _ => None,
            },
            TypeExpr::Applied { name, args } if scope.get_struct(name).is_some() => {
                self.struct_property_type(name, args, property, scope, optional)
            }
            TypeExpr::Applied { name, .. } if scope.get_enum(name).is_some() => {
                Self::enum_property_type(property, optional)
            }
            _ if optional => Some(TypeExpr::Named("nil".into())),
            _ => None,
        }
    }

    fn infer_subscript_access_type(
        &self,
        object: &SNode,
        index: &SNode,
        scope: &TypeScope,
        optional: bool,
    ) -> InferredType {
        let obj_type = self.infer_type(object, scope)?;
        self.infer_subscript_type_from_type(&obj_type, index, scope, optional)
    }

    fn infer_subscript_type_from_type(
        &self,
        ty: &TypeExpr,
        index: &SNode,
        scope: &TypeScope,
        optional: bool,
    ) -> InferredType {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) if name == "nil" => {
                optional.then(|| TypeExpr::Named("nil".into()))
            }
            TypeExpr::Named(name)
                if matches!(name.as_str(), "any" | "unknown" | "_")
                    || scope.is_generic_type_param(name) =>
            {
                None
            }
            TypeExpr::Union(members) => {
                let mut inferred = Vec::new();
                for member in members {
                    if self.type_is_nil(member, scope) {
                        if optional {
                            inferred.push(TypeExpr::Named("nil".into()));
                        } else {
                            return None;
                        }
                    } else if let Some(member_type) =
                        self.infer_subscript_type_from_type(member, index, scope, optional)
                    {
                        inferred.push(member_type);
                    } else {
                        return None;
                    }
                }
                (!inferred.is_empty()).then(|| simplify_union(inferred))
            }
            TypeExpr::List(inner) => Some(*inner.clone()),
            TypeExpr::DictType(_, value) => Some(*value.clone()),
            TypeExpr::Shape(fields) => {
                if let Node::StringLiteral(key) = &index.node {
                    Self::shape_property_type(fields, key, false)
                } else {
                    None
                }
            }
            TypeExpr::Named(name) if name == "string" => Some(TypeExpr::Named("string".into())),
            _ => None,
        }
    }

    fn shape_property_type(
        fields: &[ShapeField],
        property: &str,
        optional_access: bool,
    ) -> InferredType {
        let Some(field) = fields.iter().find(|field| field.name == property) else {
            return optional_access.then(|| TypeExpr::Named("nil".into()));
        };
        Some(if field.optional {
            simplify_union(vec![field.type_expr.clone(), TypeExpr::Named("nil".into())])
        } else {
            field.type_expr.clone()
        })
    }

    fn list_property_type(
        item_type: Option<&TypeExpr>,
        property: &str,
        optional_access: bool,
    ) -> InferredType {
        match property {
            "count" => Some(TypeExpr::Named("int".into())),
            "empty" => Some(TypeExpr::Named("bool".into())),
            "first" | "last" => item_type
                .map(|inner| simplify_union(vec![inner.clone(), TypeExpr::Named("nil".into())])),
            _ if optional_access => Some(TypeExpr::Named("nil".into())),
            _ => None,
        }
    }

    fn string_property_type(property: &str, optional_access: bool) -> InferredType {
        match property {
            "count" => Some(TypeExpr::Named("int".into())),
            "empty" => Some(TypeExpr::Named("bool".into())),
            _ if optional_access => Some(TypeExpr::Named("nil".into())),
            _ => None,
        }
    }

    fn enum_property_type(property: &str, optional_access: bool) -> InferredType {
        match property {
            "variant" => Some(TypeExpr::Named("string".into())),
            "fields" => Some(TypeExpr::Named("list".into())),
            _ if optional_access => Some(TypeExpr::Named("nil".into())),
            _ => None,
        }
    }

    fn struct_property_type(
        &self,
        name: &str,
        args: &[TypeExpr],
        property: &str,
        scope: &TypeScope,
        optional_access: bool,
    ) -> InferredType {
        let struct_info = scope.get_struct(name)?;
        let Some(field) = struct_info
            .fields
            .iter()
            .find(|field| field.name == property)
        else {
            return optional_access.then(|| TypeExpr::Named("nil".into()));
        };
        let mut field_type = field.type_expr.clone()?;
        if struct_info.type_params.len() == args.len() {
            let bindings = struct_info
                .type_params
                .iter()
                .map(|param| param.name.clone())
                .zip(args.iter().cloned())
                .collect::<BTreeMap<_, _>>();
            field_type = Self::apply_type_bindings(&field_type, &bindings);
        }
        Some(if field.optional {
            simplify_union(vec![field_type, TypeExpr::Named("nil".into())])
        } else {
            field_type
        })
    }

    fn builtin_preserves_first_arg_type(name: &str) -> bool {
        matches!(
            name,
            "add_assistant" | "add_message" | "add_system" | "add_tool_result" | "add_user"
        )
    }

    pub(in crate::typechecker) fn check_unnecessary_safe_property_access(
        &mut self,
        snode: &SNode,
        object: &SNode,
        property: &str,
        scope: &TypeScope,
    ) {
        let Some(receiver_type) = self.infer_type(object, scope) else {
            return;
        };
        if !self.type_is_provably_non_nil(&receiver_type, scope) {
            return;
        }
        if !self.regular_property_access_is_safe(&receiver_type, property, scope) {
            return;
        }
        self.emit_unnecessary_safe_navigation(
            snode,
            object,
            SafeNavigationKind::Property(property),
        );
    }

    pub(in crate::typechecker) fn check_unnecessary_safe_method_call(
        &mut self,
        snode: &SNode,
        object: &SNode,
        scope: &TypeScope,
    ) {
        let Some(receiver_type) = self.infer_type(object, scope) else {
            return;
        };
        if self.type_is_provably_non_nil(&receiver_type, scope) {
            self.emit_unnecessary_safe_navigation(snode, object, SafeNavigationKind::Method);
        }
    }

    pub(in crate::typechecker) fn check_unnecessary_safe_subscript_access(
        &mut self,
        snode: &SNode,
        object: &SNode,
        scope: &TypeScope,
    ) {
        let Some(receiver_type) = self.infer_type(object, scope) else {
            return;
        };
        if self.type_is_provably_non_nil(&receiver_type, scope) {
            self.emit_unnecessary_safe_navigation(snode, object, SafeNavigationKind::Subscript);
        }
    }

    fn emit_unnecessary_safe_navigation(
        &mut self,
        snode: &SNode,
        object: &SNode,
        kind: SafeNavigationKind<'_>,
    ) {
        let Some(fix) = self.safe_navigation_fix(snode.span, object, &kind) else {
            return;
        };
        let span = fix[0].span;
        let access = match kind {
            SafeNavigationKind::Property(property) => format!("`?.{property}`"),
            SafeNavigationKind::Method => "safe method call".to_string(),
            SafeNavigationKind::Subscript => "`?[]`".to_string(),
        };
        self.lint_warning_at_with_fix(
            Code::LintUnnecessarySafeNavigation,
            UNNECESSARY_SAFE_NAVIGATION_RULE,
            format!("{access} is unnecessary because the receiver cannot be nil"),
            span,
            "use ordinary access on non-optional receivers".to_string(),
            fix,
        );
    }

    fn safe_navigation_fix(
        &self,
        span: Span,
        object: &SNode,
        kind: &SafeNavigationKind<'_>,
    ) -> Option<Vec<FixEdit>> {
        let source = self.source.as_deref()?;
        let search_start = object.span.end.min(span.end);
        let search_end = span.end.min(source.len());
        let region = source.get(search_start..search_end)?;
        let (relative_start, len, replacement) = match kind {
            SafeNavigationKind::Property(_) | SafeNavigationKind::Method => {
                (region.find("?.")?, 2, ".")
            }
            SafeNavigationKind::Subscript => (region.find("?[")?, 1, ""),
        };
        let start = search_start + relative_start;
        let end = start + len;
        Some(vec![FixEdit {
            span: Self::source_span_for_offsets(source, start, end),
            replacement: replacement.to_string(),
        }])
    }

    fn source_span_for_offsets(source: &str, start: usize, end: usize) -> Span {
        let prefix = &source[..start.min(source.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map(|offset| offset + 1).unwrap_or(0);
        Span::with_offsets(start, end, line, start - line_start + 1)
    }

    fn optional_method_result_type(ty: TypeExpr, include_nil: bool) -> TypeExpr {
        if include_nil {
            simplify_union(vec![ty, TypeExpr::Named("nil".into())])
        } else {
            ty
        }
    }

    pub(in crate::typechecker) fn type_is_nil(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        matches!(self.resolve_alias(ty, scope), TypeExpr::Named(name) if name == "nil")
    }

    fn type_may_include_nil(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) if name == "nil" => true,
            TypeExpr::Union(members) => members
                .iter()
                .any(|member| self.type_may_include_nil(member, scope)),
            _ => false,
        }
    }

    fn type_is_provably_non_nil(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) if matches!(name.as_str(), "nil" | "any" | "unknown" | "_") => {
                false
            }
            TypeExpr::Named(name) if scope.is_generic_type_param(name) => false,
            TypeExpr::Union(members) => members
                .iter()
                .all(|member| self.type_is_provably_non_nil(member, scope)),
            TypeExpr::Never => false,
            _ => true,
        }
    }

    fn regular_property_access_is_safe(
        &self,
        ty: &TypeExpr,
        property: &str,
        scope: &TypeScope,
    ) -> bool {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Union(members) => members
                .iter()
                .all(|member| self.regular_property_access_is_safe(member, property, scope)),
            TypeExpr::Intersection(members) => members
                .iter()
                .any(|member| self.regular_property_access_is_safe(member, property, scope)),
            TypeExpr::Shape(fields) => fields.iter().any(|field| field.name == property),
            TypeExpr::DictType(_, _) => false,
            TypeExpr::List(inner) => {
                Self::list_property_type(Some(inner), property, false).is_some()
            }
            TypeExpr::Named(name) if name == "list" => {
                Self::list_property_type(None, property, false).is_some()
            }
            TypeExpr::Named(name) if name == "dict" => false,
            TypeExpr::Named(name) if name == "string" => {
                Self::string_property_type(property, false).is_some()
            }
            TypeExpr::Named(name) if name == "Pair" => matches!(property, "first" | "second"),
            TypeExpr::Named(name) => {
                scope
                    .get_struct(name)
                    .is_some_and(|info| info.fields.iter().any(|field| field.name == property))
                    || (scope.get_enum(name).is_some()
                        && Self::enum_property_type(property, false).is_some())
            }
            TypeExpr::Applied { name, .. } if name == "Pair" => {
                matches!(property, "first" | "second")
            }
            TypeExpr::Applied { name, .. } => {
                scope
                    .get_struct(name)
                    .is_some_and(|info| info.fields.iter().any(|field| field.name == property))
                    || (scope.get_enum(name).is_some()
                        && Self::enum_property_type(property, false).is_some())
            }
            _ => false,
        }
    }

    fn infer_pipe_type(&self, left: &SNode, right: &SNode, scope: &TypeScope) -> InferredType {
        let left_type = self.infer_type(left, scope);

        if Self::contains_pipe_placeholder(right) {
            let mut pipe_scope = scope.child();
            pipe_scope.vars.insert("_".into(), left_type);
            return self.infer_type(right, &pipe_scope);
        }

        match &right.node {
            Node::Closure { params, body, .. } => {
                let mut closure_scope = scope.child();
                for (idx, param) in params.iter().enumerate() {
                    let ty = if idx == 0 {
                        param.type_expr.clone().or_else(|| left_type.clone())
                    } else {
                        param.type_expr.clone()
                    };
                    closure_scope.define_var(&param.name, ty);
                }
                self.infer_block_type(body, &closure_scope)
            }
            Node::Identifier(name) => {
                if let Some(sig) = scope.get_fn(name).cloned() {
                    return sig.return_type;
                }
                builtin_return_type(name)
            }
            _ => match self.infer_type(right, scope) {
                Some(TypeExpr::FnType { return_type, .. }) => Some(*return_type),
                _ => None,
            },
        }
    }

    fn contains_pipe_placeholder(node: &SNode) -> bool {
        match &node.node {
            Node::Identifier(name) if name == "_" => true,
            Node::FunctionCall { args, .. } => args.iter().any(Self::contains_pipe_placeholder),
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                Self::contains_pipe_placeholder(object)
                    || args.iter().any(Self::contains_pipe_placeholder)
            }
            Node::HitlExpr { args, .. } => args
                .iter()
                .any(|arg| Self::contains_pipe_placeholder(&arg.value)),
            Node::BinaryOp { left, right, .. } => {
                Self::contains_pipe_placeholder(left) || Self::contains_pipe_placeholder(right)
            }
            Node::UnaryOp { operand, .. } => Self::contains_pipe_placeholder(operand),
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                Self::contains_pipe_placeholder(condition)
                    || Self::contains_pipe_placeholder(true_expr)
                    || Self::contains_pipe_placeholder(false_expr)
            }
            Node::Assignment { target, value, .. } => {
                Self::contains_pipe_placeholder(target) || Self::contains_pipe_placeholder(value)
            }
            Node::RangeExpr { start, end, .. } => {
                Self::contains_pipe_placeholder(start) || Self::contains_pipe_placeholder(end)
            }
            Node::ListLiteral(items) => items.iter().any(Self::contains_pipe_placeholder),
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => entries.iter().any(|entry| {
                Self::contains_pipe_placeholder(&entry.key)
                    || Self::contains_pipe_placeholder(&entry.value)
            }),
            Node::EnumConstruct { args, .. } => args.iter().any(Self::contains_pipe_placeholder),
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                Self::contains_pipe_placeholder(object)
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                Self::contains_pipe_placeholder(object) || Self::contains_pipe_placeholder(index)
            }
            Node::SliceAccess { object, start, end } => {
                Self::contains_pipe_placeholder(object)
                    || start
                        .as_ref()
                        .is_some_and(|start| Self::contains_pipe_placeholder(start))
                    || end
                        .as_ref()
                        .is_some_and(|end| Self::contains_pipe_placeholder(end))
            }
            Node::Spread(inner)
            | Node::TryOperator { operand: inner }
            | Node::TryStar { operand: inner } => Self::contains_pipe_placeholder(inner),
            _ => false,
        }
    }

    /// Infer the type of a block (last expression, or `never` if the block definitely exits).
    pub(in crate::typechecker) fn infer_block_type(
        &self,
        stmts: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        if Self::block_definitely_exits(stmts) {
            return Some(TypeExpr::Never);
        }
        stmts.last().and_then(|s| self.infer_type(s, scope))
    }
}
