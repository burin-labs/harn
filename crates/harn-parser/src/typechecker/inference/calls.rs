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

use std::collections::BTreeMap;

use crate::ast::*;
use crate::builtin_signatures;
use harn_lexer::Span;

use super::super::format::format_type;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{is_builtin, EnumDeclInfo, StructDeclInfo, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
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
                    Self::extract_type_bindings(param, &arg_ty, type_params, bindings)?;
                }
                Ok(())
            }
            _ => {
                if let Some(arg_ty) = self.infer_type(arg, scope) {
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
                        Some(h) => {
                            self.warning_at_with_help(msg, node.span, format!("use `{h}` instead"))
                        }
                        None => self.warning_at(msg, node.span),
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
            | Node::MutexBlock { body }
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
                self.visit_for_deprecation(object)
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
                let suggestion = crate::diagnostic::find_closest_match(
                    name,
                    candidates.iter().map(|s| s.as_str()),
                    2,
                )
                .map(|c| c.to_string());
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
                    Some(s) => {
                        self.error_at_with_help(message, span, format!("did you mean `{s}`?"))
                    }
                    None => self.error_at(message, span),
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
                Some(h) => self.warning_at_with_help(msg, span, h),
                None => self.warning_at(msg, span),
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
                            self.error_at(
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
                self.check_unknown_exhaustiveness(scope, span, &format!("{}()", name));
            }
        }

        // Check against known function signatures
        let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
        if let Some(sig) = scope.get_fn(name).cloned() {
            if !type_args.is_empty() {
                if sig.type_param_names.is_empty() {
                    self.error_at(
                        format!("Function '{}' does not declare type parameters", name),
                        span,
                    );
                } else if type_args.len() != sig.type_param_names.len() {
                    self.error_at(
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
            if !has_spread
                && !is_builtin(name)
                && !sig.has_rest
                && (args.len() < sig.required_params || args.len() > sig.params.len())
            {
                let expected = if sig.required_params == sig.params.len() {
                    format!("{}", sig.params.len())
                } else {
                    format!("{}-{}", sig.required_params, sig.params.len())
                };
                self.warning_at(
                    format!(
                        "Function '{}' expects {} arguments, got {}",
                        name,
                        expected,
                        args.len()
                    ),
                    span,
                );
            }
            // Build a scope that includes the function's generic type params
            // so they are treated as compatible with any concrete type.
            let call_scope = if sig.type_param_names.is_empty() {
                scope.clone()
            } else {
                let mut s = scope.child();
                for tp_name in &sig.type_param_names {
                    s.generic_type_params.insert(tp_name.clone());
                }
                s
            };
            let mut type_bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
            let type_param_set: std::collections::BTreeSet<String> =
                sig.type_param_names.iter().cloned().collect();
            if type_args.len() == sig.type_param_names.len() {
                for (param_name, type_arg) in sig.type_param_names.iter().zip(type_args.iter()) {
                    type_bindings.insert(param_name.clone(), type_arg.clone());
                }
            }
            for (arg, (_param_name, param_type)) in args.iter().zip(sig.params.iter()) {
                if let Some(param_ty) = param_type {
                    if let Err(message) = self.bind_from_arg_node(
                        param_ty,
                        arg,
                        &type_param_set,
                        &mut type_bindings,
                        scope,
                    ) {
                        self.error_at(message, arg.span);
                    }
                }
            }
            for (i, (arg, (param_name, param_type))) in
                args.iter().zip(sig.params.iter()).enumerate()
            {
                if let Some(expected) = param_type {
                    let actual = self.infer_type(arg, scope);
                    if let Some(actual) = &actual {
                        let expected = Self::apply_type_bindings(expected, &type_bindings);
                        if !self.types_compatible(&expected, actual, &call_scope) {
                            self.error_at(
                                format!(
                                    "Argument {} ('{}'): expected {}, got {}",
                                    i + 1,
                                    param_name,
                                    format_type(&expected),
                                    format_type(actual)
                                ),
                                arg.span,
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
                            self.error_at(
                                format!(
                                    "Type '{}' does not satisfy interface '{}': only named types can satisfy interfaces (required by constraint `where {}: {}`)",
                                    concrete_name, bound, type_param, bound
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
                                format!(
                                    "Type '{}' does not satisfy interface '{}': {} \
                                     (required by constraint `where {}: {}`)",
                                    concrete_name, bound, reason, type_param, bound
                                ),
                                span,
                            );
                        }
                    }
                }
            }
        } else if !type_args.is_empty() && is_builtin(name) {
            if let Some(sig) = builtin_signatures::lookup_generic_builtin_sig(name) {
                if type_args.len() != sig.type_params.len() {
                    self.error_at(
                        format!(
                            "Builtin function '{}' expects {} type arguments, got {}",
                            name,
                            sig.type_params.len(),
                            type_args.len()
                        ),
                        span,
                    );
                }
            } else {
                self.error_at(
                    format!(
                        "Builtin function '{}' does not declare type parameters",
                        name
                    ),
                    span,
                );
            }
        }
        // Check args recursively
        for arg in args {
            self.check_node(arg, scope);
        }
    }
}
