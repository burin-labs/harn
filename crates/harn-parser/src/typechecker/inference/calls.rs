//! Call-site checking, generic type-parameter binding, struct/enum
//! constructors, and the standalone deprecation visitor.
//!
//! `check_call` is the diagnostic-emitting per-call-site walk: it enforces
//! arity, argument types, generic binding, where-clause interface bounds,
//! cross-module resolvability, deprecation, and the
//! `unreachable() / never-returning` exhaustiveness contract. The
//! generic-binding helpers (`bind_type_param`, `extract_type_bindings`,
//! `bind_from_arg_node`, `apply_type_bindings`) are also used by
//! `subtyping::interface_mismatch_reason_for_type` and the inferred struct/enum
//! literal types in `expressions.rs`. `visit_for_deprecation` runs once
//! across the program to catch deprecated calls that hide inside
//! expression contexts where `check_node` would only trigger `infer_type`.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use crate::ast::*;
use crate::builtin_signatures;
use crate::builtin_signatures::TyExt;
use crate::diagnostic_codes::Code;
use harn_lexer::Span;

use super::super::format::format_type;
use super::super::schema_inference::{schema_of_type_token_call, schema_type_expr_from_node};
use super::super::scope::{EnumDeclInfo, FnSignature, StructDeclInfo, TypeScope};
use super::super::union::collapse_members_opt;
use super::super::union::reference_path_key;
use super::super::union::simplify_union;
use super::super::union::without_nil;
use super::super::TypeChecker;

#[derive(Clone, Copy)]
enum CallKind {
    Builtin,
    Function,
}

impl CallKind {
    fn label(self) -> &'static str {
        match self {
            Self::Builtin => "Builtin function",
            Self::Function => "Function",
        }
    }
}

struct CallParam<'a> {
    name: &'a str,
    ty: Option<Cow<'a, TypeExpr>>,
    bind_generics: bool,
    check_type: bool,
    allow_optional_nil: bool,
}

struct CallCheckSignature<'a> {
    name: &'a str,
    kind: CallKind,
    params: Vec<CallParam<'a>>,
    required_params: usize,
    type_param_names: Vec<String>,
    where_clauses: Vec<(String, TypeExpr)>,
    has_rest: bool,
    definition_span: Option<Span>,
}

impl TypeChecker {
    fn call_param_for_arg<'params, 'types>(
        params: &'params [CallParam<'types>],
        has_rest: bool,
        index: usize,
    ) -> Option<&'params CallParam<'types>> {
        if has_rest && index >= params.len().saturating_sub(1) {
            params.last()
        } else {
            params.get(index)
        }
    }

    fn minimum_call_args(required_params: usize, total_params: usize, has_rest: bool) -> usize {
        if has_rest {
            required_params.min(total_params.saturating_sub(1))
        } else {
            required_params
        }
    }

    fn call_signature_arity_ok(
        kind: CallKind,
        supplied: usize,
        required_params: usize,
        total_params: usize,
        has_rest: bool,
    ) -> bool {
        let minimum = Self::minimum_call_args(required_params, total_params, has_rest);
        let below_minimum = supplied < minimum;
        let above_builtin_maximum =
            matches!(kind, CallKind::Builtin) && !has_rest && supplied > total_params;
        !below_minimum && !above_builtin_maximum
    }

    fn call_signature_expected_arity(
        kind: CallKind,
        required_params: usize,
        total_params: usize,
        has_rest: bool,
    ) -> (String, bool) {
        let minimum = Self::minimum_call_args(required_params, total_params, has_rest);
        if matches!(kind, CallKind::Function) {
            return (format!("at least {minimum}"), minimum == 1);
        }
        if has_rest {
            (format!("at least {minimum}"), minimum == 1)
        } else if required_params == total_params {
            (total_params.to_string(), total_params == 1)
        } else {
            (format!("{required_params}-{total_params}"), false)
        }
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

    fn builtin_call_params(sig: &builtin_signatures::BuiltinSignature) -> Vec<CallParam<'_>> {
        sig.params
            .iter()
            .map(|param| {
                let is_schema_param = matches!(param.ty, builtin_signatures::Ty::SchemaOf(_));
                let is_any = param.ty.is_any();
                CallParam {
                    name: param.name,
                    ty: (!is_any).then(|| Cow::Owned(param.ty.to_type_expr())),
                    bind_generics: !is_any,
                    check_type: !is_any && !is_schema_param,
                    allow_optional_nil: param.optional,
                }
            })
            .collect()
    }

    fn function_call_params(sig: &FnSignature) -> Vec<CallParam<'_>> {
        sig.params
            .iter()
            .map(|(name, ty)| CallParam {
                name,
                ty: ty.as_ref().map(Cow::Borrowed),
                bind_generics: ty.is_some(),
                check_type: ty.is_some(),
                allow_optional_nil: false,
            })
            .collect()
    }

    fn typed_param_call_params(params: &[TypedParam]) -> Vec<CallParam<'_>> {
        params
            .iter()
            .map(|param| CallParam {
                name: &param.name,
                ty: param.type_expr.as_ref().map(Cow::Borrowed),
                bind_generics: param.type_expr.is_some(),
                check_type: param.type_expr.is_some(),
                allow_optional_nil: false,
            })
            .collect()
    }

    fn initial_type_bindings(
        type_param_names: &[String],
        type_args: &[TypeExpr],
    ) -> BTreeMap<String, TypeExpr> {
        let mut bindings = BTreeMap::new();
        if type_args.len() == type_param_names.len() {
            for (param_name, type_arg) in type_param_names.iter().zip(type_args.iter()) {
                bindings.insert(param_name.clone(), type_arg.clone());
            }
        }
        bindings
    }

    fn has_complete_explicit_type_bindings(
        type_param_names: &[String],
        type_args: &[TypeExpr],
    ) -> bool {
        !type_param_names.is_empty() && type_args.len() == type_param_names.len()
    }

    fn bind_call_params_from_args(
        &self,
        params: &[CallParam<'_>],
        has_rest: bool,
        type_param_names: &[String],
        args: &[SNode],
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Vec<(Span, String)> {
        let type_param_set: BTreeSet<String> = type_param_names.iter().cloned().collect();
        if type_param_set.is_empty() {
            return Vec::new();
        }
        let mut errors = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let Some(param) = Self::call_param_for_arg(params, has_rest, i) else {
                continue;
            };
            if !param.bind_generics {
                continue;
            }
            let Some(param_ty) = param.ty.as_deref() else {
                continue;
            };
            if let Err(message) =
                self.bind_from_arg_node(param_ty, arg, &type_param_set, bindings, scope)
            {
                errors.push((arg.span, message));
            }
        }
        errors
    }

    fn bind_inferred_call_type_params(
        &self,
        params: &[CallParam<'_>],
        has_rest: bool,
        type_param_names: &[String],
        type_args: &[TypeExpr],
        args: &[SNode],
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Vec<(Span, String)> {
        if Self::has_complete_explicit_type_bindings(type_param_names, type_args) {
            Vec::new()
        } else {
            self.bind_call_params_from_args(
                params,
                has_rest,
                type_param_names,
                args,
                bindings,
                scope,
            )
        }
    }

    pub(in crate::typechecker) fn infer_function_call_type_bindings(
        &self,
        sig: &FnSignature,
        type_args: &[TypeExpr],
        args: &[SNode],
        scope: &TypeScope,
    ) -> BTreeMap<String, TypeExpr> {
        let params = Self::function_call_params(sig);
        let mut bindings = Self::initial_type_bindings(&sig.type_param_names, type_args);
        let _ = self.bind_inferred_call_type_params(
            &params,
            sig.has_rest,
            &sig.type_param_names,
            type_args,
            args,
            &mut bindings,
            scope,
        );
        bindings
    }

    pub(in crate::typechecker) fn infer_builtin_call_type_bindings(
        &self,
        sig: &builtin_signatures::BuiltinSignature,
        type_args: &[TypeExpr],
        args: &[SNode],
        scope: &TypeScope,
    ) -> BTreeMap<String, TypeExpr> {
        let type_param_names = sig.type_param_names();
        let params = Self::builtin_call_params(sig);
        let mut bindings = Self::initial_type_bindings(&type_param_names, type_args);
        let _ = self.bind_inferred_call_type_params(
            &params,
            sig.has_rest,
            &type_param_names,
            type_args,
            args,
            &mut bindings,
            scope,
        );
        bindings
    }

    pub(in crate::typechecker) fn infer_typed_param_type_bindings(
        &self,
        params: &[TypedParam],
        has_rest: bool,
        type_param_names: &[String],
        args: &[SNode],
        scope: &TypeScope,
    ) -> (BTreeMap<String, TypeExpr>, Vec<(Span, String)>) {
        let call_params = Self::typed_param_call_params(params);
        let mut bindings = BTreeMap::new();
        let errors = self.bind_call_params_from_args(
            &call_params,
            has_rest,
            type_param_names,
            args,
            &mut bindings,
            scope,
        );
        (bindings, errors)
    }

    fn check_call_signature_arguments(
        &mut self,
        sig: CallCheckSignature<'_>,
        type_args: &[TypeExpr],
        args: &[SNode],
        has_spread: bool,
        scope: &mut TypeScope,
        span: Span,
    ) {
        let target_label = sig.kind.label();
        if !type_args.is_empty() {
            if sig.type_param_names.is_empty() {
                self.error_at(
                    Code::GenericTypeArgumentUnsupported,
                    format!(
                        "{target_label} '{}' does not declare type parameters",
                        sig.name
                    ),
                    span,
                );
            } else if type_args.len() != sig.type_param_names.len() {
                self.error_at(
                    Code::GenericTypeArgumentArity,
                    format!(
                        "{} '{}' expects {} type arguments, got {}",
                        target_label,
                        sig.name,
                        sig.type_param_names.len(),
                        type_args.len()
                    ),
                    span,
                );
            }
        }

        if !has_spread {
            let total = sig.params.len();
            let arity_ok = Self::call_signature_arity_ok(
                sig.kind,
                args.len(),
                sig.required_params,
                total,
                sig.has_rest,
            );
            if !arity_ok {
                let (expected, single_arg) = Self::call_signature_expected_arity(
                    sig.kind,
                    sig.required_params,
                    total,
                    sig.has_rest,
                );
                let arg_word = if single_arg { "argument" } else { "arguments" };
                let message = format!(
                    "{} '{}' expects {} {}, got {}",
                    target_label,
                    sig.name,
                    expected,
                    arg_word,
                    args.len()
                );
                match sig.kind {
                    CallKind::Builtin => self.warning_at(Code::BuiltinArity, message, span),
                    CallKind::Function => self.warning_at(Code::OrchestrationArity, message, span),
                }
            }
        }

        let mut type_bindings = Self::initial_type_bindings(&sig.type_param_names, type_args);
        for (error_span, message) in self.bind_inferred_call_type_params(
            &sig.params,
            sig.has_rest,
            &sig.type_param_names,
            type_args,
            args,
            &mut type_bindings,
            scope,
        ) {
            self.error_at(Code::ArgumentTypeMismatch, message, error_span);
        }

        let type_param_set: BTreeSet<String> = sig.type_param_names.iter().cloned().collect();
        let unbound_type_params: BTreeSet<String> = type_param_set
            .iter()
            .filter(|name| !type_bindings.contains_key(*name))
            .cloned()
            .collect();
        let mut expected_args: Vec<Option<(String, TypeExpr, bool)>> =
            Vec::with_capacity(args.len());
        let mut contextual_args = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            if matches!(sig.kind, CallKind::Builtin)
                && sig.name == "schema_of"
                && i == 0
                && matches!(arg.node, Node::Identifier(_))
            {
                expected_args.push(None);
                contextual_args.push(false);
                continue;
            }
            let Some(param) = Self::call_param_for_arg(&sig.params, sig.has_rest, i) else {
                self.check_node(arg, scope);
                expected_args.push(None);
                contextual_args.push(false);
                continue;
            };
            let Some(expected) = param.ty.as_deref().filter(|_| param.check_type) else {
                self.check_node(arg, scope);
                expected_args.push(None);
                contextual_args.push(false);
                continue;
            };
            let expected = Self::apply_type_bindings(expected, &type_bindings);
            let contextual_expected =
                (!Self::contains_type_param(&expected, &unbound_type_params)).then_some(&expected);
            let context_checked = self.check_node_with_expected(arg, contextual_expected, scope);
            expected_args.push(Some((
                param.name.to_string(),
                expected,
                param.allow_optional_nil,
            )));
            contextual_args.push(context_checked);
        }

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
            let Some((param_name, expected, allow_optional_nil)) =
                expected_args.get(i).and_then(|entry| entry.as_ref())
            else {
                continue;
            };
            let Some(actual) = self.infer_type(arg, scope) else {
                continue;
            };
            if matches!(sig.kind, CallKind::Builtin) {
                self.check_strict_llm_option_keys(sig.name, param_name, expected, arg);
            }
            if !matches!(sig.kind, CallKind::Builtin)
                || !Self::builtin_uses_strict_llm_option_keys(sig.name, param_name)
            {
                self.check_unknown_option_bag_fields(
                    format!("argument {} `{}`", i + 1, param_name),
                    param_name,
                    expected,
                    arg,
                    call_scope,
                );
            }
            let compatible = contextual_args.get(i).copied().unwrap_or(false)
                || self.types_compatible(expected, &actual, call_scope)
                || (*allow_optional_nil
                    && without_nil(&actual).is_none_or(|non_nil| {
                        self.types_compatible(expected, &non_nil, call_scope)
                    }));
            if !compatible {
                self.type_mismatch_at(
                    Code::ArgumentTypeMismatch,
                    format!("argument {} `{}`", i + 1, param_name),
                    expected,
                    &actual,
                    arg.span,
                    (
                        sig.definition_span
                            .map(|span| (span, format!("parameter `{param_name}` declared here"))),
                        Some(arg.span),
                    ),
                    call_scope,
                );
            }
        }

        for (type_param, bound) in &sig.where_clauses {
            if let Some(concrete_type) = type_bindings.get(type_param) {
                let concrete_name = format_type(concrete_type);
                let bound = Self::apply_type_bindings(bound, &type_bindings);
                let bound_name = format_type(&bound);
                if Self::base_type_name(concrete_type).is_none() {
                    self.error_at(Code::WhereConstraintMismatch,
                        format!(
                            "Type '{concrete_name}' does not satisfy interface '{bound_name}': only named types can satisfy interfaces (required by constraint `where {type_param}: {bound_name}`)"
                        ),
                        span,
                    );
                    continue;
                }
                if let Some(reason) =
                    self.interface_mismatch_reason_for_type(concrete_type, &bound, scope)
                {
                    self.error_at(
                        Code::WhereConstraintMismatch,
                        format!(
                            "Type '{concrete_name}' does not satisfy interface '{bound_name}': {reason} \
                             (required by constraint `where {type_param}: {bound_name}`)"
                        ),
                        span,
                    );
                }
            }
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
        let check_sig = CallCheckSignature {
            name,
            kind: CallKind::Builtin,
            params: Self::builtin_call_params(sig),
            required_params: sig.required_params(),
            type_param_names: sig.type_param_names(),
            where_clauses: sig
                .where_clause_strings()
                .into_iter()
                .map(|(type_param, bound)| (type_param, TypeExpr::Named(bound)))
                .collect(),
            has_rest: sig.has_rest,
            definition_span: None,
        };
        self.check_call_signature_arguments(check_sig, type_args, args, has_spread, scope, span);
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
            // Two arguments pinning the same parameter to different types
            // JOIN to their union instead of hard-erroring: `choose(1, "x")`
            // infers `T = int | string` the same way a heterogeneous list
            // literal infers `list<int | string>` (and TypeScript infers a
            // union). The call still fails downstream if the joined type
            // violates a bound or the declared return contract.
            if existing != concrete {
                let joined = simplify_union(vec![existing.clone(), concrete.clone()]);
                bindings.insert(param_name.to_string(), joined);
                return Ok(());
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
                        span: field.span,
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
                        span: field.span,
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
            Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
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
        self.check_cross_module_call_target_resolves(name, args, scope, span);

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
        // Special-case: unreachable(x) — when the argument is a variable or
        // stable reference path, verify it has been narrowed to `never`.
        if name == "unreachable" {
            if let Some(arg) = args.first() {
                if matches!(&arg.node, Node::Identifier(_)) || reference_path_key(arg).is_some() {
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
            let check_sig = CallCheckSignature {
                name,
                kind: CallKind::Function,
                params: Self::function_call_params(&sig),
                required_params: sig.required_params,
                type_param_names: sig.type_param_names.clone(),
                where_clauses: sig.where_clauses.clone(),
                has_rest: sig.has_rest,
                definition_span: sig.definition_span,
            };
            self.check_call_signature_arguments(
                check_sig, type_args, args, has_spread, scope, span,
            );
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
        } else if !schema_of_type_token_call(name, args) {
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
