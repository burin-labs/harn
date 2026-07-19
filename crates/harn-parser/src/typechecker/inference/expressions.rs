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
use crate::builtin_signatures::BuiltinSignatureExt;
use crate::diagnostic_codes::Code;
use harn_lexer::{FixEdit, Span};

use super::super::binary_ops::{infer_binary_op_type, merge_shape_fields};
use super::super::format::format_type;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{builtin_return_type, InferredType, PathNarrowing, TypeScope};
use super::super::union::{
    intersect_types, narrow_to_single, reference_path_key, reference_path_key_for_subscript,
    remove_from_union, simplify_union, subtract_type, without_nil,
};
use super::super::{is_gradual_type_name, TypeChecker};

const UNNECESSARY_SAFE_NAVIGATION_RULE: &str = "unnecessary-safe-navigation";
const UNNECESSARY_NON_NULL_ASSERT_RULE: &str = "unnecessary-non-null-assert";

enum SafeNavigationKind<'a> {
    Subscript,
    Property(&'a str),
    Method,
}

/// Whether a subscript slot is being read or written. Governs the
/// `list`/`dict` index optionality rule in [`TypeChecker::subscript_slot_type`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::typechecker) enum SubscriptMode {
    Read,
    Write,
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
                | Node::MutexBlock { body, .. }
                | Node::DeadlineBlock { body, .. }
                | Node::Pipeline { body, .. }
                | Node::OverrideDecl { body, .. } => {
                    if let Some(ty) = self.infer_try_error_type(body, scope) {
                        inferred.push(ty);
                    }
                }
                // A `try`/`catch` handles some of its body's errors and lets the
                // rest escape — so the block contributes its *residual*, not the
                // raw body-thrown set. This is what makes the throws channel
                // (and its catch-exhaustiveness) sound: an error caught here does
                // not count against an enclosing `throws`, and one the catch does
                // not cover does. The runtime is type-selective — a typed
                // `catch (e: E)` matches only errors whose type is `E` and
                // rethrows the rest (see `Vm::handle_error`) — so the static
                // residual mirrors exactly what propagates at run time.
                Node::TryCatch {
                    body,
                    has_catch,
                    error_type,
                    catch_body,
                    finally_body,
                    ..
                } => {
                    let body_thrown = self.infer_try_error_type(body, scope);
                    if !has_catch {
                        // `try { } finally { }` with no catch: the body's errors
                        // propagate unchanged.
                        if let Some(ty) = body_thrown {
                            inferred.push(ty);
                        }
                    } else if let Some(catch_ty) = error_type {
                        // Typed catch: only body errors assignable to the catch
                        // type are handled; the residual propagates.
                        inferred.extend(self.uncaught_residual(body_thrown, catch_ty, scope));
                    }
                    // else: an untyped `catch` is a catch-all — no body error escapes.

                    // The catch handler and the `finally` block can themselves
                    // throw, and those always escape.
                    if *has_catch {
                        if let Some(ty) = self.infer_try_error_type(catch_body, scope) {
                            inferred.push(ty);
                        }
                    }
                    if let Some(finally_body) = finally_body {
                        if let Some(ty) = self.infer_try_error_type(finally_body, scope) {
                            inferred.push(ty);
                        }
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

    /// The members of a `try` body's thrown-type union that a `catch (e: E)`
    /// clause does *not* handle — i.e. those that will be rethrown and
    /// propagate past the block. A thrown member `M` is caught iff it is
    /// assignable to the catch type `E` (`E` is a supertype of `M`), matching
    /// the VM's type-selective `handle_error`. A bare/aggregate throw union is
    /// treated member-by-member so `catch (e: A)` over a `throw`-`A`-or-`B`
    /// body correctly leaves `B` escaping.
    fn uncaught_residual(
        &self,
        thrown: InferredType,
        catch_ty: &TypeExpr,
        scope: &TypeScope,
    ) -> Vec<TypeExpr> {
        let Some(thrown) = thrown else {
            return Vec::new();
        };
        let members = match thrown {
            TypeExpr::Union(members) => members,
            single => vec![single],
        };
        // The VM only catches *enum* errors: `handle_error` matches a thrown
        // `EnumVariant` by name and rethrows everything else. A typed catch on a
        // non-enum type therefore catches nothing at run time, so — to stay
        // sound (never under-count what escapes) — treat the whole body-thrown
        // set as escaping unless the catch type is an enum (or a union of them).
        if !self.is_enum_catch_type(catch_ty, scope) {
            return members;
        }
        members
            .into_iter()
            .filter(|member| !self.types_compatible(catch_ty, member, scope))
            .collect()
    }

    /// Whether `ty` is an enum type — or a union whose every non-nil member is
    /// one — i.e. a catch type the VM can actually match against a thrown
    /// `EnumVariant`. Used to keep [`Self::uncaught_residual`] sound with the
    /// runtime's enum-only `catch` dispatch.
    fn is_enum_catch_type(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        match self.resolve_alias(ty, scope) {
            TypeExpr::Named(name) => scope.get_enum(&name).is_some(),
            TypeExpr::Applied { name, .. } => scope.get_enum(&name).is_some(),
            TypeExpr::Union(members) => members
                .iter()
                .all(|m| self.type_is_nil(m, scope) || self.is_enum_catch_type(m, scope)),
            _ => false,
        }
    }

    /// Enforce a callable's declared `throws E` (or `throws (E1 | E2)`) channel:
    /// every value the body can `throw` — or surface via `?` — must conform to
    /// the declared set. Reuses [`Self::infer_try_error_type`] (the same
    /// collector that computes a `try` block's error type) to gather the body's
    /// thrown-type union, then requires it to be covered by `declared`.
    ///
    /// Only callables that opt into a `throws` clause are checked, so this never
    /// constrains existing unannotated code. Throw sites nested inside a local
    /// `try {}` are accounted for precisely: [`Self::infer_try_error_type`]
    /// subtracts the errors a `catch` clause handles and adds those the catch /
    /// finally bodies raise, so a caught error does not count against the
    /// declared set and an *uncaught* one does. That makes the declared channel
    /// catch-exhaustive — a `try`/`catch` that fails to cover an error the body
    /// can throw surfaces here as an uncovered escapee (HARN-TYP-026) unless the
    /// clause declares it.
    pub(in crate::typechecker) fn check_declared_throws(
        &mut self,
        declared: &TypeExpr,
        params: &[TypedParam],
        body: &[SNode],
        throws_span: Span,
        enclosing_scope: &TypeScope,
    ) {
        let mut body_scope = enclosing_scope.child();
        for param in params {
            let param_type = if param.rest {
                param
                    .type_expr
                    .clone()
                    .map(|inner| TypeExpr::List(Box::new(inner)))
            } else {
                param.type_expr.clone()
            };
            body_scope.define_var(&param.name, param_type);
        }
        let Some(actual) = self.infer_try_error_type(body, &body_scope) else {
            return;
        };
        if !self.types_compatible(declared, &actual, &body_scope) {
            self.error_at(
                Code::ThrowsTypeMismatch,
                format!(
                    "this callable can throw `{}`, which its declared `throws {}` does not cover",
                    format_type(&actual),
                    format_type(declared),
                ),
                throws_span,
            );
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
            self.narrow_match_subject(value, &arm.pattern, &mut arm_scope);
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

    fn infer_llm_call_result_type(
        &self,
        name: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let (data_type, data_required) = self.llm_call_schema_data_type(args, scope)?;
        let mut result = builtin_return_type(name)?;
        let TypeExpr::Shape(fields) = &mut result else {
            return Some(result);
        };
        if let Some(field) = fields.iter_mut().find(|field| field.name == "data") {
            field.type_expr = data_type;
            field.optional = !data_required;
        }
        Some(result)
    }

    fn llm_call_schema_data_type(
        &self,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Option<(TypeExpr, bool)> {
        let opts = args.get(2)?;
        let Node::DictLiteral(entries) = &opts.node else {
            return None;
        };
        let mut data_type = None;
        let mut data_required = false;
        for entry in entries {
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key.as_str(),
                _ => continue,
            };
            if key == "schema" || key == "output_schema" {
                data_type = schema_type_expr_from_node(&entry.value, scope);
            } else if key == "output_validation" {
                data_required =
                    matches!(&entry.value.node, Node::StringLiteral(value) if value == "error");
            }
        }
        data_type.map(|ty| (ty, data_required))
    }

    pub(in crate::typechecker) fn define_match_pattern_bindings(
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
                    match &element.node {
                        // Leading element pattern: binds the element type `T`.
                        Node::Identifier(name) if name != "_" => {
                            scope.define_var(name, item_type.clone());
                        }
                        // `...rest` collects the tail into a `list<T>` (parity
                        // with `let`-destructuring rest typing).
                        Node::Spread(inner) => {
                            if let Node::Identifier(name) = &inner.node {
                                if name != "_" {
                                    let rest_ty = Some(match &item_type {
                                        Some(t) => TypeExpr::List(Box::new(t.clone())),
                                        None => TypeExpr::Named("list".into()),
                                    });
                                    scope.define_var(name, rest_ty);
                                }
                            }
                        }
                        _ => {}
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
                self.define_enum_pattern_bindings(enum_name, variant, args, value_type, scope);
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                if let Node::Identifier(enum_name) = &object.node {
                    self.define_enum_pattern_bindings(enum_name, method, args, value_type, scope);
                }
            }
            // Bare call-shaped variant patterns use the same catalog decision
            // as codegen. The scrutinee type refines payload types only after
            // a globally unique owner has established the pattern's identity.
            Node::FunctionCall { name, args, .. } => {
                let catalog = scope.lexical_match_pattern_catalog();
                if let crate::lexical::BareVariantResolution::Unique(enum_name) =
                    catalog.resolve_bare_variant(name)
                {
                    self.define_enum_pattern_bindings(enum_name, name, args, value_type, scope);
                }
            }
            _ => {}
        }
    }

    pub(in crate::typechecker) fn define_enum_pattern_bindings(
        &self,
        enum_name: &str,
        variant: &str,
        args: &[SNode],
        value_type: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) {
        let Some(enum_info) = scope.get_enum(enum_name) else {
            return;
        };
        let Some(variant_info) = enum_info.variants.iter().find(|v| v.name == variant) else {
            return;
        };
        // Instantiate the variant's declared field types with the scrutinee's
        // type arguments: matching a `Result<int, string>` must bind
        // `Result.Ok(v)`'s payload as `int`, not the raw declaration-side
        // parameter `T`. When the scrutinee's arguments are unknown (bare
        // `Result`, or no static type), a field type that still mentions a
        // declaration parameter is degraded to gradual instead of leaking a
        // phantom named type into the arm scope.
        // Declaration params that are NOT also generic params of the
        // enclosing scope: after substitution these have no meaning at the
        // match site (inside `fn f<T>(r: Result<T, E2>)` the surviving `T`
        // is the function's own parameter and stays).
        let unbound_param_names: std::collections::BTreeSet<String> = enum_info
            .type_params
            .iter()
            .map(|tp| tp.name.clone())
            .filter(|name| !scope.is_generic_type_param(name))
            .collect();
        let type_bindings: BTreeMap<String, TypeExpr> = value_type
            .map(|ty| self.resolve_alias(ty, scope))
            .and_then(|resolved| match resolved {
                TypeExpr::Applied { name, args: targs }
                    if name == enum_name && targs.len() == enum_info.type_params.len() =>
                {
                    Some(
                        enum_info
                            .type_params
                            .iter()
                            .map(|tp| tp.name.clone())
                            .zip(targs)
                            .collect(),
                    )
                }
                _ => None,
            })
            .unwrap_or_default();
        let bindings: Vec<(String, InferredType)> = args
            .iter()
            .zip(&variant_info.fields)
            .filter_map(|(arg, field)| match &arg.node {
                Node::Identifier(name) if name != "_" => {
                    let field_ty = field.type_expr.as_ref().map(|ty| {
                        if type_bindings.is_empty() {
                            ty.clone()
                        } else {
                            Self::apply_type_bindings(ty, &type_bindings)
                        }
                    });
                    let field_ty = field_ty.filter(|ty| {
                        // Unbound declaration params stay gradual, not phantom.
                        !Self::contains_type_param(ty, &unbound_param_names)
                    });
                    Some((name.clone(), field_ty))
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
                // Build the shape by folding entries left-to-right with the
                // right-biased shape merge, so `{...base, k: v}` and
                // `{...a, ...b}` infer the precise merged record instead of a
                // bare `dict`. A spread of anything that isn't a statically
                // closed shape (a `dict`, `dict<K,V>`, union, or unknown) means
                // an unknown tail, so the result degrades to `dict` — we can't
                // name the merged fields without open-record types.
                let mut fields: Vec<ShapeField> = Vec::new();
                for entry in entries {
                    if matches!(&entry.value.node, Node::Spread(inner) if matches!(&inner.node, Node::Identifier(_) | Node::DictLiteral(_) | Node::PropertyAccess { .. } | Node::SubscriptAccess { .. } | Node::OptionalPropertyAccess { .. } | Node::FunctionCall { .. } | Node::MethodCall { .. }))
                    {
                        let Node::Spread(inner) = &entry.value.node else {
                            unreachable!()
                        };
                        match self.infer_type(inner, scope) {
                            Some(TypeExpr::Shape(spread_fields)) => {
                                fields = merge_shape_fields(&fields, &spread_fields);
                            }
                            _ => return Some(TypeExpr::Named("dict".into())),
                        }
                        continue;
                    }
                    let key = match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => key.clone(),
                        _ => return Some(TypeExpr::Named("dict".into())),
                    };
                    let val_type = self
                        .infer_type(&entry.value, scope)
                        .unwrap_or_else(Self::wildcard_type);
                    fields =
                        merge_shape_fields(&fields, &[ShapeField::synthetic(key, val_type, false)]);
                }
                // A dict literal has statically known fields, so it is a
                // precise closed record — the empty literal `{}` is the empty
                // record `Shape([])`, not the opaque `dict`. This is how TS/Flow
                // type object literals: `{}` is the top object type (it satisfies
                // an all-optional shape and accepts any later value), while a
                // *bare* `dict` value from `json_parse` stays opaque and must be
                // narrowed before it can flow into a specific shape.
                Some(TypeExpr::Shape(fields))
            }
            Node::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                let all_typed = params.iter().all(|p| p.type_expr.is_some());
                if all_typed || return_type.is_some() {
                    let param_types: Vec<TypeExpr> = params
                        .iter()
                        .map(|p| p.type_expr.clone().unwrap_or_else(Self::wildcard_type))
                        .collect();
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
                    let ret = return_type
                        .clone()
                        .or_else(|| self.infer_closure_body_return(body, &closure_scope));
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
                // A defined local shadows a same-named function — return its
                // type even when statically unknown (`None`), rather than
                // falling through to the function reference below. (Flattening
                // to `None` and falling through would mis-resolve a shadowing
                // `var x = …` to a function `x` of the same name.)
                if let Some(var_ty) = scope.get_var(name) {
                    return var_ty.clone();
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
                        let mut bindings =
                            self.infer_function_call_type_bindings(&sig, type_args, args, scope);
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
                if name == "llm_call" || name == "llm_completion" {
                    if let Some(result_type) = self.infer_llm_call_result_type(name, args, scope) {
                        return Some(result_type);
                    }
                }
                // Generic builtins (schema_parse/check/expect):
                // bind T by walking each arg node against the param
                // TypeExpr, then apply bindings to the declared return
                // type. Falls through to `builtin_return_type` when no T
                // can be bound (e.g. llm_call without an output_schema
                // option).
                if let Some(sig) = builtin_signatures::lookup(name).filter(|s| s.is_generic()) {
                    let type_param_names = sig.type_param_names();
                    let bindings =
                        self.infer_builtin_call_type_bindings(sig, type_args, args, scope);
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
                if op == "??" {
                    // `??` strips the nil arm from its left operand, but the
                    // structural `without_nil`/`contains_nil` helpers can't see
                    // through a named alias — so `type T = {..}?; x: T; x ?? d`
                    // would keep `T` (still nilable) as the result. Resolve
                    // aliases first so the coalesce drops nil for aliased
                    // nilable types exactly as it does for inline unions.
                    let lt = lt.map(|t| self.resolve_alias(&t, scope));
                    return infer_binary_op_type(op, &lt, &rt);
                }
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

            // `expr!` asserts the operand is non-nil, so its static type is the
            // operand type with every `nil` arm removed (`T | nil` -> `T`). When
            // the operand is already non-nil, `without_nil` returns it unchanged.
            Node::NonNullAssert { operand } => {
                let t = self.infer_type(operand, scope)?;
                without_nil(&t).or(Some(t))
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let refs = self.extract_refinements(condition, scope);

                let mut true_scope = scope.child();
                refs.apply_truthy(&mut true_scope);
                let tt = self.infer_type(true_expr, &true_scope);

                let mut false_scope = scope.child();
                refs.apply_falsy(&mut false_scope);
                let ft = self.infer_type(false_expr, &false_scope);

                // Branch merge mirrors if/else-expression inference:
                // `simplify_union` flattens nested unions, dedups, and
                // collapses `Never` arms so `cond ? throw("x") : 5` is `int`,
                // not `never | int`.
                match (&tt, &ft) {
                    (Some(a), Some(b)) if a == b => tt,
                    (Some(a), Some(b)) => Some(simplify_union(vec![a.clone(), b.clone()])),
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
                // Resolve named aliases ONCE and match every structural
                // receiver arm against the resolved form — otherwise sibling
                // methods disagree on aliased receivers (`type Env =
                // dict<string, string>`: `.values()` kept the value type but
                // `.map_values()` degraded to bare `dict`).
                let resolved_recv = obj_type.as_ref().map(|t| self.resolve_alias(t, scope));
                let include_optional_nil = optional_access
                    && obj_type
                        .as_ref()
                        .is_some_and(|ty| self.type_may_include_nil(ty, scope));
                let result = |ty| Self::optional_method_result_type(ty, include_optional_nil);
                if let Some(method_type) = obj_type
                    .as_ref()
                    .and_then(|ty| self.harness_method_return_type(ty, method.as_str(), scope))
                {
                    return Some(result(method_type));
                }
                // Iter<T> receiver: combinators preserve or transform T; sinks
                // materialize. This must come before the shared-method match
                // below so `.map` / `.filter` / etc. on an iter return Iter,
                // not list.
                let iter_elem_type: Option<TypeExpr> = match &resolved_recv {
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
                        "map" => {
                            let r = args
                                .first()
                                .and_then(|a| {
                                    self.infer_callable_return(a, std::slice::from_ref(&t), scope)
                                })
                                .unwrap_or_else(Self::wildcard_type);
                            return Some(result(iter_of(r)));
                        }
                        "flat_map" => {
                            let r = args
                                .first()
                                .and_then(|a| {
                                    self.infer_callable_return(a, std::slice::from_ref(&t), scope)
                                })
                                .map(Self::flatten_one_level)
                                .unwrap_or_else(Self::wildcard_type);
                            return Some(result(iter_of(r)));
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
                    match &resolved_recv {
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
                let is_dict = matches!(&resolved_recv, Some(TypeExpr::Named(n)) if n == "dict")
                    || matches!(&resolved_recv, Some(TypeExpr::DictType(..)))
                    || matches!(&resolved_recv, Some(TypeExpr::Shape(_)));
                // Element / key / value types of the (eager) collection receiver,
                // when parameterized. Eager list/dict combinators materialize a
                // new collection, so they return `List<…>` / `dict<…>` rather than
                // an `Iter`, but they preserve or transform the element type the
                // same way the lazy `Iter` combinators above do. `None` falls back
                // to the opaque `list`/`dict` type for unparameterized receivers.
                let list_elem: Option<TypeExpr> = match &resolved_recv {
                    Some(TypeExpr::List(inner)) => Some((**inner).clone()),
                    _ => None,
                };
                let (dict_key, dict_val): (Option<TypeExpr>, Option<TypeExpr>) =
                    match &resolved_recv {
                        Some(TypeExpr::DictType(k, v)) => {
                            (Some((**k).clone()), Some((**v).clone()))
                        }
                        _ => (None, None),
                    };
                let list_of = |t: TypeExpr| TypeExpr::List(Box::new(t));
                let pair_of = |k: TypeExpr, v: TypeExpr| TypeExpr::Applied {
                    name: "Pair".into(),
                    args: vec![k, v],
                };
                match method.as_str() {
                    // Shared: bool-returning methods
                    "contains" | "starts_with" | "ends_with" | "empty" | "has" | "any" | "all" => {
                        Some(result(TypeExpr::Named("bool".into())))
                    }
                    // Shared: int-returning methods
                    "count" | "index_of" | "last_index_of" | "rfind" => {
                        Some(result(TypeExpr::Named("int".into())))
                    }
                    // String methods
                    "trim" | "lowercase" | "uppercase" | "replace" | "substring" | "pad_left"
                    | "pad_right" | "repeat" | "join" => {
                        Some(result(TypeExpr::Named("string".into())))
                    }
                    // `reverse` works on both strings and lists, returning the
                    // receiver's own type. Reversing a list yields a list with the
                    // same element type; reversing a string yields a string. A
                    // gradual/unknown receiver (`any`/`_`, or an unannotated value
                    // whose type we couldn't pin down) could be either at runtime,
                    // so it stays gradual rather than being forced to `string` —
                    // forcing `string` mis-typed list reversals through untyped
                    // bindings (e.g. `list + value.reverse()`).
                    "reverse" => {
                        let is_list = list_elem.is_some()
                            || matches!(&resolved_recv, Some(TypeExpr::Named(n)) if n == "list");
                        let is_string =
                            matches!(&resolved_recv, Some(TypeExpr::Named(n)) if n == "string");
                        if is_list {
                            match &list_elem {
                                Some(e) => Some(result(list_of(e.clone()))),
                                None => Some(result(TypeExpr::Named("list".into()))),
                            }
                        } else if is_string {
                            Some(result(TypeExpr::Named("string".into())))
                        } else {
                            // Gradual receiver: defer to `any` so neither the
                            // string nor the list interpretation is rejected.
                            Some(result(TypeExpr::Named("any".into())))
                        }
                    }
                    "split" | "chars" => Some(result(TypeExpr::Named("list".into()))),
                    // filter returns dict for dicts, list for lists; the element
                    // type is unchanged (filter only drops elements).
                    "filter" => {
                        if is_dict {
                            match (&dict_key, &dict_val) {
                                (Some(k), Some(v)) => Some(result(TypeExpr::DictType(
                                    Box::new(k.clone()),
                                    Box::new(v.clone()),
                                ))),
                                _ => Some(result(TypeExpr::Named("dict".into()))),
                            }
                        } else {
                            match &list_elem {
                                Some(e) => Some(result(list_of(e.clone()))),
                                None => Some(result(TypeExpr::Named("list".into()))),
                            }
                        }
                    }
                    // List methods. `map`/`flat_map` produce the closure's return
                    // type per element (flat_map flattens one level); `sort` keeps
                    // the element type.
                    "map" => {
                        let param = list_elem.unwrap_or_else(Self::wildcard_type);
                        match args.first().and_then(|a| {
                            self.infer_callable_return(a, std::slice::from_ref(&param), scope)
                        }) {
                            Some(r) => Some(result(list_of(r))),
                            None => Some(result(TypeExpr::Named("list".into()))),
                        }
                    }
                    "flat_map" => {
                        let param = list_elem.unwrap_or_else(Self::wildcard_type);
                        match args.first().and_then(|a| {
                            self.infer_callable_return(a, std::slice::from_ref(&param), scope)
                        }) {
                            Some(r) => Some(result(list_of(Self::flatten_one_level(r)))),
                            None => Some(result(TypeExpr::Named("list".into()))),
                        }
                    }
                    "sort" => match &list_elem {
                        Some(e) => Some(result(list_of(e.clone()))),
                        None => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "window" | "each_cons" | "sliding_window" => match &list_elem {
                        Some(e) => Some(result(TypeExpr::List(Box::new(TypeExpr::List(
                            Box::new(e.clone()),
                        ))))),
                        None => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "reduce" | "find" | "first" | "last" => None,
                    // Dict methods — project the key/value type parameters.
                    "keys" => match &dict_key {
                        Some(k) => Some(result(list_of(k.clone()))),
                        None => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "values" => match &dict_val {
                        Some(v) => Some(result(list_of(v.clone()))),
                        None => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "entries" => match (&dict_key, &dict_val) {
                        (Some(k), Some(v)) => Some(result(list_of(pair_of(k.clone(), v.clone())))),
                        _ => Some(result(TypeExpr::Named("list".into()))),
                    },
                    "merge" | "map_values" | "rekey" | "map_keys" => {
                        // Rekey/map_keys transform keys; resulting dict still keys-by-string.
                        // Preserve the value-type parameter when known so downstream code can
                        // still rely on dict<string, V> typing after a key-rename.
                        if let Some(v) = &dict_val {
                            Some(result(TypeExpr::DictType(
                                Box::new(TypeExpr::Named("string".into())),
                                Box::new(v.clone()),
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
                condition,
                then_body,
                else_body,
                ..
            } => {
                // Narrow each branch with the condition's refinements, the same
                // way the ternary arm does — an `if`-expression used for its
                // value (`let x = if type_of(p) == "list" { p } else { ... }`)
                // must see `p` narrowed inside the matching branch, otherwise
                // the result type widens back to the un-narrowed union.
                let refs = self.extract_refinements(condition, scope);
                let mut then_scope = scope.child();
                refs.apply_truthy(&mut then_scope);
                let then_type = self.infer_block_type(then_body, &then_scope);
                match else_body {
                    Some(eb) => {
                        let mut else_scope = scope.child();
                        refs.apply_falsy(&mut else_scope);
                        let else_type = self.infer_block_type(eb, &else_scope);
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
        let natural = self.infer_property_type_from_type(&obj_type, property, scope, optional);

        // Flow-sensitive path narrowing: if a guard earlier on this path
        // (`type_of(o.x) == "T"`, `o.x != nil`) narrowed this exact reference,
        // re-apply that directive to the freshly computed natural type.
        if let Some(base_key) = reference_path_key(object) {
            let key = format!("{base_key}.{property}");
            if let Some(narrowing) = scope.get_narrowed_path(&key) {
                return Self::apply_path_narrowing(natural, narrowing);
            }
        }
        natural
    }

    /// Re-apply a [`PathNarrowing`] directive to a reference path's natural
    /// type. The directives reuse the same type algebra that variable
    /// narrowing uses (`narrow_to_single`/`remove_from_union` for `type_of`,
    /// `intersect_types`/`subtract_type` for schema checks), treating a
    /// non-union type as a one-member union. A top type (`unknown`/`any`)
    /// narrows straight to the tested kind on `Keep`, mirroring variable
    /// `unknown` narrowing. When a directive matches nothing (a statically
    /// dead branch) the natural type is returned unchanged rather than
    /// over-narrowing.
    fn apply_path_narrowing(natural: InferredType, narrowing: &PathNarrowing) -> InferredType {
        let ty = natural?;
        match narrowing {
            PathNarrowing::Keep(tag) => {
                if matches!(&ty, TypeExpr::Named(n) if is_gradual_type_name(n)) {
                    return Some(TypeExpr::Named(tag.clone()));
                }
                let members = Self::as_union_members(&ty);
                narrow_to_single(&members, tag).or(Some(ty))
            }
            PathNarrowing::Remove(tag) => {
                let members = Self::as_union_members(&ty);
                remove_from_union(&members, tag).or(Some(ty))
            }
            PathNarrowing::Intersect(schema) => intersect_types(&ty, schema).or(Some(ty)),
            PathNarrowing::Subtract(schema) => subtract_type(&ty, schema).or(Some(ty)),
            // Invalidated by a base reassignment: natural type unchanged.
            PathNarrowing::Cleared => Some(ty),
        }
    }

    fn as_union_members(ty: &TypeExpr) -> Vec<TypeExpr> {
        match ty {
            TypeExpr::Union(members) => members.clone(),
            other => vec![other.clone()],
        }
    }

    pub(super) fn infer_property_type_from_type(
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
                if is_gradual_type_name(name) || scope.is_generic_type_param(name) =>
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
                "term" => Some(TypeExpr::Named("HarnessTerm".into())),
                "clock" => Some(TypeExpr::Named("HarnessClock".into())),
                "fs" => Some(TypeExpr::Named("HarnessFs".into())),
                "env" => Some(TypeExpr::Named("HarnessEnv".into())),
                "random" => Some(TypeExpr::Named("HarnessRandom".into())),
                "net" => Some(TypeExpr::Named("HarnessNet".into())),
                "process" => Some(TypeExpr::Named("HarnessProcess".into())),
                "crypto" => Some(TypeExpr::Named("HarnessCrypto".into())),
                "system" => Some(TypeExpr::Named("HarnessSystem".into())),
                "secrets" => Some(TypeExpr::Named("HarnessSecrets".into())),
                "llm" => Some(TypeExpr::Named("HarnessLlm".into())),
                "tenant" => Some(TypeExpr::Named("HarnessTenant".into())),
                "auth" => Some(TypeExpr::Named("HarnessAuth".into())),
                "obs" => Some(TypeExpr::Named("HarnessObs".into())),
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
        let natural = self.infer_subscript_type_from_type(&obj_type, index, scope, optional);

        // Flow-sensitive path narrowing on a constant-index subscript
        // (`xs[0]`, `cfg["mode"]`) — same deferred mechanism as property
        // paths; `reference_path_key` only keys constant indices.
        if let Some(key) = reference_path_key_for_subscript(object, index) {
            if let Some(narrowing) = scope.get_narrowed_path(&key) {
                return Self::apply_path_narrowing(natural, narrowing);
            }
        }
        natural
    }

    pub(super) fn infer_subscript_type_from_type(
        &self,
        ty: &TypeExpr,
        index: &SNode,
        scope: &TypeScope,
        optional: bool,
    ) -> InferredType {
        self.subscript_slot_type(ty, index, scope, optional, SubscriptMode::Read)
    }

    /// Slot type of a subscript access, distinguishing a *read* (`v = xs[i]`)
    /// from a *write* target (`xs[i] = v`).
    ///
    /// In `Read` mode a `list`/`dict` index yields `T | nil`: an out-of-bounds
    /// index or an absent key is `nil` at runtime, so the read is unsound if
    /// typed as bare `T` (TypeScript's `noUncheckedIndexedAccess`). In `Write`
    /// mode it yields the bare element/value type `T`, because an assignment
    /// stores a present `T` — the absent-slot `nil` never reaches the RHS. A
    /// nilable element type such as `list<int?>` still admits `nil` on write,
    /// because that `nil` comes from `T` itself, not from the absent case.
    pub(super) fn subscript_slot_type(
        &self,
        ty: &TypeExpr,
        index: &SNode,
        scope: &TypeScope,
        optional: bool,
        mode: SubscriptMode,
    ) -> InferredType {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) if name == "nil" => {
                optional.then(|| TypeExpr::Named("nil".into()))
            }
            TypeExpr::Named(name)
                if is_gradual_type_name(name) || scope.is_generic_type_param(name) =>
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
                        self.subscript_slot_type(member, index, scope, optional, mode)
                    {
                        inferred.push(member_type);
                    } else {
                        return None;
                    }
                }
                (!inferred.is_empty()).then(|| simplify_union(inferred))
            }
            // Mirrors the Intersection arm of `infer_property_type_from_type`
            // — `x["a"]` and `x.a` must resolve the same set of types.
            TypeExpr::Intersection(members) => {
                for member in members {
                    if let Some(member_type) =
                        self.subscript_slot_type(member, index, scope, optional, mode)
                    {
                        return Some(member_type);
                    }
                }
                optional.then(|| TypeExpr::Named("nil".into()))
            }
            TypeExpr::List(inner) => Some(Self::index_slot_type((**inner).clone(), mode)),
            TypeExpr::DictType(_, value) => Some(Self::index_slot_type((**value).clone(), mode)),
            TypeExpr::Shape(fields) => {
                if let Node::StringLiteral(key) = &index.node {
                    Self::shape_property_type(fields, key, optional)
                } else {
                    None
                }
            }
            // A `string` index is a one-character `string`, absent (`nil`) when
            // the index is out of bounds — the same optionality rule as `list`.
            TypeExpr::Named(name) if name == "string" => Some(Self::index_slot_type(
                TypeExpr::Named("string".into()),
                mode,
            )),
            _ => None,
        }
    }

    /// Apply the read/write optionality rule for a `list`/`dict` index: reads
    /// widen the element type `T` to `T | nil` (the slot may be absent); writes
    /// keep the bare `T` (see [`Self::subscript_slot_type`]).
    fn index_slot_type(element: TypeExpr, mode: SubscriptMode) -> TypeExpr {
        match mode {
            SubscriptMode::Read => simplify_union(vec![element, TypeExpr::Named("nil".into())]),
            SubscriptMode::Write => element,
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

    /// Warn when `expr!` is applied to a value that is already provably
    /// non-nil — the assertion is dead and can be dropped. Mirrors the
    /// unnecessary-safe-navigation lint.
    pub(in crate::typechecker) fn check_unnecessary_non_null_assert(
        &mut self,
        snode: &SNode,
        operand: &SNode,
        scope: &TypeScope,
    ) {
        let Some(t) = self.infer_type(operand, scope) else {
            return;
        };
        if !self.type_is_provably_non_nil(&t, scope) {
            return;
        }
        let fix = self.non_null_assert_removal_fix(snode.span, operand);
        self.lint_warning_at_with_fix(
            Code::LintUnnecessaryNonNullAssert,
            UNNECESSARY_NON_NULL_ASSERT_RULE,
            "`!` is unnecessary because the value is already non-nil".to_string(),
            snode.span,
            "remove the `!`".to_string(),
            fix,
        );
    }

    /// Fix edit that deletes the trailing `!` of a non-null assertion.
    fn non_null_assert_removal_fix(&self, span: Span, operand: &SNode) -> Vec<FixEdit> {
        let Some(source) = self.source.as_deref() else {
            return Vec::new();
        };
        let search_start = operand.span.end.min(span.end);
        let search_end = span.end.min(source.len());
        let Some(region) = source.get(search_start..search_end) else {
            return Vec::new();
        };
        let Some(rel) = region.find('!') else {
            return Vec::new();
        };
        let start = search_start + rel;
        vec![FixEdit {
            span: Self::source_span_for_offsets(source, start, start + 1),
            replacement: String::new(),
        }]
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
            SafeNavigationKind::Subscript => "`?.[]`".to_string(),
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
            SafeNavigationKind::Subscript => {
                if let Some(start) = region.find("?.[") {
                    (start, 2, "")
                } else {
                    (region.find("?[")?, 1, "")
                }
            }
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

    /// Infer the return type of a callable argument (a closure or a function
    /// reference) given the parameter types the call site supplies. Powers
    /// `map`/`flat_map` element typing: a closure's params are bound to
    /// `param_types`, so an unannotated `{ x -> x * 2 }` still infers its body
    /// against the real element type instead of collapsing to `any`. The
    /// closure's own annotation, when present, wins over the contextual type.
    pub(super) fn infer_callable_return(
        &self,
        arg: &SNode,
        param_types: &[TypeExpr],
        scope: &TypeScope,
    ) -> InferredType {
        match &arg.node {
            Node::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                if let Some(return_type) = return_type {
                    return Some(return_type.clone());
                }
                let mut closure_scope = scope.child();
                for (i, param) in params.iter().enumerate() {
                    let param_ty = param
                        .type_expr
                        .clone()
                        .or_else(|| param_types.get(i).cloned());
                    closure_scope.define_var(&param.name, param_ty);
                }
                self.infer_closure_body_return(body, &closure_scope)
            }
            // A function reference (`xs.map(double)`): use its declared/inferred
            // return type.
            _ => match self.infer_type(arg, scope) {
                Some(TypeExpr::FnType { return_type, .. }) => Some(*return_type),
                _ => None,
            },
        }
    }

    /// The type a callable body yields: the trailing expression, or the operand
    /// of a trailing `return`.
    pub(in crate::typechecker) fn infer_closure_body_return(
        &self,
        body: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let last = body.last()?;
        match &last.node {
            Node::ReturnStmt { value: Some(v) } => self.infer_type(v, scope),
            Node::ReturnStmt { value: None } => Some(TypeExpr::Named("nil".into())),
            _ => self.infer_type(last, scope),
        }
    }

    /// Unwrap one level of list/iter nesting — `flat_map`'s closure returns a
    /// collection per element, which `flat_map` flattens by one level.
    fn flatten_one_level(ty: TypeExpr) -> TypeExpr {
        match ty {
            TypeExpr::List(inner) | TypeExpr::Iter(inner) => *inner,
            other => other,
        }
    }

    fn harness_method_return_type(
        &self,
        receiver: &TypeExpr,
        method: &str,
        scope: &TypeScope,
    ) -> InferredType {
        let receiver = self.resolve_alias(receiver, scope);
        match receiver {
            TypeExpr::Named(name) => {
                let sub_handle = crate::harness_methods::harness_type_sub_handle(name.as_str())?;
                if sub_handle == "term" && method == "read_password" {
                    return Some(TypeExpr::Named("string".into()));
                }
                crate::harness_methods::harness_sub_handle_ambient(sub_handle, method)
                    .and_then(builtin_return_type)
            }
            _ => None,
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
            Node::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                let mut closure_scope = scope.child();
                for (idx, param) in params.iter().enumerate() {
                    let ty = if idx == 0 {
                        param.type_expr.clone().or_else(|| left_type.clone())
                    } else {
                        param.type_expr.clone()
                    };
                    closure_scope.define_var(&param.name, ty);
                }
                return_type
                    .clone()
                    .or_else(|| self.infer_block_type(body, &closure_scope))
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
