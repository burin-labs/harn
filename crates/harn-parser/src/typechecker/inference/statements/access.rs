use super::*;

impl TypeChecker {
    pub(super) fn check_generic_method_bound(
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
    pub(super) fn check_property_access(
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
        if !self.is_strict_access_source(object, &raw, scope) {
            return;
        }
        // `nil` and `unknown` receivers fail identically for any access
        // form, so route them through the shared diagnosis. Once it fires,
        // the field-existence checks below would be redundant noise.
        if self.diagnose_nil_or_unknown_receiver(
            &resolved,
            AccessForm::Property(property),
            span,
            optional,
        ) {
            return;
        }
        match &resolved {
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

    /// Decide whether an access (`.`, `[]`, or `.()`) should take the
    /// strict diagnostic path. The "loose" case is the ambient dict-literal
    /// idiom (`let d = {a: 1}; d.missing` returns nil at runtime) and the
    /// unannotated `var x = nil; …; x.field` widening pattern — both rely
    /// on the runtime's silent-nil behavior. Strict diagnostics fire when
    /// the type came from a real contract: a written annotation, a named
    /// alias / struct / enum, a struct or function-call return, or any
    /// non-identifier expression (a call chain, literal, etc.). Shared by
    /// property, subscript, and method-receiver checks so all three honour
    /// the same boundary.
    pub(super) fn is_strict_access_source(
        &self,
        object: &SNode,
        raw: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        match &object.node {
            Node::Identifier(name) => {
                scope.is_annotated(name) || self.is_named_contract_type(raw, scope)
            }
            _ => true,
        }
    }

    /// True if `ty` carries a real type contract (named struct, named
    /// type alias, or a generic instantiation thereof). Used to decide
    /// whether an Identifier whose type was *inferred* (no annotation)
    /// should still take the strict property-access path. A bare
    /// `Shape(...)` returns false — that almost always came from a dict
    /// literal and historically tolerates `.missing` returning nil.
    pub(super) fn is_named_contract_type(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
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
    pub(super) fn check_struct_property(
        &mut self,
        name: &str,
        property: &str,
        scope: &TypeScope,
        span: Span,
    ) {
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
    pub(super) fn check_nilable_property_access(
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
        self.emit_nilable_access_error(AccessForm::Property(property), ty, span);
    }

    /// Diagnose a statically-`nil` or `unknown` receiver for any access
    /// form. Returns `true` when a diagnostic was emitted so the caller can
    /// skip the access-kind-specific checks (field existence, etc.) that
    /// would then be redundant. Optional access (`?.`, `?[]`, `?.()`)
    /// suppresses the `nil` diagnostic — that is exactly what those
    /// operators are for — but an `unknown` receiver is still flagged
    /// because `?.` only guards `nil`, not a non-shape concrete value.
    pub(super) fn diagnose_nil_or_unknown_receiver(
        &mut self,
        resolved: &TypeExpr,
        form: AccessForm,
        span: Span,
        optional: bool,
    ) -> bool {
        match resolved {
            TypeExpr::Named(name) if name == "nil" => {
                if optional {
                    return false;
                }
                self.error_at_with_help(
                    Code::InvalidOptionalAccess,
                    format!(
                        "cannot access {} on `nil`; the value is statically known to be nil here",
                        form.subject()
                    ),
                    span,
                    format!(
                        "use {}, or narrow the value with a `!= nil` guard {}",
                        form.optional_hint(),
                        form.guard_clause()
                    ),
                );
                true
            }
            TypeExpr::Named(name) if name == "unknown" => {
                self.warning_at_with_help(
                    Code::UnknownField,
                    format!(
                        "{} on an `unknown` value will fail at runtime if the value is not {}",
                        form.unknown_label(),
                        form.unknown_requirement()
                    ),
                    span,
                    "narrow with `is_a`/`type_of`, validate with `assert_shape`, or annotate with a shape type before accessing fields"
                        .to_string(),
                );
                true
            }
            _ => false,
        }
    }

    /// Emit the shared "value may be nil at runtime" error for a `T | nil`
    /// receiver, phrased for whichever access form was written.
    pub(super) fn emit_nilable_access_error(
        &mut self,
        form: AccessForm,
        ty: &TypeExpr,
        span: Span,
    ) {
        self.error_at_with_help(
            Code::InvalidOptionalAccess,
            format!(
                "cannot access {} on nilable type `{}`; the value may be nil at runtime",
                form.subject(),
                format_type(ty)
            ),
            span,
            format!(
                "use {}, or narrow the value with a `!= nil` guard to drop the nil arm",
                form.optional_hint()
            ),
        );
    }

    /// True when `ty` is a `T | nil` union of any width. Subscript and
    /// method-call receivers have no per-field semantics, so any nil arm
    /// is grounds to warn (unlike property access, which first checks
    /// whether the non-nil arm even admits the field).
    pub(super) fn is_nilable_union(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        matches!(ty, TypeExpr::Union(members)
            if members.iter().any(|m| self.type_is_nil(m, scope)))
    }

    /// Subscript (`obj[idx]`) nil-safety, mirroring `check_property_access`:
    /// a statically-`nil`, may-be-`nil`, or `unknown` receiver is diagnosed
    /// with the same guidance, pointing at `?[…]` instead of `?.`.
    pub(super) fn check_subscript_access(
        &mut self,
        object: &SNode,
        scope: &TypeScope,
        span: Span,
        optional: bool,
    ) {
        let Some(raw) = self.infer_type(object, scope) else {
            return;
        };
        let resolved = self.resolve_alias(&raw, scope);
        if !self.is_strict_access_source(object, &raw, scope) {
            return;
        }
        if self.diagnose_nil_or_unknown_receiver(&resolved, AccessForm::Subscript, span, optional) {
            return;
        }
        if !optional && self.is_nilable_union(&resolved, scope) {
            self.emit_nilable_access_error(AccessForm::Subscript, &resolved, span);
        }
    }

    /// Method-call (`obj.name(..)`) receiver nil-safety, mirroring
    /// `check_property_access`: a statically-`nil`, may-be-`nil`, or
    /// `unknown` receiver is diagnosed before the args/bound checks run.
    pub(super) fn check_method_receiver(
        &mut self,
        object: &SNode,
        method: &str,
        scope: &TypeScope,
        span: Span,
        optional: bool,
    ) {
        let Some(raw) = self.infer_type(object, scope) else {
            return;
        };
        let resolved = self.resolve_alias(&raw, scope);
        if !self.is_strict_access_source(object, &raw, scope) {
            return;
        }
        if self.diagnose_nil_or_unknown_receiver(
            &resolved,
            AccessForm::Method(method),
            span,
            optional,
        ) {
            return;
        }
        if !optional && self.is_nilable_union(&resolved, scope) {
            self.emit_nilable_access_error(AccessForm::Method(method), &resolved, span);
        }
    }

    pub(super) fn check_strict_untyped_access(
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

    /// Root identifier of an assignment target that is a property/subscript
    /// path (`o.a`, `o.a[i]`, `o?.a`). Returns `None` for a bare identifier
    /// target (handled separately) or an unrooted target.
    pub(super) fn assignment_target_root(target: &SNode) -> Option<&str> {
        match &target.node {
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. } => {
                let mut cur = object;
                loop {
                    match &cur.node {
                        Node::Identifier(name) => return Some(name.as_str()),
                        Node::PropertyAccess { object, .. }
                        | Node::OptionalPropertyAccess { object, .. }
                        | Node::SubscriptAccess { object, .. }
                        | Node::OptionalSubscriptAccess { object, .. } => cur = object,
                        _ => return None,
                    }
                }
            }
            _ => None,
        }
    }
}
