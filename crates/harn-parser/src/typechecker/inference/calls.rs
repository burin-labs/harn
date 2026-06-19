//! Call-site checking, generic type-parameter binding, struct/enum
//! constructors, and the standalone deprecation visitor.
//!
//! `check_call` is the diagnostic-emitting per-call-site walk: it enforces
//! arity, argument types, generic binding, where-clause interface bounds,
//! cross-module resolvability, deprecation, and the
//! `unreachable() / never-returning` exhaustiveness contract. The
//! generic-binding helpers (`bind_type_param`, `extract_type_bindings`,
//! `bind_from_arg_node`, `apply_type_bindings`) are also used by
//! `subtyping::interface_mismatch_reason` and the inferred struct/enum
//! literal types in `expressions.rs`. `visit_for_deprecation` runs once
//! across the program to catch deprecated calls that hide inside
//! expression contexts where `check_node` would only trigger `infer_type`.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;
use crate::builtin_signatures;
use crate::builtin_signatures::TyExt;
use crate::diagnostic_codes::Code;
use harn_lexer::Span;

use super::super::format::format_type;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{is_builtin, EnumDeclInfo, FnSignature, StructDeclInfo, TypeScope};
use super::super::union::collapse_members_opt;
use super::super::union::without_nil;
use super::super::TypeChecker;

impl TypeChecker {
    fn builtin_param_for_arg(
        sig: &builtin_signatures::BuiltinSignature,
        index: usize,
    ) -> Option<&builtin_signatures::Param> {
        if sig.has_rest && index >= sig.params.len().saturating_sub(1) {
            sig.params.last()
        } else {
            sig.params.get(index)
        }
    }

    fn function_param_for_arg(
        sig: &FnSignature,
        index: usize,
    ) -> Option<(&str, Option<&TypeExpr>)> {
        let entry = if sig.has_rest && index >= sig.params.len().saturating_sub(1) {
            sig.params.last()
        } else {
            sig.params.get(index)
        }?;
        Some((entry.0.as_str(), entry.1.as_ref()))
    }

    /// Collapse the field types of a shape into a single value type. Used when
    /// binding `dict<string, V>` against a heterogeneous shape literal — V is
    /// the union of every field type, simplified so a homogeneous shape stays
    /// a single named type instead of a one-element union.
    fn union_of_shape_field_types(fields: &[ShapeField]) -> Option<TypeExpr> {
        let mut members: Vec<TypeExpr> = Vec::new();
        for field in fields {
            if !members.contains(&field.type_expr) {
                members.push(field.type_expr.clone());
            }
        }
        collapse_members_opt(members, TypeExpr::Union)
    }

    fn builtin_uses_strict_llm_option_keys(name: &str, param_name: &str) -> bool {
        param_name == "options"
            && matches!(
                name,
                "llm_call"
                    | "llm_call_safe"
                    | "llm_stream_call"
                    | "llm_call_structured"
                    | "llm_call_structured_safe"
                    | "llm_call_structured_result"
                    | "llm_completion"
            )
    }

    fn check_strict_llm_option_keys(
        &mut self,
        builtin_name: &str,
        param_name: &str,
        expected: &TypeExpr,
        arg: &SNode,
    ) {
        if !Self::builtin_uses_strict_llm_option_keys(builtin_name, param_name) {
            return;
        }
        let TypeExpr::Shape(fields) = expected else {
            return;
        };
        let Node::DictLiteral(entries) = &arg.node else {
            return;
        };
        let candidates: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
        for entry in entries {
            if matches!(entry.value.node, Node::Spread(_)) {
                continue;
            }
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key,
                _ => continue,
            };
            if fields.iter().any(|field| field.name == *key) {
                continue;
            }
            let message =
                match crate::diagnostic::find_closest_match(key, candidates.iter().copied(), 3) {
                    Some(suggestion) => {
                        format!(
                            "unknown `{builtin_name}` option `{key}`; did you mean `{suggestion}`?"
                        )
                    }
                    None => format!("unknown `{builtin_name}` option `{key}`"),
                };
            self.warning_at(Code::UnknownLlmOption, message, entry.key.span);
        }
    }

    fn option_bag_type_name(ty: &TypeExpr) -> Option<&str> {
        match ty {
            TypeExpr::Named(name) | TypeExpr::Applied { name, .. }
                if name.ends_with("Options") || name.ends_with("Config") =>
            {
                Some(name)
            }
            TypeExpr::Union(members) => members.iter().find_map(Self::option_bag_type_name),
            _ => None,
        }
    }

    fn is_option_bag_param_name(param_name: &str) -> bool {
        matches!(param_name, "opts" | "options" | "config")
            || param_name.ends_with("_opts")
            || param_name.ends_with("_options")
            || param_name.ends_with("_config")
    }

    fn option_bag_shape_fields(
        &self,
        param_name: &str,
        expected: &TypeExpr,
        scope: &TypeScope,
    ) -> Option<Vec<ShapeField>> {
        let option_bag_name = Self::option_bag_type_name(expected);
        if option_bag_name.is_none() && !Self::is_option_bag_param_name(param_name) {
            return None;
        }
        let resolved = self.resolve_alias(expected, scope);
        match resolved {
            TypeExpr::Shape(fields) => Some(fields),
            TypeExpr::Union(members) => {
                let mut shapes = members.into_iter().filter_map(|member| match member {
                    TypeExpr::Named(name) if name == "nil" => None,
                    TypeExpr::Shape(fields) => Some(fields),
                    _ => None,
                });
                let fields = shapes.next()?;
                if shapes.next().is_none() {
                    Some(fields)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn check_unknown_option_bag_fields(
        &mut self,
        context: impl Into<String>,
        param_name: &str,
        expected: &TypeExpr,
        arg: &SNode,
        scope: &TypeScope,
    ) {
        let Node::DictLiteral(entries) = &arg.node else {
            return;
        };
        let Some(fields) = self.option_bag_shape_fields(param_name, expected, scope) else {
            return;
        };
        let known: BTreeSet<&str> = fields.iter().map(|field| field.name.as_str()).collect();
        if known.is_empty() {
            return;
        }
        let context = context.into();
        let expected_list = known
            .iter()
            .map(|field| format!("`{field}`"))
            .collect::<Vec<_>>()
            .join(", ");
        for entry in entries {
            if matches!(entry.value.node, Node::Spread(_)) {
                continue;
            }
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key,
                _ => continue,
            };
            if known.contains(key.as_str()) {
                continue;
            }
            let mut message =
                format!("{context}: unknown option `{key}`; expected one of {expected_list}");
            if let Some(candidate) =
                crate::diagnostic::find_closest_match(key, known.iter().copied(), 3)
            {
                message.push_str(&format!(" — did you mean `{candidate}`?"));
            }
            self.error_at(Code::UnknownOption, message, entry.key.span);
        }
    }

    fn check_builtin_signature_call(
        &mut self,
        name: &str,
        sig: &builtin_signatures::BuiltinSignature,
        type_args: &[TypeExpr],
        args: &[SNode],
        has_spread: bool,
        scope: &mut TypeScope,
        span: Span,
    ) {
        if !type_args.is_empty() {
            if !sig.is_generic() {
                self.error_at(
                    Code::GenericTypeArgumentUnsupported,
                    format!("Builtin function '{name}' does not declare type parameters"),
                    span,
                );
            } else if type_args.len() != sig.type_params.len() {
                self.error_at(
                    Code::GenericTypeArgumentArity,
                    format!(
                        "Builtin function '{}' expects {} type arguments, got {}",
                        name,
                        sig.type_params.len(),
                        type_args.len()
                    ),
                    span,
                );
            }
        }

        if !has_spread {
            let required = sig.required_params();
            let total = sig.params.len();
            let arity_ok = if sig.has_rest {
                args.len() >= total.saturating_sub(1)
            } else {
                args.len() >= required && args.len() <= total
            };
            if !arity_ok {
                let (expected, single_arg) = if sig.has_rest {
                    (
                        format!("at least {}", total.saturating_sub(1)),
                        total.saturating_sub(1) == 1,
                    )
                } else if required == total {
                    (total.to_string(), total == 1)
                } else {
                    (format!("{required}-{total}"), false)
                };
                let arg_word = if single_arg { "argument" } else { "arguments" };
                self.warning_at(
                    Code::BuiltinArity,
                    format!(
                        "Builtin function '{}' expects {} {}, got {}",
                        name,
                        expected,
                        arg_word,
                        args.len()
                    ),
                    span,
                );
            }
        }

        let type_param_names = sig.type_param_names();
        let type_param_set: std::collections::BTreeSet<String> =
            type_param_names.iter().cloned().collect();
        let mut type_bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
        if type_args.len() == type_param_names.len() {
            for (param_name, type_arg) in type_param_names.iter().zip(type_args.iter()) {
                type_bindings.insert(param_name.clone(), type_arg.clone());
            }
        }

        for (i, arg) in args.iter().enumerate() {
            let Some(param) = Self::builtin_param_for_arg(sig, i) else {
                continue;
            };
            if param.ty.is_any() {
                continue;
            }
            let param_ty = param.ty.to_type_expr();
            if let Err(message) =
                self.bind_from_arg_node(&param_ty, arg, &type_param_set, &mut type_bindings, scope)
            {
                self.error_at(Code::ArgumentTypeMismatch, message, arg.span);
            }
        }

        let unbound_type_params: std::collections::BTreeSet<String> = type_param_set
            .iter()
            .filter(|name| !type_bindings.contains_key(*name))
            .cloned()
            .collect();
        let mut expected_args: Vec<Option<TypeExpr>> = Vec::with_capacity(args.len());
        let mut contextual_args = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let Some(param) = Self::builtin_param_for_arg(sig, i) else {
                self.check_node(arg, scope);
                expected_args.push(None);
                contextual_args.push(false);
                continue;
            };
            if param.ty.is_any() || matches!(param.ty, builtin_signatures::Ty::SchemaOf(_)) {
                self.check_node(arg, scope);
                expected_args.push(None);
                contextual_args.push(false);
                continue;
            }
            let expected = Self::apply_type_bindings(&param.ty.to_type_expr(), &type_bindings);
            let contextual_expected =
                (!Self::contains_type_param(&expected, &unbound_type_params)).then_some(&expected);
            let context_checked = self.check_node_with_expected(arg, contextual_expected, scope);
            expected_args.push(Some(expected));
            contextual_args.push(context_checked);
        }

        // `call_scope` only needs to differ from `scope` when the callee
        // declares its own generic type params (which must be visible while
        // we check this call's arguments). The common case — calling a
        // non-generic builtin — borrows `scope` directly, sparing a clone
        // per call. Otherwise we allocate a child that adds those names.
        let call_scope_owned;
        let call_scope: &TypeScope = if sig.type_params.is_empty() {
            scope
        } else {
            let mut s = scope.child();
            for tp_name in sig.type_params {
                s.generic_type_params.insert((*tp_name).to_string());
            }
            call_scope_owned = s;
            &call_scope_owned
        };

        for (i, arg) in args.iter().enumerate() {
            let Some(param) = Self::builtin_param_for_arg(sig, i) else {
                continue;
            };
            let Some(expected) = expected_args.get(i).and_then(|ty| ty.as_ref()) else {
                continue;
            };
            let actual = self.infer_type(arg, scope);
            if let Some(actual) = &actual {
                self.check_strict_llm_option_keys(name, param.name, expected, arg);
                if !Self::builtin_uses_strict_llm_option_keys(name, param.name) {
                    self.check_unknown_option_bag_fields(
                        format!("argument {} `{}`", i + 1, param.name),
                        param.name,
                        expected,
                        arg,
                        call_scope,
                    );
                }
                let compatible = contextual_args.get(i).copied().unwrap_or(false)
                    || self.types_compatible(expected, actual, call_scope)
                    || (param.optional
                        && without_nil(actual).is_none_or(|non_nil| {
                            self.types_compatible(expected, &non_nil, call_scope)
                        }));
                if !compatible {
                    self.type_mismatch_at(
                        Code::ArgumentTypeMismatch,
                        format!("argument {} `{}`", i + 1, param.name),
                        expected,
                        actual,
                        arg.span,
                        (None, Some(arg.span)),
                        call_scope,
                    );
                }
            }
        }

        if !sig.where_clauses.is_empty() {
            for (type_param, bound) in sig.where_clauses {
                if let Some(concrete_type) = type_bindings.get(*type_param) {
                    let concrete_name = format_type(concrete_type);
                    let Some(base_type_name) = Self::base_type_name(concrete_type) else {
                        self.error_at(Code::WhereConstraintMismatch,
                            format!(
                                "Type '{concrete_name}' does not satisfy interface '{bound}': only named types can satisfy interfaces (required by constraint `where {type_param}: {bound}`)"
                            ),
                            span,
                        );
                        continue;
                    };
                    if let Some(reason) = self.interface_mismatch_reason(
                        base_type_name,
                        bound,
                        &BTreeMap::new(),
                        scope,
                    ) {
                        self.error_at(
                            Code::WhereConstraintMismatch,
                            format!(
                                "Type '{concrete_name}' does not satisfy interface '{bound}': {reason} \
                                 (required by constraint `where {type_param}: {bound}`)"
                            ),
                            span,
                        );
                    }
                }
            }
        }
    }

    pub(in crate::typechecker) fn check_harness_method_call(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        scope: &mut TypeScope,
        span: Span,
    ) -> bool {
        let Some(raw_type) = self.infer_type(object, scope) else {
            return false;
        };
        let TypeExpr::Named(type_name) = self.resolve_alias(&raw_type, scope) else {
            return false;
        };
        let Some(sub_handle) = crate::harness_methods::harness_type_sub_handle(type_name.as_str())
        else {
            return false;
        };
        let Some(ambient) = crate::harness_methods::harness_sub_handle_ambient(sub_handle, method)
        else {
            return false;
        };
        let Some(sig) = builtin_signatures::lookup(ambient) else {
            return false;
        };
        let display_name = format!("harness.{sub_handle}.{method}");
        let has_spread = args.iter().any(|arg| matches!(&arg.node, Node::Spread(_)));
        self.check_builtin_signature_call(&display_name, sig, &[], args, has_spread, scope, span);
        true
    }

    pub(in crate::typechecker) fn bind_type_param(
        param_name: &str,
        concrete: &TypeExpr,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Result<(), String> {
        if Self::is_wildcard_type(concrete) {
            return Ok(());
        }
        if let Some(existing) = bindings.get(param_name) {
            if Self::is_wildcard_type(existing) {
                bindings.insert(param_name.to_string(), concrete.clone());
                return Ok(());
            }
            if existing != concrete {
                return Err(format!(
                    "type parameter '{}' was inferred as both {} and {}",
                    param_name,
                    format_type(existing),
                    format_type(concrete)
                ));
            }
            return Ok(());
        }
        bindings.insert(param_name.to_string(), concrete.clone());
        Ok(())
    }

    /// Recursively extract type parameter bindings from matching param/arg types.
    /// E.g., param_type=list<T> + arg_type=list<Dog> → binds T=Dog.
    pub(in crate::typechecker) fn extract_type_bindings(
        param_type: &TypeExpr,
        arg_type: &TypeExpr,
        type_params: &std::collections::BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Result<(), String> {
        match (param_type, arg_type) {
            (TypeExpr::Named(param_name), concrete) if type_params.contains(param_name) => {
                Self::bind_type_param(param_name, concrete, bindings)
            }
            (TypeExpr::List(p_inner), TypeExpr::List(a_inner)) => {
                Self::extract_type_bindings(p_inner, a_inner, type_params, bindings)
            }
            (TypeExpr::DictType(pk, pv), TypeExpr::DictType(ak, av)) => {
                Self::extract_type_bindings(pk, ak, type_params, bindings)?;
                Self::extract_type_bindings(pv, av, type_params, bindings)
            }
            // A shape literal `{a: 1, b: "x"}` flowing into a `dict<string, V>`
            // parameter is the most common stdlib call pattern — `pick_keys`,
            // `filter_nil`, `merge`, etc. all advertise a generic dict-shape
            // contract. Bind V to the union of field types so the projected
            // result keeps useful element typing instead of collapsing to
            // `dict`.
            (TypeExpr::DictType(pk, pv), TypeExpr::Shape(arg_fields))
            | (
                TypeExpr::DictType(pk, pv),
                TypeExpr::OpenShape {
                    fields: arg_fields, ..
                },
            ) => {
                if matches!(pk.as_ref(), TypeExpr::Named(name) if name == "string") {
                    let value_union = Self::union_of_shape_field_types(arg_fields)
                        .unwrap_or_else(|| TypeExpr::Named("nil".into()));
                    Self::extract_type_bindings(pv, &value_union, type_params, bindings)?;
                }
                Ok(())
            }
            (
                TypeExpr::Applied {
                    name: p_name,
                    args: p_args,
                },
                TypeExpr::Applied {
                    name: a_name,
                    args: a_args,
                },
            ) if p_name == a_name && p_args.len() == a_args.len() => {
                for (param, arg) in p_args.iter().zip(a_args.iter()) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Ok(())
            }
            (TypeExpr::Shape(param_fields), TypeExpr::Shape(arg_fields)) => {
                for param_field in param_fields {
                    if let Some(arg_field) = arg_fields
                        .iter()
                        .find(|field| field.name == param_field.name)
                    {
                        Self::extract_type_bindings(
                            &param_field.type_expr,
                            &arg_field.type_expr,
                            type_params,
                            bindings,
                        )?;
                    }
                }
                Ok(())
            }
            // Open record parameter `{f: T, ...R}` against an actual record:
            // bind the explicit fields field-by-field, then bind the single row
            // variable `R` to the actual's **leftover** fields (one-sided row
            // matching — the design's core operation; no HM unification). With
            // no explicit fields (`{...R}`, as in `merge`'s params) R simply
            // binds to the whole actual record. Multiple row variables can't be
            // split unambiguously, so they are left for the gradual fallback.
            (
                TypeExpr::OpenShape {
                    fields: pf,
                    rests: prests,
                },
                arg_type,
            ) => {
                let af: &[ShapeField] = match arg_type {
                    TypeExpr::Shape(af) => af,
                    TypeExpr::OpenShape { fields: af, .. } => af,
                    _ => return Ok(()),
                };
                for pfield in pf {
                    if let Some(afield) = af.iter().find(|f| f.name == pfield.name) {
                        Self::extract_type_bindings(
                            &pfield.type_expr,
                            &afield.type_expr,
                            type_params,
                            bindings,
                        )?;
                    }
                }
                let row_vars: Vec<&String> = prests
                    .iter()
                    .filter_map(|r| match r {
                        TypeExpr::Named(n) if type_params.contains(n) => Some(n),
                        _ => None,
                    })
                    .collect();
                if row_vars.len() == 1 {
                    let explicit: std::collections::BTreeSet<&str> =
                        pf.iter().map(|f| f.name.as_str()).collect();
                    let leftover: Vec<ShapeField> = af
                        .iter()
                        .filter(|f| !explicit.contains(f.name.as_str()))
                        .cloned()
                        .collect();
                    Self::bind_type_param(row_vars[0], &TypeExpr::Shape(leftover), bindings)?;
                }
                Ok(())
            }
            (
                TypeExpr::FnType {
                    params: p_params,
                    return_type: p_ret,
                },
                TypeExpr::FnType {
                    params: a_params,
                    return_type: a_ret,
                },
            ) => {
                for (param, arg) in p_params.iter().zip(a_params.iter()) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Self::extract_type_bindings(p_ret, a_ret, type_params, bindings)
            }
            _ => Ok(()),
        }
    }

    /// Bind type parameters by walking a param [`TypeExpr`] against an
    /// argument AST node. Used by the generic-builtin dispatch path for
    /// `llm_call`, `schema_parse`, etc.
    ///
    /// Unlike [`extract_type_bindings`], which matches a param type against
    /// an inferred arg *type*, this walks the arg *node* so that
    /// `Schema<T>` in a param position can pull `T` from the structural
    /// value of the corresponding argument (e.g. a type alias identifier
    /// or an inline JSON-Schema dict literal). When the param is not a
    /// `Schema<_>` or shape marker, we fall back to standard type-based
    /// binding against the arg's inferred type.
    pub(in crate::typechecker) fn bind_from_arg_node(
        &self,
        param: &TypeExpr,
        arg: &SNode,
        type_params: &std::collections::BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Result<(), String> {
        match param {
            TypeExpr::Applied { name, args } if name == "Schema" && args.len() == 1 => {
                if let TypeExpr::Named(tp) = &args[0] {
                    if type_params.contains(tp) {
                        if let Some(resolved) = schema_type_expr_from_node(arg, scope) {
                            Self::bind_type_param(tp, &resolved, bindings)?;
                        }
                    }
                }
                Ok(())
            }
            TypeExpr::Shape(fields) => {
                if let Node::DictLiteral(entries) = &arg.node {
                    for field in fields {
                        let matching = entries.iter().find(|entry| match &entry.key.node {
                            Node::StringLiteral(key) | Node::Identifier(key) => key == &field.name,
                            _ => false,
                        });
                        if let Some(entry) = matching {
                            self.bind_from_arg_node(
                                &field.type_expr,
                                &entry.value,
                                type_params,
                                bindings,
                                scope,
                            )?;
                        }
                    }
                    return Ok(());
                }
                if let Some(arg_ty) = self.infer_type(arg, scope) {
                    let arg_ty = self.resolve_alias(&arg_ty, scope);
                    Self::extract_type_bindings(param, &arg_ty, type_params, bindings)?;
                }
                Ok(())
            }
            _ => {
                if let Some(arg_ty) = self.infer_type(arg, scope) {
                    // Resolve named aliases (`type Opts = {..}`) to their
                    // structural form first: `extract_type_bindings` is a
                    // pure function that can't see through an alias, so a
                    // `dict<string, V>` param would otherwise fail to bind V
                    // from a named-shape argument the way it does for an
                    // inline shape literal.
                    let arg_ty = self.resolve_alias(&arg_ty, scope);
                    Self::extract_type_bindings(param, &arg_ty, type_params, bindings)?;
                }
                Ok(())
            }
        }
    }

    pub(in crate::typechecker) fn apply_type_bindings(
        ty: &TypeExpr,
        bindings: &BTreeMap<String, TypeExpr>,
    ) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => bindings
                .get(name)
                .cloned()
                .unwrap_or_else(|| TypeExpr::Named(name.clone())),
            TypeExpr::Union(items) => TypeExpr::Union(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Intersection(items) => TypeExpr::Intersection(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: Self::apply_type_bindings(&field.type_expr, bindings),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            // Substitute the explicit fields and each row tail, then fold:
            // once the row variables resolve to shapes this collapses to a
            // precise closed `Shape` (the `{...R1, ...R2}` merge result).
            TypeExpr::OpenShape { fields, rests } => {
                let fields = fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: Self::apply_type_bindings(&field.type_expr, bindings),
                        optional: field.optional,
                    })
                    .collect();
                let rests = rests
                    .iter()
                    .map(|rest| Self::apply_type_bindings(rest, bindings))
                    .collect();
                super::super::binary_ops::fold_open_shape(fields, rests)
            }
            TypeExpr::List(inner) => {
                TypeExpr::List(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Iter(inner) => {
                TypeExpr::Iter(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Generator(inner) => {
                TypeExpr::Generator(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Stream(inner) => {
                TypeExpr::Stream(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(Self::apply_type_bindings(key, bindings)),
                Box::new(Self::apply_type_bindings(value, bindings)),
            ),
            TypeExpr::Applied { name, args } => TypeExpr::Applied {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| Self::apply_type_bindings(arg, bindings))
                    .collect(),
            },
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|param| Self::apply_type_bindings(param, bindings))
                    .collect(),
                return_type: Box::new(Self::apply_type_bindings(return_type, bindings)),
            },
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(s) => TypeExpr::LitString(s.clone()),
            TypeExpr::LitInt(v) => TypeExpr::LitInt(*v),
            TypeExpr::Owned(inner) => {
                TypeExpr::Owned(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
        }
    }

    pub(in crate::typechecker) fn applied_type_or_name(
        name: &str,
        args: Vec<TypeExpr>,
    ) -> TypeExpr {
        if args.is_empty() {
            TypeExpr::Named(name.to_string())
        } else {
            TypeExpr::Applied {
                name: name.to_string(),
                args,
            }
        }
    }

    pub(in crate::typechecker) fn infer_struct_bindings(
        &self,
        struct_info: &StructDeclInfo,
        fields: &[DictEntry],
        scope: &TypeScope,
    ) -> BTreeMap<String, TypeExpr> {
        let type_param_set: std::collections::BTreeSet<String> = struct_info
            .type_params
            .iter()
            .map(|tp| tp.name.clone())
            .collect();
        let mut bindings = BTreeMap::new();
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
            let _ = Self::extract_type_bindings(
                expected_type,
                &actual_type,
                &type_param_set,
                &mut bindings,
            );
        }
        bindings
    }

    pub(in crate::typechecker) fn infer_struct_type(
        &self,
        struct_name: &str,
        struct_info: &StructDeclInfo,
        fields: &[DictEntry],
        scope: &TypeScope,
    ) -> TypeExpr {
        let bindings = self.infer_struct_bindings(struct_info, fields, scope);
        let args = struct_info
            .type_params
            .iter()
            .map(|tp| {
                bindings
                    .get(&tp.name)
                    .cloned()
                    .unwrap_or_else(Self::wildcard_type)
            })
            .collect();
        Self::applied_type_or_name(struct_name, args)
    }

    pub(in crate::typechecker) fn infer_enum_type(
        &self,
        enum_name: &str,
        enum_info: &EnumDeclInfo,
        variant_name: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> TypeExpr {
        let type_param_set: std::collections::BTreeSet<String> = enum_info
            .type_params
            .iter()
            .map(|tp| tp.name.clone())
            .collect();
        let mut bindings = BTreeMap::new();
        if let Some(variant) = enum_info
            .variants
            .iter()
            .find(|variant| variant.name == variant_name)
        {
            for (field, arg) in variant.fields.iter().zip(args.iter()) {
                let Some(expected_type) = &field.type_expr else {
                    continue;
                };
                let Some(actual_type) = self.infer_type(arg, scope) else {
                    continue;
                };
                let _ = Self::extract_type_bindings(
                    expected_type,
                    &actual_type,
                    &type_param_set,
                    &mut bindings,
                );
            }
        }
        let args = enum_info
            .type_params
            .iter()
            .map(|tp| {
                bindings
                    .get(&tp.name)
                    .cloned()
                    .unwrap_or_else(Self::wildcard_type)
            })
            .collect();
        Self::applied_type_or_name(enum_name, args)
    }

    /// Recursively scan an AST node for FunctionCalls whose name is in
    /// `self.deprecated_fns`, emitting a warning at each call site.
    /// Standalone from `check_call` so it works even in expression
    /// positions where `check_node` only triggers `infer_type`.
    pub(in crate::typechecker) fn visit_for_deprecation(&mut self, node: &SNode) {
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                if let Some((since, use_hint)) = self.deprecated_fns.get(name).cloned() {
                    let mut msg = format!("`{name}` is deprecated");
                    if let Some(s) = since {
                        msg.push_str(&format!(" (since {s})"));
                    }
                    match use_hint {
                        Some(h) => self.warning_at_with_help(
                            Code::DeprecatedFunction,
                            msg,
                            node.span,
                            format!("use `{h}` instead"),
                        ),
                        None => self.warning_at(Code::DeprecatedFunction, msg, node.span),
                    }
                }
                for a in args {
                    self.visit_for_deprecation(a);
                }
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                self.visit_for_deprecation(object);
                for a in args {
                    self.visit_for_deprecation(a);
                }
            }
            Node::AttributedDecl { inner, .. } => self.visit_for_deprecation(inner),
            Node::CostRoute { options, body } => {
                for (_, value) in options {
                    self.visit_for_deprecation(value);
                }
                for s in body {
                    self.visit_for_deprecation(s);
                }
            }
            Node::HitlExpr { args, .. } => {
                for arg in args {
                    self.visit_for_deprecation(&arg.value);
                }
            }
            Node::Pipeline { body, .. }
            | Node::OverrideDecl { body, .. }
            | Node::FnDecl { body, .. }
            | Node::ToolDecl { body, .. }
            | Node::SpawnExpr { body }
            | Node::TryExpr { body }
            | Node::Block(body)
            | Node::Closure { body, .. }
            | Node::WhileLoop { body, .. }
            | Node::Retry { body, .. }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body, .. }
            | Node::Parallel { body, .. } => {
                for s in body {
                    self.visit_for_deprecation(s);
                }
            }
            Node::SkillDecl { fields, .. } => {
                for (_k, v) in fields {
                    self.visit_for_deprecation(v);
                }
            }
            Node::EvalPackDecl {
                fields,
                body,
                summarize,
                ..
            } => {
                for (_k, v) in fields {
                    self.visit_for_deprecation(v);
                }
                for s in body {
                    self.visit_for_deprecation(s);
                }
                if let Some(summary_body) = summarize {
                    for s in summary_body {
                        self.visit_for_deprecation(s);
                    }
                }
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.visit_for_deprecation(condition);
                for s in then_body {
                    self.visit_for_deprecation(s);
                }
                if let Some(eb) = else_body {
                    for s in eb {
                        self.visit_for_deprecation(s);
                    }
                }
            }
            Node::ForIn { iterable, body, .. } => {
                self.visit_for_deprecation(iterable);
                for s in body {
                    self.visit_for_deprecation(s);
                }
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in body {
                    self.visit_for_deprecation(s);
                }
                for s in catch_body {
                    self.visit_for_deprecation(s);
                }
                if let Some(fb) = finally_body {
                    for s in fb {
                        self.visit_for_deprecation(s);
                    }
                }
            }
            Node::DeadlineBlock { duration, body } => {
                self.visit_for_deprecation(duration);
                for s in body {
                    self.visit_for_deprecation(s);
                }
            }
            Node::MatchExpr { value, arms } => {
                self.visit_for_deprecation(value);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.visit_for_deprecation(g);
                    }
                    for s in &arm.body {
                        self.visit_for_deprecation(s);
                    }
                }
            }
            Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
                self.visit_for_deprecation(value);
            }
            Node::ConstBinding { value, .. } => {
                self.visit_for_deprecation(value);
            }
            Node::Assignment { target, value, .. } => {
                self.visit_for_deprecation(target);
                self.visit_for_deprecation(value);
            }
            Node::ReturnStmt { value: Some(v) } | Node::YieldExpr { value: Some(v) } => {
                self.visit_for_deprecation(v);
            }
            Node::EmitExpr { value: v } => {
                self.visit_for_deprecation(v);
            }
            Node::ThrowStmt { value }
            | Node::TryOperator { operand: value }
            | Node::TryStar { operand: value }
            | Node::Spread(value) => self.visit_for_deprecation(value),
            Node::UnaryOp { operand, .. } => self.visit_for_deprecation(operand),
            Node::BinaryOp { left, right, .. } => {
                self.visit_for_deprecation(left);
                self.visit_for_deprecation(right);
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.visit_for_deprecation(condition);
                self.visit_for_deprecation(true_expr);
                self.visit_for_deprecation(false_expr);
            }
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.visit_for_deprecation(object);
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                self.visit_for_deprecation(object);
                self.visit_for_deprecation(index);
            }
            Node::SliceAccess { object, start, end } => {
                self.visit_for_deprecation(object);
                if let Some(s) = start {
                    self.visit_for_deprecation(s);
                }
                if let Some(e) = end {
                    self.visit_for_deprecation(e);
                }
            }
            Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
                for a in args {
                    self.visit_for_deprecation(a);
                }
            }
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => {
                for e in entries {
                    self.visit_for_deprecation(&e.key);
                    self.visit_for_deprecation(&e.value);
                }
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.visit_for_deprecation(condition);
                for s in else_body {
                    self.visit_for_deprecation(s);
                }
            }
            Node::RequireStmt {
                condition, message, ..
            } => {
                self.visit_for_deprecation(condition);
                if let Some(m) = message {
                    self.visit_for_deprecation(m);
                }
            }
            Node::RangeExpr { start, end, .. } => {
                self.visit_for_deprecation(start);
                self.visit_for_deprecation(end);
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for c in cases {
                    self.visit_for_deprecation(&c.channel);
                    for s in &c.body {
                        self.visit_for_deprecation(s);
                    }
                }
                if let Some((d, b)) = timeout {
                    self.visit_for_deprecation(d);
                    for s in b {
                        self.visit_for_deprecation(s);
                    }
                }
                if let Some(b) = default_body {
                    for s in b {
                        self.visit_for_deprecation(s);
                    }
                }
            }
            Node::ImplBlock { methods, .. } => {
                for m in methods {
                    self.visit_for_deprecation(m);
                }
            }
            // Terminals / decls without nested expressions of interest
            _ => {}
        }
    }

    pub(in crate::typechecker) fn check_call(
        &mut self,
        name: &str,
        type_args: &[TypeExpr],
        args: &[SNode],
        scope: &mut TypeScope,
        span: Span,
    ) {
        // Cross-module undefined-call check. Only active when the caller
        // supplied a resolved imported-name set via `with_imported_names`;
        // in that mode every call target must be satisfied by builtins,
        // local declarations, struct constructors, callable variables, or
        // an imported symbol. Anything else is a static resolution error
        // (not a lint warning, so it fails `harn check`/`harn run` before
        // the VM does).
        if let Some(imported) = self.imported_names.as_ref() {
            let resolvable = is_builtin(name)
                || scope.get_fn(name).is_some()
                || scope.get_struct(name).is_some()
                || scope.get_enum(name).is_some()
                || scope.get_var(name).is_some()
                || imported.contains(name)
                || scope.is_generic_type_param(name)
                || name.starts_with("__")
                // `hostlib_*` builtins are registered onto the VM at
                // runtime by `harn_hostlib::install_default` (see the
                // `hostlib` cargo feature in `harn-cli`). The parser
                // has no static signature for them, so static
                // resolution treats the prefix as an opaque escape
                // hatch — same idea as `__`-prefixed names.
                || name.starts_with("hostlib_")
                // Built-in value constructors — `Ok`/`Err` are VM
                // builtins (Result variants) but are not in the
                // parser's BUILTIN_SIGNATURES table because they have
                // no static arity signature. `Some`/`None` are Option
                // variants constructed via the same path.
                || matches!(name, "Ok" | "Err" | "Some" | "None");
            if !resolvable {
                // Suggest a close match across builtins, local
                // functions, and imported names so typos show the same
                // "did you mean?" hint the linter used to provide.
                let candidates: Vec<String> = builtin_signatures::iter_builtin_names()
                    .map(|s| s.to_string())
                    .chain(scope.all_fn_names())
                    .chain(imported.iter().cloned())
                    .collect();
                let suggestion = crate::diagnostic::renamed_stdlib_symbol(name)
                    .map(str::to_string)
                    .or_else(|| {
                        crate::diagnostic::find_closest_match(
                            name,
                            candidates.iter().map(|s| s.as_str()),
                            2,
                        )
                        .map(|c| c.to_string())
                    });
                // Fold the suggestion into the message so callers that
                // only surface `diag.message` (like `harn run` / the
                // conformance runner) still see the "did you mean"
                // hint. The rendered help line also duplicates it for
                // pretty-printed output.
                let message = match &suggestion {
                    Some(s) => format!(
                        "call target `{name}` is not defined or imported — did you mean `{s}`?"
                    ),
                    None => format!("call target `{name}` is not defined or imported"),
                };
                match suggestion {
                    Some(s) => self.error_at_with_help(
                        Code::UndefinedFunction,
                        message,
                        span,
                        format!("did you mean `{s}`?"),
                    ),
                    None => self.error_at(Code::UndefinedFunction, message, span),
                }
            }
        }

        // Deprecation: emit a warning at every call site of an `@deprecated`
        // function, including `since:` and `use:` hints when present.
        // (Also covered by the visit_for_deprecation pass; keep both so
        // callers reachable only through one path are still flagged.)
        if let Some((since, use_hint)) = self.deprecated_fns.get(name).cloned() {
            let mut msg = format!("`{name}` is deprecated");
            if let Some(s) = since {
                msg.push_str(&format!(" (since {s})"));
            }
            let help = use_hint.map(|h| format!("use `{h}` instead"));
            match help {
                Some(h) => self.warning_at_with_help(Code::DeprecatedFunction, msg, span, h),
                None => self.warning_at(Code::DeprecatedFunction, msg, span),
            }
        }
        // Special-case: unreachable(x) — when the argument is a variable,
        // verify it has been narrowed to `never` (exhaustiveness check).
        if name == "unreachable" {
            if let Some(arg) = args.first() {
                if matches!(&arg.node, Node::Identifier(_)) {
                    let arg_type = self.infer_type(arg, scope);
                    if let Some(ref ty) = arg_type {
                        if !matches!(ty, TypeExpr::Never) {
                            self.error_at(Code::NonExhaustiveMatch,
                                format!(
                                    "unreachable() argument has type `{}` — not all cases are handled",
                                    format_type(ty)
                                ),
                                span,
                            );
                        }
                    }
                }
            }
            self.check_unknown_exhaustiveness(scope, span, "unreachable()");
            for arg in args {
                self.check_node(arg, scope);
            }
            return;
        }

        // Calls to user-defined functions with a `never` return type also
        // signal "this path claims exhaustiveness" — apply the same check.
        if let Some(sig) = scope.get_fn(name).cloned() {
            if matches!(sig.return_type, Some(TypeExpr::Never)) {
                self.check_unknown_exhaustiveness(scope, span, &format!("{name}()"));
            }
        }

        // Check against known function signatures
        let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
        if let Some(sig) = scope.get_fn(name).cloned() {
            if !type_args.is_empty() {
                if sig.type_param_names.is_empty() {
                    self.error_at(
                        Code::GenericTypeArgumentUnsupported,
                        format!("Function '{name}' does not declare type parameters"),
                        span,
                    );
                } else if type_args.len() != sig.type_param_names.len() {
                    self.error_at(
                        Code::GenericTypeArgumentArity,
                        format!(
                            "Function '{}' expects {} type arguments, got {}",
                            name,
                            sig.type_param_names.len(),
                            type_args.len()
                        ),
                        span,
                    );
                }
            }
            if !has_spread && !is_builtin(name) {
                let arity_ok = if sig.has_rest {
                    args.len() >= sig.params.len().saturating_sub(1)
                } else {
                    args.len() >= sig.required_params && args.len() <= sig.params.len()
                };
                if !arity_ok {
                    let total = sig.params.len();
                    let (expected, single_arg) = if sig.has_rest {
                        (
                            format!("at least {}", total.saturating_sub(1)),
                            total.saturating_sub(1) == 1,
                        )
                    } else if sig.required_params == total {
                        (format!("{total}"), total == 1)
                    } else {
                        (format!("{}-{}", sig.required_params, total), false)
                    };
                    let arg_word = if single_arg { "argument" } else { "arguments" };
                    self.warning_at(
                        Code::OrchestrationArity,
                        format!(
                            "Function '{}' expects {} {}, got {}",
                            name,
                            expected,
                            arg_word,
                            args.len()
                        ),
                        span,
                    );
                }
            }
            let mut type_bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
            let type_param_set: std::collections::BTreeSet<String> =
                sig.type_param_names.iter().cloned().collect();
            if type_args.len() == sig.type_param_names.len() {
                for (param_name, type_arg) in sig.type_param_names.iter().zip(type_args.iter()) {
                    type_bindings.insert(param_name.clone(), type_arg.clone());
                }
            }
            for (i, arg) in args.iter().enumerate() {
                let Some((_param_name, param_type)) = Self::function_param_for_arg(&sig, i) else {
                    continue;
                };
                if let Some(param_ty) = param_type {
                    if let Err(message) = self.bind_from_arg_node(
                        param_ty,
                        arg,
                        &type_param_set,
                        &mut type_bindings,
                        scope,
                    ) {
                        self.error_at(Code::ArgumentTypeMismatch, message, arg.span);
                    }
                }
            }
            let unbound_type_params: std::collections::BTreeSet<String> = type_param_set
                .iter()
                .filter(|name| !type_bindings.contains_key(*name))
                .cloned()
                .collect();
            let mut expected_args: Vec<Option<(String, TypeExpr)>> = Vec::with_capacity(args.len());
            let mut contextual_args = Vec::with_capacity(args.len());
            for (i, arg) in args.iter().enumerate() {
                let Some((param_name, param_type)) = Self::function_param_for_arg(&sig, i) else {
                    self.check_node(arg, scope);
                    expected_args.push(None);
                    contextual_args.push(false);
                    continue;
                };
                if let Some(expected) = param_type {
                    let expected = Self::apply_type_bindings(expected, &type_bindings);
                    let contextual_expected =
                        (!Self::contains_type_param(&expected, &unbound_type_params))
                            .then_some(&expected);
                    let context_checked =
                        self.check_node_with_expected(arg, contextual_expected, scope);
                    expected_args.push(Some((param_name.to_string(), expected)));
                    contextual_args.push(context_checked);
                } else {
                    self.check_node(arg, scope);
                    expected_args.push(None);
                    contextual_args.push(false);
                }
            }

            // Most callees have no generic type params and can reuse the
            // caller's scope by reference. The branch with a child scope
            // is only allocated when generic names need to be visible
            // during arg checking.
            let call_scope_owned;
            let call_scope: &TypeScope = if sig.type_param_names.is_empty() {
                scope
            } else {
                let mut s = scope.child();
                for tp_name in &sig.type_param_names {
                    s.generic_type_params.insert(tp_name.clone());
                }
                call_scope_owned = s;
                &call_scope_owned
            };
            for (i, arg) in args.iter().enumerate() {
                if let Some((param_name, expected)) =
                    expected_args.get(i).and_then(|entry| entry.as_ref())
                {
                    let actual = self.infer_type(arg, scope);
                    if let Some(actual) = &actual {
                        self.check_unknown_option_bag_fields(
                            format!("argument {} `{}`", i + 1, param_name),
                            param_name,
                            expected,
                            arg,
                            call_scope,
                        );
                        if !contextual_args.get(i).copied().unwrap_or(false)
                            && !self.types_compatible(expected, actual, call_scope)
                        {
                            self.type_mismatch_at(
                                Code::ArgumentTypeMismatch,
                                format!("argument {} `{}`", i + 1, param_name),
                                expected,
                                actual,
                                arg.span,
                                (
                                    sig.definition_span.map(|span| {
                                        (span, format!("parameter `{param_name}` declared here"))
                                    }),
                                    Some(arg.span),
                                ),
                                call_scope,
                            );
                        }
                    }
                }
            }
            if !sig.where_clauses.is_empty() {
                for (type_param, bound) in &sig.where_clauses {
                    if let Some(concrete_type) = type_bindings.get(type_param) {
                        let concrete_name = format_type(concrete_type);
                        let Some(base_type_name) = Self::base_type_name(concrete_type) else {
                            self.error_at(Code::WhereConstraintMismatch,
                                format!(
                                    "Type '{concrete_name}' does not satisfy interface '{bound}': only named types can satisfy interfaces (required by constraint `where {type_param}: {bound}`)"
                                ),
                                span,
                            );
                            continue;
                        };
                        if let Some(reason) = self.interface_mismatch_reason(
                            base_type_name,
                            bound,
                            &BTreeMap::new(),
                            scope,
                        ) {
                            self.error_at(
                                Code::WhereConstraintMismatch,
                                format!(
                                    "Type '{concrete_name}' does not satisfy interface '{bound}': {reason} \
                                     (required by constraint `where {type_param}: {bound}`)"
                                ),
                                span,
                            );
                        }
                    }
                }
            }
        } else if let Some(sig) =
            builtin_signatures::lookup(name).filter(|_| !self.name_is_imported(name))
        {
            // An explicit `import { name } from ...` shadows a same-named
            // builtin, matching the VM's runtime resolution. Guards the
            // case where an imported symbol is known by name but its
            // signature was not resolved into scope: without this, e.g.
            // `render` imported from `std/disclosure` (3 params) would be
            // checked against the `render` builtin (`path: string?,
            // bindings: dict`) and report phantom errors. Like every other
            // imported call in that mode, we then only check the arguments.
            self.check_builtin_signature_call(name, sig, type_args, args, has_spread, scope, span);
        } else {
            for arg in args {
                self.check_node(arg, scope);
            }
        }
    }

    /// Whether `name` was brought into scope by an `import` in the
    /// resolved cross-module mode (`with_imported_names`). Outside that
    /// mode there is no import set, so builtins are never shadowed.
    fn name_is_imported(&self, name: &str) -> bool {
        self.imported_names
            .as_ref()
            .is_some_and(|names| names.contains(name))
    }
}
