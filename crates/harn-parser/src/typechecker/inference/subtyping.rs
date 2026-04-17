//! Subtype checking, alias resolution, and Go-style interface satisfaction.
//!
//! `types_compatible` and `types_compatible_at` are the workhorse subtype
//! checks called from every assignment / argument / return position.
//! `resolve_alias` flattens named type aliases before subtype dispatch so
//! aliases never reach the match arms in `types_compatible_at`.
//! `satisfies_interface` / `interface_mismatch_reason` implement implicit
//! interface satisfaction by structurally matching impl-block method
//! signatures against the interface declaration.

use std::collections::BTreeMap;

use crate::ast::*;

use super::super::format::format_type;
use super::super::scope::{Polarity, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    /// Check if a type satisfies an interface (Go-style implicit satisfaction).
    /// A type satisfies an interface if its impl block has all the required methods.
    pub(in crate::typechecker) fn satisfies_interface(
        &self,
        type_name: &str,
        interface_name: &str,
        interface_bindings: &BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> bool {
        self.interface_mismatch_reason(type_name, interface_name, interface_bindings, scope)
            .is_none()
    }

    /// Return a detailed reason why a type does not satisfy an interface, or None
    /// if it does satisfy it. Used for producing actionable warning messages.
    pub(in crate::typechecker) fn interface_mismatch_reason(
        &self,
        type_name: &str,
        interface_name: &str,
        interface_bindings: &BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Option<String> {
        let interface_info = match scope.get_interface(interface_name) {
            Some(info) => info,
            None => return Some(format!("interface '{}' not found", interface_name)),
        };
        let impl_methods = match scope.get_impl_methods(type_name) {
            Some(methods) => methods,
            None => {
                if interface_info.methods.is_empty() {
                    return None;
                }
                let names: Vec<_> = interface_info
                    .methods
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect();
                return Some(format!("missing method(s): {}", names.join(", ")));
            }
        };
        let mut bindings = interface_bindings.clone();
        let associated_type_names: std::collections::BTreeSet<String> = interface_info
            .associated_types
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        for iface_method in &interface_info.methods {
            let iface_params: Vec<_> = iface_method
                .params
                .iter()
                .filter(|p| p.name != "self")
                .collect();
            let iface_param_count = iface_params.len();
            let matching_impl = impl_methods.iter().find(|im| im.name == iface_method.name);
            let impl_method = match matching_impl {
                Some(m) => m,
                None => {
                    return Some(format!("missing method '{}'", iface_method.name));
                }
            };
            if impl_method.param_count != iface_param_count {
                return Some(format!(
                    "method '{}' has {} parameter(s), expected {}",
                    iface_method.name, impl_method.param_count, iface_param_count
                ));
            }
            // Check parameter types where both sides specify them
            for (i, iface_param) in iface_params.iter().enumerate() {
                if let (Some(expected), Some(actual)) = (
                    &iface_param.type_expr,
                    impl_method.param_types.get(i).and_then(|t| t.as_ref()),
                ) {
                    if let Err(message) = Self::extract_type_bindings(
                        expected,
                        actual,
                        &associated_type_names,
                        &mut bindings,
                    ) {
                        return Some(message);
                    }
                    let expected = Self::apply_type_bindings(expected, &bindings);
                    if !self.types_compatible(&expected, actual, scope) {
                        return Some(format!(
                            "method '{}' parameter {} has type '{}', expected '{}'",
                            iface_method.name,
                            i + 1,
                            format_type(actual),
                            format_type(&expected),
                        ));
                    }
                }
            }
            // Check return type where both sides specify it
            if let (Some(expected_ret), Some(actual_ret)) =
                (&iface_method.return_type, &impl_method.return_type)
            {
                if let Err(message) = Self::extract_type_bindings(
                    expected_ret,
                    actual_ret,
                    &associated_type_names,
                    &mut bindings,
                ) {
                    return Some(message);
                }
                let expected_ret = Self::apply_type_bindings(expected_ret, &bindings);
                if !self.types_compatible(&expected_ret, actual_ret, scope) {
                    return Some(format!(
                        "method '{}' returns '{}', expected '{}'",
                        iface_method.name,
                        format_type(actual_ret),
                        format_type(&expected_ret),
                    ));
                }
            }
        }
        for (assoc_name, default_type) in &interface_info.associated_types {
            if let (Some(default_type), Some(actual)) = (default_type, bindings.get(assoc_name)) {
                let expected = Self::apply_type_bindings(default_type, &bindings);
                if !self.types_compatible(&expected, actual, scope) {
                    return Some(format!(
                        "associated type '{}' resolves to '{}', expected '{}'",
                        assoc_name,
                        format_type(actual),
                        format_type(&expected),
                    ));
                }
            }
        }
        None
    }

    pub(in crate::typechecker) fn types_compatible(
        &self,
        expected: &TypeExpr,
        actual: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        self.types_compatible_at(Polarity::Covariant, expected, actual, scope)
    }

    /// Polarity-aware subtype check.
    ///
    /// - `Covariant`: `actual <: expected` under ordinary widening
    ///   (this is the public entry point behavior).
    /// - `Contravariant`: swaps the arguments and recurses covariantly.
    /// - `Invariant`: both directions must hold covariantly. This
    ///   disables the asymmetric numeric widening (`int <: float`)
    ///   that we rely on in covariant positions, so mutable container
    ///   slots do not accept a narrower element type.
    pub(in crate::typechecker) fn types_compatible_at(
        &self,
        polarity: Polarity,
        expected: &TypeExpr,
        actual: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        match polarity {
            Polarity::Covariant => {}
            Polarity::Contravariant => {
                return self.types_compatible_at(Polarity::Covariant, actual, expected, scope);
            }
            Polarity::Invariant => {
                return self.types_compatible_at(Polarity::Covariant, expected, actual, scope)
                    && self.types_compatible_at(Polarity::Covariant, actual, expected, scope);
            }
        }

        // From here on we are in the covariant case.
        if Self::is_wildcard_type(expected) || Self::is_wildcard_type(actual) {
            return true;
        }
        // Generic type parameters match anything.
        if let TypeExpr::Named(name) = expected {
            if scope.is_generic_type_param(name) {
                return true;
            }
        }
        if let TypeExpr::Named(name) = actual {
            if scope.is_generic_type_param(name) {
                return true;
            }
        }
        let expected = self.resolve_alias(expected, scope);
        let actual = self.resolve_alias(actual, scope);

        // Interface satisfaction: if expected names an interface, check method compatibility.
        if let Some(iface_name) = Self::base_type_name(&expected) {
            if let Some(interface_info) = scope.get_interface(iface_name) {
                let mut interface_bindings = BTreeMap::new();
                if let TypeExpr::Applied { args, .. } = &expected {
                    for (type_param, arg) in interface_info.type_params.iter().zip(args.iter()) {
                        interface_bindings.insert(type_param.name.clone(), arg.clone());
                    }
                }
                if let Some(type_name) = Self::base_type_name(&actual) {
                    return self.satisfies_interface(
                        type_name,
                        iface_name,
                        &interface_bindings,
                        scope,
                    );
                }
                return false;
            }
        }

        match (&expected, &actual) {
            // never is the bottom type: assignable to any type.
            (_, TypeExpr::Never) => true,
            // Nothing is assignable to never (except never itself, handled above).
            (TypeExpr::Never, _) => false,
            // `any` is the top type (escape hatch): every type flows into `any`,
            // and `any` flows back out to any concrete type with no narrowing required.
            (TypeExpr::Named(n), _) if n == "any" => true,
            (_, TypeExpr::Named(n)) if n == "any" => true,
            // `unknown` is the safe top: every type flows into `unknown`, but
            // `unknown` only flows back out to `unknown` itself (or `any`, via the
            // arm above). Concrete uses require narrowing via `type_of` / `schema_is`.
            (TypeExpr::Named(n), _) if n == "unknown" => true,
            // Reverse direction: `unknown` is not assignable to anything concrete.
            // The `(_, Named("unknown"))` arm deliberately falls through to `=> false`
            // below, producing a "expected T, got unknown" diagnostic.
            (TypeExpr::Named(a), TypeExpr::Named(b)) => a == b || (a == "float" && b == "int"),
            (TypeExpr::Named(a), TypeExpr::Applied { name: b, .. })
            | (TypeExpr::Applied { name: a, .. }, TypeExpr::Named(b)) => a == b,
            (
                TypeExpr::Applied {
                    name: expected_name,
                    args: expected_args,
                },
                TypeExpr::Applied {
                    name: actual_name,
                    args: actual_args,
                },
            ) => {
                if expected_name != actual_name || expected_args.len() != actual_args.len() {
                    return false;
                }
                // Consult the declared variance for each type parameter
                // of this constructor. User-declared generics default
                // to `Invariant` for any parameter without a marker,
                // which is enforced by the per-TypeParam default set in
                // the parser and AST. Unknown constructors (e.g. an
                // inferred schema-driven wrapper whose decl has not
                // been registered yet) fall back to invariance — that
                // is strictly safer than the previous implicit
                // covariance.
                let variances = scope.variance_of(expected_name);
                for (idx, (expected_arg, actual_arg)) in
                    expected_args.iter().zip(actual_args.iter()).enumerate()
                {
                    let child_variance = variances
                        .as_ref()
                        .and_then(|v| v.get(idx).copied())
                        .unwrap_or(Variance::Invariant);
                    let arg_polarity = Polarity::Covariant.compose(child_variance);
                    if !self.types_compatible_at(arg_polarity, expected_arg, actual_arg, scope) {
                        return false;
                    }
                }
                true
            }
            // Union-to-Union: every member of actual must be compatible with
            // at least one member of expected.
            (TypeExpr::Union(exp_members), TypeExpr::Union(act_members)) => {
                act_members.iter().all(|am| {
                    exp_members
                        .iter()
                        .any(|em| self.types_compatible(em, am, scope))
                })
            }
            (TypeExpr::Union(members), actual_type) => members
                .iter()
                .any(|m| self.types_compatible(m, actual_type, scope)),
            (expected_type, TypeExpr::Union(members)) => members
                .iter()
                .all(|m| self.types_compatible(expected_type, m, scope)),
            (TypeExpr::Shape(_), TypeExpr::Named(n)) if n == "dict" => true,
            (TypeExpr::Named(n), TypeExpr::Shape(_)) if n == "dict" => true,
            (TypeExpr::Shape(ef), TypeExpr::Shape(af)) => ef.iter().all(|expected_field| {
                if expected_field.optional {
                    return true;
                }
                af.iter().any(|actual_field| {
                    actual_field.name == expected_field.name
                        && self.types_compatible(
                            &expected_field.type_expr,
                            &actual_field.type_expr,
                            scope,
                        )
                })
            }),
            // dict<K, V> expected, Shape actual → all field values must match V
            (TypeExpr::DictType(ek, ev), TypeExpr::Shape(af)) => {
                let keys_ok = matches!(ek.as_ref(), TypeExpr::Named(n) if n == "string");
                keys_ok
                    && af
                        .iter()
                        .all(|f| self.types_compatible(ev, &f.type_expr, scope))
            }
            // Shape expected, dict<K, V> actual → gradual: allow since dict may have the fields
            (TypeExpr::Shape(_), TypeExpr::DictType(_, _)) => true,
            // list<T> is invariant: the element type must match exactly
            // (no int→float widening) because lists are mutable
            // (`push`, index assignment). Covariant lists are unsound
            // on write — a `list<int>` flowing into a `list<float>`
            // slot would let a `float` be pushed and later observed
            // as an `int`.
            (TypeExpr::List(expected_inner), TypeExpr::List(actual_inner)) => {
                self.types_compatible_at(Polarity::Invariant, expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::List(_)) if n == "list" => true,
            (TypeExpr::List(_), TypeExpr::Named(n)) if n == "list" => true,
            // iter<T> is covariant: it is a read-only sequence with no
            // mutating projection, so widening its element type is
            // sound.
            (TypeExpr::Iter(expected_inner), TypeExpr::Iter(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::Iter(_)) if n == "iter" => true,
            (TypeExpr::Iter(_), TypeExpr::Named(n)) if n == "iter" => true,
            // dict<K, V> is invariant in both K and V: dicts are
            // mutable (key/value assignment). See the `list` comment
            // above for the soundness argument.
            (TypeExpr::DictType(ek, ev), TypeExpr::DictType(ak, av)) => {
                self.types_compatible_at(Polarity::Invariant, ek, ak, scope)
                    && self.types_compatible_at(Polarity::Invariant, ev, av, scope)
            }
            (TypeExpr::Named(n), TypeExpr::DictType(_, _)) if n == "dict" => true,
            (TypeExpr::DictType(_, _), TypeExpr::Named(n)) if n == "dict" => true,
            // FnType subtyping: parameters are contravariant (an
            // `fn(float)` can stand in for an expected `fn(int)`
            // because floats contain ints); return types remain
            // covariant. Previously params were checked covariantly,
            // which let `fn(int)` stand in for `fn(float)` — an
            // unsound callback substitution.
            (
                TypeExpr::FnType {
                    params: ep,
                    return_type: er,
                },
                TypeExpr::FnType {
                    params: ap,
                    return_type: ar,
                },
            ) => {
                ep.len() == ap.len()
                    && ep.iter().zip(ap.iter()).all(|(e, a)| {
                        self.types_compatible_at(Polarity::Contravariant, e, a, scope)
                    })
                    && self.types_compatible(er, ar, scope)
            }
            // FnType is compatible with Named("closure") for backward compat
            (TypeExpr::FnType { .. }, TypeExpr::Named(n)) if n == "closure" => true,
            (TypeExpr::Named(n), TypeExpr::FnType { .. }) if n == "closure" => true,
            // Literal types: identical literals match; a literal flows
            // into its base type (`"pass"` → `string`); and — as a gradual
            // concession — a base type flows into a literal-typed slot
            // (`string` → `"pass" | "fail"`) so that existing
            // string/int-typed data can populate discriminated unions
            // without per-call-site widening. Runtime schema validation
            // (emitted for typed params and `schema_is`/`schema_expect`
            // guards) catches values that violate the literal set.
            (TypeExpr::LitString(a), TypeExpr::LitString(b)) => a == b,
            (TypeExpr::LitInt(a), TypeExpr::LitInt(b)) => a == b,
            (TypeExpr::Named(n), TypeExpr::LitString(_)) if n == "string" => true,
            (TypeExpr::Named(n), TypeExpr::LitInt(_)) if n == "int" || n == "float" => true,
            (TypeExpr::LitString(_), TypeExpr::Named(n)) if n == "string" => true,
            (TypeExpr::LitInt(_), TypeExpr::Named(n)) if n == "int" => true,
            _ => false,
        }
    }

    pub(in crate::typechecker) fn resolve_alias<'a>(
        &self,
        ty: &'a TypeExpr,
        scope: &'a TypeScope,
    ) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => {
                if let Some(resolved) = scope.resolve_type(name) {
                    return self.resolve_alias(resolved, scope);
                }
                ty.clone()
            }
            TypeExpr::Union(types) => TypeExpr::Union(
                types
                    .iter()
                    .map(|ty| self.resolve_alias(ty, scope))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.resolve_alias(&field.type_expr, scope),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            TypeExpr::List(inner) => TypeExpr::List(Box::new(self.resolve_alias(inner, scope))),
            TypeExpr::Iter(inner) => TypeExpr::Iter(Box::new(self.resolve_alias(inner, scope))),
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(self.resolve_alias(key, scope)),
                Box::new(self.resolve_alias(value, scope)),
            ),
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|param| self.resolve_alias(param, scope))
                    .collect(),
                return_type: Box::new(self.resolve_alias(return_type, scope)),
            },
            TypeExpr::Applied { name, args } => TypeExpr::Applied {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.resolve_alias(arg, scope))
                    .collect(),
            },
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(s) => TypeExpr::LitString(s.clone()),
            TypeExpr::LitInt(v) => TypeExpr::LitInt(*v),
        }
    }
}
