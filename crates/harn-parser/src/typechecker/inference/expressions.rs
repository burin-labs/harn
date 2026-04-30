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

use super::super::binary_ops::infer_binary_op_type;
use super::super::scope::{builtin_return_type, InferredType, TypeScope};
use super::super::union::simplify_union;
use super::super::TypeChecker;

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
                        .unwrap_or(TypeExpr::Named("nil".into()));
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
                    // Try to infer return type from last expression in body
                    let ret = body.last().and_then(|last| self.infer_type(last, scope));
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
                        return Some(Self::apply_type_bindings(&ty, &bindings));
                    }
                    return None;
                }
                // Generic builtins (llm_call, schema_parse/check/expect):
                // bind T by walking each arg node against the param
                // TypeExpr, then apply bindings to the declared return
                // type. Falls through to `builtin_return_type` when no T
                // could be bound (e.g. llm_call without an output_schema
                // option), preserving the historical `dict` return.
                if let Some(sig) = builtin_signatures::lookup_generic_builtin_sig(name) {
                    let type_param_set: std::collections::BTreeSet<String> =
                        sig.type_params.iter().cloned().collect();
                    let mut bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
                    if type_args.len() == sig.type_params.len() {
                        for (param_name, type_arg) in sig.type_params.iter().zip(type_args) {
                            bindings.insert(param_name.clone(), type_arg.clone());
                        }
                    }
                    for (arg, param_ty) in args.iter().zip(sig.params.iter()) {
                        let _ = self.bind_from_arg_node(
                            param_ty,
                            arg,
                            &type_param_set,
                            &mut bindings,
                            scope,
                        );
                    }
                    let all_bound = sig.type_params.iter().all(|tp| bindings.contains_key(tp));
                    if all_bound {
                        return Some(Self::apply_type_bindings(&sig.return_type, &bindings));
                    }
                }
                // Check builtin return types
                builtin_return_type(name)
            }

            Node::BinaryOp { op, left, right } => {
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
                // EnumName.Variant → infer as the enum type
                if let Node::Identifier(name) = &object.node {
                    if let Some(enum_info) = scope.get_enum(name) {
                        return Some(self.infer_enum_type(name, enum_info, property, &[], scope));
                    }
                }
                // .variant on an enum value → string
                if property == "variant" {
                    let obj_type = self.infer_type(object, scope);
                    if let Some(name) = obj_type.as_ref().and_then(Self::base_type_name) {
                        if scope.get_enum(name).is_some() {
                            return Some(TypeExpr::Named("string".into()));
                        }
                    }
                }
                // Shape field access: obj.field → field type
                let obj_type = self.infer_type(object, scope);
                // Pair<K, V> has `.first` and `.second` accessors.
                if let Some(TypeExpr::Applied { name, args }) = &obj_type {
                    if name == "Pair" && args.len() == 2 {
                        if property == "first" {
                            return Some(args[0].clone());
                        } else if property == "second" {
                            return Some(args[1].clone());
                        }
                    }
                }
                if let Some(TypeExpr::Shape(fields)) = &obj_type {
                    if let Some(field) = fields.iter().find(|f| f.name == *property) {
                        return Some(field.type_expr.clone());
                    }
                }
                None
            }

            Node::SubscriptAccess { object, index } => {
                let obj_type = self.infer_type(object, scope);
                match &obj_type {
                    Some(TypeExpr::List(inner)) => Some(*inner.clone()),
                    Some(TypeExpr::DictType(_, v)) => Some(*v.clone()),
                    Some(TypeExpr::Shape(fields)) => {
                        // If index is a string literal, look up the field type
                        if let Node::StringLiteral(key) = &index.node {
                            fields
                                .iter()
                                .find(|f| &f.name == key)
                                .map(|f| f.type_expr.clone())
                        } else {
                            None
                        }
                    }
                    Some(TypeExpr::Named(n)) if n == "list" => None,
                    Some(TypeExpr::Named(n)) if n == "dict" => None,
                    Some(TypeExpr::Named(n)) if n == "string" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    _ => None,
                }
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
                        "iter" => return Some(iter_of(t)),
                        "map" | "flat_map" => {
                            // Closure-return inference is not threaded here;
                            // fall back to a coarse `iter<any>` — matches the
                            // list-return style the rest of the checker uses.
                            return Some(TypeExpr::Named("iter".into()));
                        }
                        "filter" | "take" | "skip" | "take_while" | "skip_while" => {
                            return Some(iter_of(t));
                        }
                        "zip" => {
                            return Some(iter_of(pair(t, TypeExpr::Named("any".into()))));
                        }
                        "enumerate" => {
                            return Some(iter_of(pair(TypeExpr::Named("int".into()), t)));
                        }
                        "chain" => return Some(iter_of(t)),
                        "chunks" | "windows" => {
                            return Some(iter_of(TypeExpr::List(Box::new(t))));
                        }
                        // Sinks
                        "to_list" => return Some(TypeExpr::List(Box::new(t))),
                        "to_set" => {
                            return Some(TypeExpr::Applied {
                                name: "set".into(),
                                args: vec![t],
                            })
                        }
                        "to_dict" => return Some(TypeExpr::Named("dict".into())),
                        "count" => return Some(TypeExpr::Named("int".into())),
                        "sum" => {
                            return Some(TypeExpr::Union(vec![
                                TypeExpr::Named("int".into()),
                                TypeExpr::Named("float".into()),
                            ]))
                        }
                        "min" | "max" | "first" | "last" | "find" => {
                            return Some(TypeExpr::Union(vec![t, TypeExpr::Named("nil".into())]));
                        }
                        "any" | "all" => return Some(TypeExpr::Named("bool".into())),
                        "for_each" => return Some(TypeExpr::Named("nil".into())),
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
                            return Some(TypeExpr::Iter(Box::new((**inner).clone())));
                        }
                        Some(TypeExpr::Generator(inner)) | Some(TypeExpr::Stream(inner)) => {
                            return Some(TypeExpr::Iter(Box::new((**inner).clone())));
                        }
                        Some(TypeExpr::DictType(k, v)) => {
                            return Some(TypeExpr::Iter(Box::new(TypeExpr::Applied {
                                name: "Pair".into(),
                                args: vec![(**k).clone(), (**v).clone()],
                            })));
                        }
                        Some(TypeExpr::Named(n))
                            if n == "list" || n == "dict" || n == "set" || n == "string" =>
                        {
                            return Some(TypeExpr::Named("iter".into()));
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
                        Some(TypeExpr::Named("bool".into()))
                    }
                    // Shared: int-returning methods
                    "count" | "index_of" => Some(TypeExpr::Named("int".into())),
                    // String methods
                    "trim" | "lowercase" | "uppercase" | "reverse" | "replace" | "substring"
                    | "pad_left" | "pad_right" | "repeat" | "join" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    "split" | "chars" => Some(TypeExpr::Named("list".into())),
                    // filter returns dict for dicts, list for lists
                    "filter" => {
                        if is_dict {
                            Some(TypeExpr::Named("dict".into()))
                        } else {
                            Some(TypeExpr::Named("list".into()))
                        }
                    }
                    // List methods
                    "map" | "flat_map" | "sort" => Some(TypeExpr::Named("list".into())),
                    "window" | "each_cons" | "sliding_window" => match &obj_type {
                        Some(TypeExpr::List(inner)) => Some(TypeExpr::List(Box::new(
                            TypeExpr::List(Box::new((**inner).clone())),
                        ))),
                        _ => Some(TypeExpr::Named("list".into())),
                    },
                    "reduce" | "find" | "first" | "last" => None,
                    // Dict methods
                    "keys" | "values" | "entries" => Some(TypeExpr::Named("list".into())),
                    "merge" | "map_values" | "rekey" | "map_keys" => {
                        // Rekey/map_keys transform keys; resulting dict still keys-by-string.
                        // Preserve the value-type parameter when known so downstream code can
                        // still rely on dict<string, V> typing after a key-rename.
                        if let Some(TypeExpr::DictType(_, v)) = &obj_type {
                            Some(TypeExpr::DictType(
                                Box::new(TypeExpr::Named("string".into())),
                                v.clone(),
                            ))
                        } else {
                            Some(TypeExpr::Named("dict".into()))
                        }
                    }
                    // Conversions
                    "to_string" => Some(TypeExpr::Named("string".into())),
                    "to_int" => Some(TypeExpr::Named("int".into())),
                    "to_float" => Some(TypeExpr::Named("float".into())),
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

            // If/else as expression: merge branch types.
            Node::IfElse {
                then_body,
                else_body,
                ..
            } => {
                let then_type = self.infer_block_type(then_body, scope);
                let else_type = else_body
                    .as_ref()
                    .and_then(|eb| self.infer_block_type(eb, scope));
                match (then_type, else_type) {
                    (Some(TypeExpr::Never), Some(TypeExpr::Never)) => Some(TypeExpr::Never),
                    (Some(TypeExpr::Never), Some(other)) | (Some(other), Some(TypeExpr::Never)) => {
                        Some(other)
                    }
                    (Some(t), Some(e)) if t == e => Some(t),
                    (Some(t), Some(e)) => Some(simplify_union(vec![t, e])),
                    (Some(t), None) => Some(t),
                    (None, _) => None,
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

            // `try* EXPR` evaluates to EXPR's value on success; rethrow on
            // error never returns. Type is therefore EXPR's inferred type.
            Node::TryStar { operand } => self.infer_type(operand, scope),

            Node::StructConstruct {
                struct_name,
                fields,
            } => scope
                .get_struct(struct_name)
                .map(|struct_info| self.infer_struct_type(struct_name, struct_info, fields, scope)),

            _ => None,
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
