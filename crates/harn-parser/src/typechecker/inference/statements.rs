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
use crate::diagnostic_codes::Code;

use super::super::binary_ops::infer_binary_op_type;
use super::super::exits::stmt_definitely_exits;
use super::super::format::{format_type, is_obvious_type};
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{
    EnumDeclInfo, FnSignature, ImplMethodSig, InferredType, InterfaceDeclInfo, StructDeclInfo,
    TypeAliasInfo, TypeScope,
};
use super::super::union::{
    collapse_members_opt, discriminant_field, narrow_shape_union_by_tag, narrow_to_single,
    simplify_union, without_nil, DiscriminantValue,
};
use super::super::{InlayHintInfo, TypeChecker};
use super::flow::{pattern_alternatives, resolve_union_shape_members};

#[derive(Clone, Copy)]
enum UntypedAccessKind {
    Property,
    Subscript,
}

impl UntypedAccessKind {
    fn direct_label(self) -> &'static str {
        match self {
            Self::Property => "Direct property access",
            Self::Subscript => "Direct subscript access",
        }
    }

    fn variable_label(self) -> &'static str {
        match self {
            Self::Property => "Accessing property",
            Self::Subscript => "Subscript access",
        }
    }
}

impl TypeChecker {
    pub(in crate::typechecker) fn check_block(&mut self, stmts: &[SNode], scope: &mut TypeScope) {
        let mut definitely_exited = false;
        for stmt in stmts {
            if definitely_exited {
                self.warning_at(
                    Code::UnreachableCode,
                    "unreachable code".to_string(),
                    stmt.span,
                );
                break; // warn once per block
            }
            self.check_node(stmt, scope);
            if Self::stmt_definitely_exits(stmt) {
                definitely_exited = true;
            }
        }
    }

    fn stmt_definitely_exits(stmt: &SNode) -> bool {
        stmt_definitely_exits(stmt)
    }

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

    pub(in crate::typechecker) fn iterable_item_type(
        &self,
        iter_type: &TypeExpr,
        scope: &TypeScope,
    ) -> InferredType {
        let resolved = self.resolve_alias(iter_type, scope);
        let non_nil = without_nil(&resolved)?;
        match self.resolve_alias(&non_nil, scope) {
            TypeExpr::List(inner)
            | TypeExpr::Iter(inner)
            | TypeExpr::Generator(inner)
            | TypeExpr::Stream(inner) => Some(*inner),
            TypeExpr::Applied { name, args } if name == "Iter" && args.len() == 1 => {
                Some(args[0].clone())
            }
            TypeExpr::DictType(key, value) => Some(TypeExpr::Applied {
                name: "Pair".into(),
                args: vec![*key, *value],
            }),
            TypeExpr::Named(name) if name == "string" => Some(TypeExpr::Named("string".into())),
            TypeExpr::Named(name) if name == "range" => Some(TypeExpr::Named("int".into())),
            TypeExpr::Union(members) => {
                let mut item_types = Vec::new();
                for member in members {
                    item_types.push(self.iterable_item_type(&member, scope)?);
                }
                Some(simplify_union(item_types))
            }
            _ => None,
        }
    }

    fn check_generic_method_bound(
        &mut self,
        object: &SNode,
        method: &str,
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(TypeExpr::Named(type_name)) = self.infer_type(object, scope) else {
            return;
        };
        if !scope.is_generic_type_param(&type_name) {
            return;
        }
        let Some(iface_name) = scope.get_where_constraint(&type_name) else {
            return;
        };
        let Some(iface_methods) = scope.get_interface(iface_name) else {
            return;
        };
        if iface_methods.methods.iter().any(|m| m.name == method) {
            return;
        }
        self.warning_at(
            Code::UnknownMethod,
            format!(
                "Method '{method}' not found in interface '{iface_name}' (constraint on '{type_name}')"
            ),
            span,
        );
    }

    /// Diagnose a property access (`obj.field` or `obj?.field`) against the
    /// statically-known type of `object`. Emits actionable errors for the
    /// common authoring failures that would otherwise surface late as a
    /// generic VM `cannot access property` runtime error or a downstream
    /// type mismatch:
    ///
    /// * Missing field on a known shape (with `did you mean` suggestion).
    /// * Missing field on a known struct.
    /// * Property access on a `nil` value (suggest `?.`).
    /// * Property access on a `T?` nilable value (suggest `?.` or guard).
    /// * Property access on an `unknown` value (suggest shape narrowing).
    ///
    /// Optional access (`obj?.field`) suppresses the nil-related diagnostics
    /// because that is exactly what `?.` is for; the missing-field check
    /// still fires so a typo in `user?.emial` is still caught.
    fn check_property_access(
        &mut self,
        object: &SNode,
        property: &str,
        scope: &TypeScope,
        span: Span,
        optional: bool,
    ) {
        let Some(raw) = self.infer_type(object, scope) else {
            return;
        };
        let resolved = self.resolve_alias(&raw, scope);
        // Decide whether to take the strict path. The "loose" case is the
        // ambient dict literal idiom (`let d = {a: 1}; d.missing` returns
        // nil at runtime) and the unannotated `var x = nil; ...; x.field`
        // widening pattern — both rely on the runtime's silent-nil
        // behavior. Strict diagnostics fire when the type came from a
        // real contract: a written annotation, a named alias or struct,
        // a struct/function-call return, or a nested property access.
        let is_strict_source = match &object.node {
            Node::Identifier(name) => {
                scope.is_annotated(name) || self.is_named_contract_type(&raw, scope)
            }
            _ => true,
        };
        if !is_strict_source {
            return;
        }
        match &resolved {
            TypeExpr::Named(name) if name == "nil" && !optional => {
                self.error_at_with_help(Code::InvalidOptionalAccess,
                    format!(
                        "cannot access property `{property}` on `nil`; the value is statically known to be nil here"
                    ),
                    span,
                    format!("use the optional access operator `?.{property}`, or narrow the value with a `!= nil` guard before reading fields"),
                );
            }
            TypeExpr::Named(name) if matches!(name.as_str(), "unknown") => {
                self.warning_at_with_help(Code::UnknownField,
                    format!("property access `.{property}` on an `unknown` value will fail at runtime if the value is not a shape with that field"),
                    span,
                    "narrow with `is_a`/`type_of`, validate with `assert_shape`, or annotate with a shape type before accessing fields"
                        .to_string(),
                );
            }
            TypeExpr::Shape(fields) if !fields.iter().any(|f| f.name == *property) => {
                let actual: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                let max_dist = if property.len() <= 4 { 1 } else { 2 };
                let suggestion = crate::diagnostic::find_closest_match(
                    property,
                    actual.iter().copied(),
                    max_dist,
                );
                let shape_str = format_type(&resolved);
                let mut msg = format!("field `{property}` does not exist on shape `{shape_str}`");
                if let Some(close) = suggestion {
                    msg.push_str(&format!(" — did you mean `{close}`?"));
                }
                let help = format!("available fields: {}", actual.join(", "));
                self.error_at_with_help(Code::UnknownField, msg, span, help);
            }
            TypeExpr::Named(name) if scope.get_struct(name).is_some() => {
                self.check_struct_property(name, property, scope, span);
            }
            TypeExpr::Applied { name, .. } if scope.get_struct(name).is_some() => {
                self.check_struct_property(name, property, scope, span);
            }
            _ if !optional => self.check_nilable_property_access(&resolved, property, scope, span),
            _ => {}
        }
    }

    /// True if `ty` carries a real type contract (named struct, named
    /// type alias, or a generic instantiation thereof). Used to decide
    /// whether an Identifier whose type was *inferred* (no annotation)
    /// should still take the strict property-access path. A bare
    /// `Shape(...)` returns false — that almost always came from a dict
    /// literal and historically tolerates `.missing` returning nil.
    fn is_named_contract_type(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        let name = match ty {
            TypeExpr::Named(name) => name,
            TypeExpr::Applied { name, .. } => name,
            _ => return false,
        };
        scope.get_struct(name).is_some()
            || scope.resolve_type_alias(name).is_some()
            || scope.get_enum(name).is_some()
    }

    /// Verify that `property` is declared on struct `name`. The
    /// existence check ignores type-parameter bindings — fields are
    /// declared by name, and generic instantiation only changes their
    /// types, not which names exist.
    fn check_struct_property(&mut self, name: &str, property: &str, scope: &TypeScope, span: Span) {
        let Some(info) = scope.get_struct(name) else {
            return;
        };
        if info.fields.iter().any(|f| f.name == property) {
            return;
        }
        let field_names: Vec<&str> = info.fields.iter().map(|f| f.name.as_str()).collect();
        let max_dist = if property.len() <= 4 { 1 } else { 2 };
        let suggestion =
            crate::diagnostic::find_closest_match(property, field_names.iter().copied(), max_dist);
        let mut msg = format!("field `{property}` does not exist on struct `{name}`");
        if let Some(close) = suggestion {
            msg.push_str(&format!(" — did you mean `{close}`?"));
        }
        let help = if field_names.is_empty() {
            format!("struct `{name}` has no fields")
        } else {
            format!("available fields: {}", field_names.join(", "))
        };
        self.error_at_with_help(Code::UnknownField, msg, span, help);
    }

    /// If `ty` is a `T | nil` union (any width), emit a hint to use `?.`
    /// or a nil guard. The generic `Union` member access already returns
    /// `None` from inference for non-optional access, but the resulting
    /// silent-pass leaves authors guessing why a runtime nil-property
    /// error fired. Pointing at the nilable type up-front matches the rest
    /// of the diagnostics in this module.
    fn check_nilable_property_access(
        &mut self,
        ty: &TypeExpr,
        property: &str,
        scope: &TypeScope,
        span: Span,
    ) {
        if !matches!(ty, TypeExpr::Union(_)) {
            return;
        }
        let TypeExpr::Union(members) = ty else {
            return;
        };
        let has_nil = members.iter().any(|m| self.type_is_nil(m, scope));
        if !has_nil {
            return;
        }
        // Only complain when the non-nil arms could plausibly accept the
        // property. Otherwise the user gets a useful "field does not
        // exist" diagnostic from the per-arm checks instead.
        let non_nil_admits_property = members.iter().any(|m| {
            if self.type_is_nil(m, scope) {
                return false;
            }
            let resolved = self.resolve_alias(m, scope);
            match &resolved {
                TypeExpr::Shape(fields) => fields.iter().any(|f| f.name == property),
                TypeExpr::Named(name) if scope.get_struct(name).is_some() => scope
                    .get_struct(name)
                    .map(|info| info.fields.iter().any(|f| f.name == property))
                    .unwrap_or(false),
                _ => false,
            }
        });
        if !non_nil_admits_property {
            return;
        }
        self.error_at_with_help(Code::InvalidOptionalAccess,
            format!(
                "cannot access property `{property}` on nilable type `{ty}`; the value may be nil at runtime",
                ty = format_type(ty)
            ),
            span,
            format!(
                "use the optional access operator `?.{property}`, or narrow the value with a `!= nil` guard to drop the nil arm"
            ),
        );
    }

    fn check_strict_untyped_access(
        &mut self,
        object: &SNode,
        scope: &TypeScope,
        span: Span,
        kind: UntypedAccessKind,
    ) {
        if !self.strict_types {
            return;
        }
        if let Node::FunctionCall { name, args, .. } = &object.node {
            if builtin_signatures::is_untyped_boundary_source(name) {
                let has_schema = (name == "llm_call" || name == "llm_completion")
                    && Self::llm_call_has_typed_schema_option(args, scope);
                if !has_schema {
                    self.warning_at_with_help(Code::BoundaryValueUnvalidated,
                        format!("{} on unvalidated `{}()` result", kind.direct_label(), name),
                        span,
                        "assign to a variable and validate with schema_expect() or a type annotation first".to_string(),
                    );
                }
            }
        }
        if let Node::Identifier(name) = &object.node {
            if let Some(source) = scope.is_untyped_source(name) {
                self.warning_at_with_help(Code::BoundaryValueUnvalidated,
                    format!(
                        "{} on unvalidated value '{}' from `{}`",
                        kind.variable_label(),
                        name,
                        source
                    ),
                    span,
                    "validate with schema_expect(), schema_is() in an if-condition, or add a shape type annotation".to_string(),
                );
            }
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
                                self.type_mismatch_at(
                                    Code::VariableTypeMismatch,
                                    format!("let binding `{name}`"),
                                    expected,
                                    actual,
                                    value.span,
                                    (
                                        Some((span, "expected type declared here".to_string())),
                                        Some(value.span),
                                    ),
                                    scope,
                                );
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
                    if type_ann.is_some() {
                        scope.mark_annotated(name);
                    }
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
                                self.type_mismatch_at(
                                    Code::VariableTypeMismatch,
                                    format!("var binding `{name}`"),
                                    expected,
                                    actual,
                                    value.span,
                                    (
                                        Some((span, "expected type declared here".to_string())),
                                        Some(value.span),
                                    ),
                                    scope,
                                );
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
                    if type_ann.is_some() {
                        scope.mark_annotated(name);
                    }
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

            Node::ConstBinding {
                name,
                type_ann,
                value,
            } => {
                // Walk and infer the value just like a let-binding so
                // existing diagnostics (undefined names, type mismatches)
                // still fire. The bounded const-eval pass below runs on
                // top of that — its failures land as HARN-MET-* /
                // HARN-CST-* diagnostics.
                self.check_node(value, scope);
                let inferred = self.infer_type(value, scope);
                if let Some(expected) = type_ann {
                    if let Some(actual) = &inferred {
                        if !self.types_compatible(expected, actual, scope) {
                            self.type_mismatch_at(
                                Code::VariableTypeMismatch,
                                format!("const binding `{name}`"),
                                expected,
                                actual,
                                value.span,
                                (
                                    Some((span, "expected type declared here".to_string())),
                                    Some(value.span),
                                ),
                                scope,
                            );
                        }
                    }
                }
                let ty = type_ann.clone().or(inferred);
                scope.define_var(name, ty);
                if type_ann.is_some() {
                    scope.mark_annotated(name);
                }
                scope.clear_nil_widenable(name);

                // Run the bounded sandbox interpreter. A successful fold
                // registers the value for later const initializers in
                // the same module; a failure emits a diagnostic keyed
                // off the failure kind so editor/CLI integrations can
                // dispatch on it.
                match crate::const_eval::const_eval(value, &self.const_env) {
                    Ok(folded) => {
                        self.const_env.insert(name.clone(), folded);
                    }
                    Err(err) => {
                        use crate::const_eval::ConstEvalErrorKind as K;
                        let message =
                            format!("const `{name}` initializer rejected: {}", err.detail);
                        match err.kind {
                            K::Disallowed => self.error_at(
                                Code::ConstEvalDisallowedExpression,
                                message,
                                err.span,
                            ),
                            K::StepLimit => {
                                self.error_at(Code::ConstEvalStepLimit, message, err.span);
                            }
                            K::RecursionLimit => {
                                self.error_at(Code::ConstEvalRecursionLimit, message, err.span);
                            }
                            K::SandboxViolation => {
                                self.error_at(Code::ConstEvalSandboxViolation, message, err.span);
                            }
                            K::RuntimeError => {
                                self.error_at(Code::ConstEvalRuntimeError, message, err.span);
                            }
                        }
                    }
                }
            }

            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_stream,
                ..
            } => {
                let callable_return_type =
                    Self::callable_return_type(*is_stream, return_type, body);
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: callable_return_type,
                    definition_span: Some(span),
                    type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                    required_params,
                    where_clauses: where_clauses
                        .iter()
                        .map(|wc| (wc.type_name.clone(), wc.bound.clone()))
                        .collect(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_fn_decl_variance(type_params, params, return_type.as_ref(), name, span);
                self.check_fn_body(
                    type_params,
                    params,
                    return_type,
                    body,
                    where_clauses,
                    *is_stream,
                    span,
                );
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
                    definition_span: Some(span),
                    type_param_names: Vec::new(),
                    required_params,
                    where_clauses: Vec::new(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                scope.clear_nil_widenable(name);
                self.check_value_returning_body(
                    params,
                    return_type,
                    body,
                    span,
                    "tool result",
                    "tool return type declared here",
                );
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

            Node::EvalPackDecl {
                binding_name,
                fields,
                body,
                summarize,
                ..
            } => {
                for (_key, value) in fields {
                    self.check_node(value, scope);
                }
                scope.define_var(binding_name, Some(TypeExpr::Named("dict".into())));
                scope.clear_nil_widenable(binding_name);

                if !body.is_empty() || summarize.is_some() {
                    let mut eval_scope = scope.child();
                    eval_scope.define_var("id", Some(TypeExpr::Named("string".into())));
                    eval_scope.clear_nil_widenable("id");
                    eval_scope.define_var("version", Some(TypeExpr::Named("int".into())));
                    eval_scope.clear_nil_widenable("version");
                    for (field_name, value) in fields {
                        let field_type = self.infer_type(value, scope);
                        eval_scope.define_var(field_name, field_type);
                        eval_scope.clear_nil_widenable(field_name);
                    }
                    self.check_block(body, &mut eval_scope);
                    if let Some(summary_body) = summarize {
                        self.check_block(summary_body, &mut eval_scope);
                    }
                }
            }

            Node::FunctionCall {
                name,
                type_args,
                args,
            } => {
                self.check_call(name, type_args, args, scope, span);
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
                let refs = self.extract_refinements_with_lint(condition, scope);

                let mut then_scope = scope.child();
                refs.apply_truthy(&mut then_scope);
                // Strict types: schema_is/is_type in condition clears
                // untyped source in then-branch
                if self.strict_types {
                    if let Node::FunctionCall { name, args, .. } = &condition.node {
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
                    let elem_type = iter_type
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope));
                    loop_scope.define_var(variable, elem_type);
                    loop_scope.clear_nil_widenable(variable);
                } else if let BindingPattern::Pair(a, b) = pattern {
                    // Pair destructuring: `for (k, v) in iter` — extract K, V
                    // from the yielded Pair<K, V>.
                    let (ka, vb) = iter_type
                        .as_ref()
                        .and_then(|ty| self.iterable_item_type(ty, scope))
                        .and_then(|ty| {
                            if let TypeExpr::Applied { name, args } = ty {
                                (name == "Pair" && args.len() == 2)
                                    .then(|| (Some(args[0].clone()), Some(args[1].clone())))
                            } else {
                                None
                            }
                        })
                        .unwrap_or((None, None));
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
                let refs = self.extract_refinements_with_lint(condition, scope);
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
                has_catch: _,
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
                    self.error_at(Code::TryOutsideFunction,
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
                        self.warning_at(Code::ImmutableAssignment,
                            format!(
                                "Cannot assign to '{name}': variable is immutable (declared with 'let')"
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
                                    self.type_mismatch_at(
                                        Code::AssignmentTypeMismatch,
                                        format!("assignment to `{name}`"),
                                        check_type,
                                        actual,
                                        value.span,
                                        (
                                            Some((
                                                target.span,
                                                format!("`{name}` has this expected type"),
                                            )),
                                            Some(value.span),
                                        ),
                                        scope,
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
                                self.warning_at(Code::InvalidMatchPattern,
                                    format!(
                                        "Match pattern type mismatch: matching {value_type_name} against {pattern_type} literal"
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
                if op == "&&" || op == "||" {
                    let refs = Self::extract_refinements(left, scope);
                    let mut right_scope = scope.child();
                    if op == "&&" {
                        refs.apply_truthy(&mut right_scope);
                    } else {
                        refs.apply_falsy(&mut right_scope);
                    }
                    self.check_node(right, &mut right_scope);
                    return;
                }
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
                                    Code::InvalidBinaryOperator,
                                    format!(
                                        "can't use '{op}' on {l} and {r} (needs numeric operands)"
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
                                    Code::InvalidBinaryOperator,
                                    format!("can't multiply {l} and {r} (try string * int)"),
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
                                let msg = format!("can't add {l} and {r}");
                                // Offer interpolation fix when one side is string
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(
                                        Code::StringInterpolationRewrite,
                                        msg,
                                        span,
                                        fix,
                                    );
                                } else {
                                    self.error_at(Code::InvalidBinaryOperator, msg, span);
                                }
                            }
                        }
                        "<" | ">" | "<=" | ">=" => {
                            let comparable = ["int", "float", "string"];
                            if !comparable.contains(&l.as_str())
                                || !comparable.contains(&r.as_str())
                            {
                                self.warning_at(
                                    Code::InvalidBinaryOperator,
                                    format!(
                                        "Comparison '{op}' may not be meaningful for types {l} and {r}"
                                    ),
                                    span,
                                );
                            } else if (l == "string") != (r == "string") {
                                self.warning_at(Code::InvalidBinaryOperator,
                                    format!(
                                        "Comparing {l} with {r} using '{op}' may give unexpected results"
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
            } => {
                self.check_node(object, scope);
                if self.check_harness_method_call(object, method, args, scope, span) {
                    return;
                }
                for arg in args {
                    self.check_node(arg, scope);
                }
                self.check_generic_method_bound(object, method, scope, span);
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.check_unnecessary_safe_method_call(snode, object, scope);
                self.check_node(object, scope);
                if self.check_harness_method_call(object, method, args, scope, span) {
                    return;
                }
                for arg in args {
                    self.check_node(arg, scope);
                }
                self.check_generic_method_bound(object, method, scope, span);
            }
            Node::PropertyAccess { object, property } => {
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Property);
                self.check_property_access(object, property, scope, span, false);
                self.check_node(object, scope);
            }
            Node::OptionalPropertyAccess { object, property } => {
                self.check_unnecessary_safe_property_access(snode, object, property, scope);
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Property);
                self.check_property_access(object, property, scope, span, true);
                self.check_node(object, scope);
            }
            Node::SubscriptAccess { object, index } => {
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Subscript);
                self.check_node(object, scope);
                self.check_node(index, scope);
            }
            Node::OptionalSubscriptAccess { object, index } => {
                self.check_unnecessary_safe_subscript_access(snode, object, scope);
                self.check_strict_untyped_access(object, scope, span, UntypedAccessKind::Subscript);
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
                let refs = self.extract_refinements_with_lint(condition, scope);

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
                let refs = self.extract_refinements_with_lint(condition, scope);

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

            Node::HitlExpr { kind, args } => {
                self.check_hitl_expr(*kind, args, scope, span);
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
                                    Code::OrchestrationType,
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
                        ParallelMode::Each | ParallelMode::EachStream | ParallelMode::Settle => {
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

            Node::CostRoute { options, body } => {
                for (key, value) in options {
                    if matches!(
                        key.as_str(),
                        "fallback_strategy" | "strategy" | "quality" | "min_quality"
                    ) && matches!(value.node, Node::Identifier(_))
                    {
                        continue;
                    }
                    self.check_node(value, scope);
                }
                let mut route_scope = scope.child();
                self.check_block(body, &mut route_scope);
            }

            Node::Closure { params, body, .. } => {
                let mut closure_scope = scope.child();
                for p in params {
                    closure_scope.define_var(&p.name, p.type_expr.clone());
                    closure_scope.clear_nil_widenable(&p.name);
                }
                self.fn_depth += 1;
                let saved_stream_depth = self.stream_fn_depth;
                let saved_stream_emit_types = self.stream_emit_types.clone();
                self.stream_fn_depth = 0;
                self.stream_emit_types.clear();
                self.check_block(body, &mut closure_scope);
                self.stream_fn_depth = saved_stream_depth;
                self.stream_emit_types = saved_stream_emit_types;
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
                if self.stream_fn_depth > 0 {
                    self.error_at(
                        Code::OrchestrationType,
                        "`yield` is not a stream emit; use `emit` inside `gen fn`".to_string(),
                        span,
                    );
                }
                if let Some(v) = value {
                    self.check_node(v, scope);
                }
            }

            Node::EmitExpr { value } => {
                self.check_node(value, scope);
                if self.stream_fn_depth == 0 {
                    self.error_at(
                        Code::OrchestrationType,
                        "`emit` can only be used inside a `gen fn`".to_string(),
                        span,
                    );
                } else if let Some(Some(expected)) = self.stream_emit_types.last().cloned() {
                    if let Some(actual) = self.infer_type(value, scope) {
                        if !self.types_compatible(&expected, &actual, scope) {
                            self.type_mismatch_at(
                                Code::ReturnTypeMismatch,
                                "`emit` value",
                                &expected,
                                &actual,
                                span,
                                (
                                    Some((span, "stream emit type expected here".to_string())),
                                    Some(value.span),
                                ),
                                scope,
                            );
                        }
                    }
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
                                    Code::UnknownField,
                                    format!("Unknown field '{key}' in struct '{struct_name}'"),
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
                                Code::FieldTypeMismatch,
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
                            self.type_mismatch_at(
                                Code::FieldTypeMismatch,
                                format!("field `{}` in struct `{struct_name}`", field.name),
                                &expected,
                                &actual_type,
                                entry.value.span,
                                (
                                    Some((span, format!("struct `{struct_name}` expected here"))),
                                    Some(entry.value.span),
                                ),
                                scope,
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
                            Code::UnknownTypeName,
                            message,
                            span,
                            format!("declare `struct {candidate} {{ ... }}` or fix the type name"),
                        ),
                        None => self.error_at_with_help(
                            Code::UnknownTypeName,
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
                            Code::InvalidEnumConstruct,
                            format!("Unknown variant '{variant}' in enum '{enum_name}'"),
                            span,
                        );
                        return;
                    };
                    if args.len() != enum_variant.fields.len() {
                        let n = enum_variant.fields.len();
                        let arg_word = if n == 1 { "argument" } else { "arguments" };
                        self.warning_at(
                            Code::OrchestrationArity,
                            format!(
                                "{}.{} expects {} {}, got {}",
                                enum_name,
                                variant,
                                n,
                                arg_word,
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
                            self.error_at(Code::GenericTypeArgumentMismatch, message, arg.span);
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
                            self.type_mismatch_at(
                                Code::ArgumentTypeMismatch,
                                format!("{}.{} argument `{}`", enum_name, variant, field.name),
                                &expected,
                                &actual_type,
                                arg.span,
                                (
                                    Some((
                                        span,
                                        format!(
                                            "enum variant `{enum_name}.{variant}` expected here"
                                        ),
                                    )),
                                    Some(arg.span),
                                ),
                                scope,
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
                            Code::InvalidMatchPattern,
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
    /// Recognized attribute names are the runtime/tooling attributes plus
    /// the durable-persona annotation set: `persona`, `trigger`, `handoff`,
    /// and `budget`. All other names produce a warning so misspellings
    /// surface early without breaking compilation.
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
                | "deterministic" | "semantic" | "archivist" | "retroactive" | "persona"
                | "step" | "trigger" | "handoff" | "budget" | "command" | "serial" | "heavy"
                | "scopes" => {}
                other => {
                    self.warning_at(
                        Code::UnknownAttribute,
                        format!("unknown attribute `@{other}`"),
                        attr.span,
                    );
                }
            }
            self.validate_standard_attribute_args(attr);
            // `@test` marks test pipelines discovered by `harn test`.
            if attr.name == "test" && !matches!(inner.node, Node::Pipeline { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@test` only applies to pipeline declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_tool" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@acp_tool` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if attr.name == "acp_skill" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@acp_skill` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(
                attr.name.as_str(),
                "persona" | "trigger" | "handoff" | "budget"
            ) && !matches!(
                inner.node,
                Node::FnDecl { .. } | Node::ToolDecl { .. } | Node::Pipeline { .. }
            ) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!(
                        "`@{}` only applies to function, tool, or pipeline declarations",
                        attr.name
                    ),
                    attr.span,
                );
            }
            if attr.name == "command" && !matches!(inner.node, Node::Pipeline { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@command` only applies to pipeline declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(attr.name.as_str(), "serial" | "heavy")
                && !matches!(inner.node, Node::Pipeline { .. })
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    format!(
                        "`@{}` only applies to pipeline declarations (use on `@test` or `test_*` pipelines)",
                        attr.name
                    ),
                    attr.span,
                );
            }
            if attr.name == "step" && !matches!(inner.node, Node::FnDecl { .. }) {
                self.warning_at(
                    Code::InvalidAttributeTarget,
                    "`@step` only applies to function declarations".to_string(),
                    attr.span,
                );
            }
            if matches!(
                attr.name.as_str(),
                "deterministic" | "semantic" | "archivist" | "retroactive"
            ) && !matches!(inner.node, Node::FnDecl { .. })
            {
                self.warning_at(
                    Code::InvalidAttributeTarget,
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
                    Code::InvalidAttributeTarget,
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
                Code::FlowInvariantAttributeInvalid,
                "`@deterministic` and `@semantic` are mutually exclusive; \
                 a Flow predicate is one mode or the other"
                    .to_string(),
                Span::merge(sem.span, det.span),
            );
        }

        if let Some(inv) = flow_invariant {
            if deterministic.is_none() && semantic.is_none() {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "Flow `@invariant` requires exactly one of `@deterministic` \
                     (default) or `@semantic`"
                        .to_string(),
                    inv.span,
                );
            }
            if archivist.is_none() {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "Flow `@invariant` is missing `@archivist(...)` provenance \
                     (evidence, confidence, source_date, coverage_examples)"
                        .to_string(),
                    inv.span,
                );
            }
        } else {
            if let Some(arch) = archivist {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
                    "`@archivist(...)` only applies to Flow predicates marked \
                     with `@invariant`"
                        .to_string(),
                    arch.span,
                );
            }
            if let Some(retro) = retroactive {
                self.warning_at(
                    Code::FlowInvariantAttributeInvalid,
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

    pub(in crate::typechecker) fn validate_standard_attribute_args(&mut self, attr: &Attribute) {
        match attr.name.as_str() {
            "persona" => self.validate_persona_args(attr),
            "step" => self.validate_step_args(attr),
            "trigger" => self.validate_trigger_args(attr),
            "handoff" => self.validate_handoff_args(attr),
            "budget" => self.validate_budget_args(attr),
            "deprecated" => self.validate_deprecated_args(attr),
            "command" => self.validate_command_args(attr),
            "serial" => self.validate_serial_args(attr),
            "heavy" => self.validate_heavy_args(attr),
            "scopes" => self.validate_scopes_args(attr),
            "test" if !attr.args.is_empty() => {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@test` does not accept arguments".to_string(),
                    attr.span,
                );
            }
            _ => {}
        }
    }

    fn validate_command_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["name", "description", "hint"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@command", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@command` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@command", name, &arg.value, arg.span);
        }
    }

    fn validate_step_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "name",
            "model",
            "approval",
            "receipt",
            "error_boundary",
            "retry",
            "budget",
        ];
        let mut has_name = false;
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@step", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@step` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "name" => {
                    has_name = true;
                    self.expect_symbol_like("@step", name, &arg.value, arg.span);
                }
                "model" => self.expect_symbol_like("@step", name, &arg.value, arg.span),
                "approval" => self.expect_one_of(
                    "@step",
                    name,
                    &arg.value,
                    arg.span,
                    &["required", "optional"],
                ),
                "receipt" => {
                    self.expect_one_of("@step", name, &arg.value, arg.span, &["audit", "none"]);
                }
                "error_boundary" => self.expect_one_of(
                    "@step",
                    name,
                    &arg.value,
                    arg.span,
                    &["fail", "continue", "escalate"],
                ),
                "retry" => self.expect_step_retry_dict(&arg.value, arg.span),
                "budget" => self.expect_step_budget_dict(&arg.value, arg.span),
                _ => {}
            }
        }
        if !has_name {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(...)` should declare `name: \"...\"` for stable supervision metadata"
                    .to_string(),
                attr.span,
            );
        }
    }

    fn expect_step_budget_dict(&mut self, value: &SNode, span: Span) {
        const NUMBER_KEYS: &[&str] = &["max_tokens", "max_usd"];
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(budget: ...)` must be a dict such as `{ max_tokens: 1000, max_usd: 0.05 }`"
                    .to_string(),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: ...)` field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            if !NUMBER_KEYS.contains(&field_name) {
                self.warning_at(Code::InvalidAttributeArgument,
                    format!(
                        "unknown `@step(budget: ...)` field `{field_name}`; expected one of {NUMBER_KEYS:?}"
                    ),
                    entry.key.span,
                );
                continue;
            }
            match (field_name, &entry.value.node) {
                ("max_tokens", Node::IntLiteral(value)) if *value >= 1 => {}
                ("max_tokens", _) => self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: { max_tokens: ... })` must be a positive integer".to_string(),
                    entry.value.span,
                ),
                ("max_usd", Node::IntLiteral(value)) if *value >= 0 => {}
                ("max_usd", Node::FloatLiteral(value)) if value.is_finite() && *value >= 0.0 => {}
                ("max_usd", _) => self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(budget: { max_usd: ... })` must be a non-negative number".to_string(),
                    entry.value.span,
                ),
                _ => {}
            }
        }
    }

    fn validate_persona_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "name",
            "description",
            "triggers",
            "schedules",
            "tools",
            "autonomy",
            "budget",
            "handoffs",
            "context_packs",
            "evals",
            "receipts",
            "model",
            "owner",
            "stages",
        ];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@persona", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@persona` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "triggers" | "schedules" => {
                    self.expect_list_of_trigger_specs("@persona", name, &arg.value, arg.span);
                }
                "tools" | "handoffs" | "context_packs" | "evals" => {
                    self.expect_list_of_symbols("@persona", name, &arg.value, arg.span);
                }
                "budget" => self.expect_budget_dict("@persona", name, &arg.value, arg.span),
                "stages" => self.expect_persona_stages("@persona", &arg.value, arg.span),
                "receipts" => {
                    if !is_symbol_like(&arg.value.node)
                        && !matches!(arg.value.node, Node::BoolLiteral(_))
                    {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@persona(receipts: ...)` must be a string/symbol or bool".to_string(),
                            arg.span,
                        );
                    }
                }
                _ => self.expect_symbol_like("@persona", name, &arg.value, arg.span),
            }
        }
    }

    fn expect_persona_stages(&mut self, owner: &str, value: &SNode, span: Span) {
        let Node::ListLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{owner}(stages: ...)` must be a list of stage dicts"),
                span,
            );
            return;
        };
        const KNOWN_STAGE_KEYS: &[&str] = &[
            "name",
            "allowed_tools",
            "side_effect_level",
            "max_iterations",
        ];
        for entry in entries {
            let Node::DictLiteral(fields) = &entry.node else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{owner}(stages: ...)` entries must be dict literals"),
                    entry.span,
                );
                continue;
            };
            let mut saw_name = false;
            for dict_entry in fields {
                let Some(key) = dict_entry_key_str(&dict_entry.key) else {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!("`{owner}(stages: ...)` stage keys must be identifiers"),
                        dict_entry.key.span,
                    );
                    continue;
                };
                if !KNOWN_STAGE_KEYS.contains(&key.as_str()) {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!("unknown stage key `{key}`; expected one of {KNOWN_STAGE_KEYS:?}"),
                        dict_entry.key.span,
                    );
                    continue;
                }
                match key.as_str() {
                    "name" | "side_effect_level" => {
                        if !is_symbol_like(&dict_entry.value.node) {
                            self.warning_at(
                                Code::InvalidAttributeArgument,
                                format!("stage `{key}` must be a string"),
                                dict_entry.value.span,
                            );
                        }
                        if key == "name" {
                            saw_name = true;
                        }
                    }
                    "allowed_tools" => self.expect_list_of_symbols(
                        owner,
                        "allowed_tools",
                        &dict_entry.value,
                        dict_entry.value.span,
                    ),
                    "max_iterations" if !matches!(dict_entry.value.node, Node::IntLiteral(n) if n >= 0) =>
                    {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "stage `max_iterations` must be a non-negative integer".to_string(),
                            dict_entry.value.span,
                        );
                    }
                    _ => {}
                }
            }
            if !saw_name {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{owner}(stages: ...)` entry missing required `name`"),
                    entry.span,
                );
            }
        }
    }

    fn validate_trigger_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &[
            "id", "provider", "kind", "event", "when", "schedule", "budget",
        ];
        for arg in &attr.args {
            if arg.name.is_none() {
                if !is_trigger_spec(&arg.value.node) {
                    self.warning_at(Code::InvalidAttributeArgument,
                        "`@trigger(...)` positional arguments must be strings, dotted trigger ids, or schedule(...)"
                            .to_string(),
                        arg.span,
                    );
                }
                continue;
            }
            let name = arg.name.as_deref().unwrap();
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@trigger` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "schedule" => {
                    if !is_trigger_spec(&arg.value.node) {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@trigger(schedule: ...)` must be a string/symbol or schedule(...)"
                                .to_string(),
                            arg.span,
                        );
                    }
                }
                "budget" => self.expect_budget_dict("@trigger", name, &arg.value, arg.span),
                _ => self.expect_symbol_like("@trigger", name, &arg.value, arg.span),
            }
        }
    }

    fn validate_handoff_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["target", "to", "reason", "schema", "artifact"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@handoff", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@handoff` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "target" | "to" => {
                    if is_symbol_like(&arg.value.node) {
                        continue;
                    }
                    self.expect_list_of_symbols("@handoff", name, &arg.value, arg.span);
                }
                _ => self.expect_symbol_like("@handoff", name, &arg.value, arg.span),
            }
        }
    }

    fn validate_budget_args(&mut self, attr: &Attribute) {
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@budget", arg) else {
                continue;
            };
            self.expect_budget_value("@budget", name, &arg.value, arg.span);
        }
    }

    fn validate_serial_args(&mut self, attr: &Attribute) {
        // `@serial` may be bare or take a single `group: "name"` arg.
        const KNOWN_KEYS: &[&str] = &["group"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@serial", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@serial` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@serial", name, &arg.value, arg.span);
        }
    }

    fn validate_heavy_args(&mut self, attr: &Attribute) {
        // `@heavy` requires a positive integer `threads` arg.
        const KNOWN_KEYS: &[&str] = &["threads"];
        let mut has_threads = false;
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@heavy", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@heavy` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            if name == "threads" {
                has_threads = true;
                if !matches!(arg.value.node, Node::IntLiteral(n) if n >= 1) {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        "`@heavy(threads: ...)` must be a positive integer".to_string(),
                        arg.span,
                    );
                }
            }
        }
        if !has_threads {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@heavy(...)` must specify `threads: <positive int>`".to_string(),
                attr.span,
            );
        }
    }

    /// Validate `@scopes("a:b", "c:d", ...)`. Arguments must be string
    /// literals (positional or named — the string value is what counts);
    /// at least one is required, and each value should be a non-empty
    /// `resource:action` shape. The shape is just a lint here so misspelled
    /// scopes surface at typecheck instead of at the first 403.
    fn validate_scopes_args(&mut self, attr: &Attribute) {
        if attr.args.is_empty() {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@scopes(...)` requires at least one scope literal, e.g. `@scopes(\"personas:read\")`"
                    .to_string(),
                attr.span,
            );
            return;
        }
        for arg in &attr.args {
            let Some(value) = symbol_like_value(&arg.value.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@scopes(...)` arguments must be string literals".to_string(),
                    arg.span,
                );
                continue;
            };
            if value.is_empty() {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@scopes(...)` arguments cannot be empty strings".to_string(),
                    arg.span,
                );
                continue;
            }
            if !value.contains(':') {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!(
                        "`@scopes({value:?})` should be a `resource:action` literal like `\"personas:read\"`"
                    ),
                    arg.span,
                );
            }
        }
    }

    fn validate_deprecated_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["since", "use"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@deprecated", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!(
                        "unknown `@deprecated` argument `{name}`; expected one of {KNOWN_KEYS:?}"
                    ),
                    arg.span,
                );
                continue;
            }
            self.expect_symbol_like("@deprecated", name, &arg.value, arg.span);
        }
    }

    fn require_named_arg<'a>(&mut self, attr_name: &str, arg: &'a AttributeArg) -> Option<&'a str> {
        match arg.name.as_deref() {
            Some(name) => Some(name),
            None => {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{attr_name}(...)` arguments must be named"),
                    arg.span,
                );
                None
            }
        }
    }

    fn expect_symbol_like(&mut self, attr_name: &str, key: &str, value: &SNode, span: Span) {
        if !is_symbol_like(&value.node) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a string or symbol"),
                span,
            );
        }
    }

    fn expect_one_of(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
        allowed: &[&str],
    ) {
        let Some(value) = symbol_like_value(&value.node) else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be one of {allowed:?}"),
                span,
            );
            return;
        };
        if !allowed.contains(&value) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be one of {allowed:?}"),
                span,
            );
        }
    }

    fn expect_list_of_symbols(&mut self, attr_name: &str, key: &str, value: &SNode, span: Span) {
        let Node::ListLiteral(items) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a list of strings or symbols"),
                span,
            );
            return;
        };
        if items.iter().any(|item| !is_symbol_like(&item.node)) {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must contain only strings or symbols"),
                span,
            );
        }
    }

    fn expect_list_of_trigger_specs(
        &mut self,
        attr_name: &str,
        key: &str,
        value: &SNode,
        span: Span,
    ) {
        let Node::ListLiteral(items) = &value.node else {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "`{attr_name}({key}: ...)` must be a list of strings, dotted trigger ids, or schedule(...)"
                ),
                span,
            );
            return;
        };
        if items.iter().any(|item| !is_trigger_spec(&item.node)) {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "`{attr_name}({key}: ...)` must contain only strings, dotted trigger ids, or schedule(...)"
                ),
                span,
            );
        }
    }

    fn expect_budget_dict(&mut self, attr_name: &str, key: &str, value: &SNode, span: Span) {
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                format!("`{attr_name}({key}: ...)` must be a dict of budget fields"),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "budget field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            self.expect_budget_value(attr_name, field_name, &entry.value, entry.value.span);
        }
    }

    fn expect_step_retry_dict(&mut self, value: &SNode, span: Span) {
        let Node::DictLiteral(entries) = &value.node else {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@step(retry: ...)` must be a dict such as `{ max_attempts: 3 }`".to_string(),
                span,
            );
            return;
        };
        for entry in entries {
            let Some(field_name) = attr_key_name(&entry.key.node) else {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@step(retry: ...)` field names must be strings or identifiers".to_string(),
                    entry.key.span,
                );
                continue;
            };
            match field_name {
                "max_attempts" => {
                    if !matches!(entry.value.node, Node::IntLiteral(i) if i >= 1) {
                        self.warning_at(
                            Code::InvalidAttributeArgument,
                            "`@step(retry: { max_attempts: ... })` must be a positive integer"
                                .to_string(),
                            entry.value.span,
                        );
                    }
                }
                other => {
                    self.warning_at(
                        Code::InvalidAttributeArgument,
                        format!(
                            "unknown `@step(retry: ...)` field `{other}`; expected `max_attempts`"
                        ),
                        entry.key.span,
                    );
                }
            }
        }
    }

    fn expect_budget_value(&mut self, attr_name: &str, key: &str, value: &SNode, span: Span) {
        const NUMBER_KEYS: &[&str] = &[
            "daily_usd",
            "hourly_usd",
            "run_usd",
            "max_tokens",
            "frontier_escalations",
            "max_autonomous_decisions_per_hour",
            "max_autonomous_decisions_per_day",
        ];
        const STRING_KEYS: &[&str] = &["on_exhausted", "on_budget_exhausted"];
        if NUMBER_KEYS.contains(&key) {
            if !matches!(value.node, Node::IntLiteral(_) | Node::FloatLiteral(_)) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("`{attr_name}({key}: ...)` must be a number"),
                    span,
                );
            }
        } else if STRING_KEYS.contains(&key) {
            self.expect_symbol_like(attr_name, key, value, span);
        } else {
            self.warning_at(Code::InvalidAttributeArgument,
                format!(
                    "unknown `{attr_name}` budget field `{key}`; expected one of {NUMBER_KEYS:?} or {STRING_KEYS:?}"
                ),
                span,
            );
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
                    Code::InvalidAttributeArgument,
                    "`@archivist(...)` arguments must be named (e.g. \
                     `evidence: [...], confidence: 0.9`)"
                        .to_string(),
                    arg.span,
                );
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
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
                            Code::InvalidAttributeArgument,
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
                Code::InvalidAttributeArgument,
                "`@archivist(...)` should declare `evidence: [...]` so \
                 predicates can be audited"
                    .to_string(),
                attr.span,
            );
        }
    }
}

fn attr_key_name(node: &Node) -> Option<&str> {
    match node {
        Node::Identifier(name) | Node::StringLiteral(name) | Node::RawStringLiteral(name) => {
            Some(name.as_str())
        }
        _ => None,
    }
}

fn is_symbol_like(node: &Node) -> bool {
    matches!(
        node,
        Node::Identifier(_) | Node::StringLiteral(_) | Node::RawStringLiteral(_)
    )
}

fn symbol_like_value(node: &Node) -> Option<&str> {
    match node {
        Node::Identifier(value) | Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            Some(value.as_str())
        }
        _ => None,
    }
}

fn dict_entry_key_str(key: &SNode) -> Option<String> {
    symbol_like_value(&key.node).map(str::to_string)
}

fn is_trigger_spec(node: &Node) -> bool {
    if is_symbol_like(node) {
        return true;
    }
    matches!(
        node,
        Node::FunctionCall { name, args, .. }
            if name == "schedule" && args.len() == 1 && is_symbol_like(&args[0].node)
    )
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
    collapse_members_opt(collected, TypeExpr::Union)
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
    collapse_members_opt(matched, TypeExpr::Union)
}
