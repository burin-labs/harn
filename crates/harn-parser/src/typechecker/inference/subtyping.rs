//! Subtype checking, alias resolution, and Go-style interface satisfaction.
//!
//! `types_compatible` and `types_compatible_at` are the workhorse subtype
//! checks called from every assignment / argument / return position.
//! `resolve_alias` flattens named type aliases before subtype dispatch so
//! aliases never reach the match arms in `types_compatible_at`.
//! `interface_mismatch_reason_for_type` implements implicit interface
//! satisfaction by structurally matching impl-block method signatures against
//! the interface declaration.

use std::collections::{BTreeMap, HashSet};

use crate::ast::*;

use super::super::format::format_type;
use super::super::scope::{Polarity, TypeScope};
use super::super::union::collapse_members;
use super::super::TypeChecker;

/// RAII pop for the coinductive recursion guard in
/// [`TypeChecker::types_compatible_at`]. Holding the guard by value means every
/// exit path — including the many early `return`s in that function — removes
/// the pushed `(expected, actual)` pair when the borrow ends.
struct SubtypeCycleGuard<'a>(&'a std::cell::RefCell<Vec<(TypeExpr, TypeExpr)>>);

impl Drop for SubtypeCycleGuard<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().pop();
    }
}

/// Whether an open record's trailing row tails are *gradual* — a `dict`,
/// `dict<K, V>`, or `any` tail stands for unknown fields, so a required
/// expected field absent from the known fields may still be present at
/// runtime. An abstract row variable (a `Named` generic param) is **not**
/// gradual: absence reasoning needs a closed, non-gradual shape.
fn open_shape_tail_is_gradual(rests: &[TypeExpr]) -> bool {
    rests.iter().any(|r| {
        matches!(r, TypeExpr::DictType(..))
            || matches!(r, TypeExpr::Named(n) if n == "dict" || n == "any" || n == "_")
    })
}

impl TypeChecker {
    /// Return a detailed reason why a type does not satisfy an interface, or
    /// `None` if it does satisfy it. Used for actionable diagnostics.
    pub(in crate::typechecker) fn interface_mismatch_reason_for_type(
        &self,
        concrete_type: &TypeExpr,
        bound: &TypeExpr,
        scope: &TypeScope,
    ) -> Option<String> {
        let Some(type_name) = Self::base_type_name(concrete_type) else {
            return Some("only named types can satisfy interfaces".into());
        };
        let Some(interface_name) = Self::base_type_name(bound) else {
            return Some(format!("'{}' is not an interface type", format_type(bound)));
        };

        let interface_bindings = self.interface_type_bindings(bound, interface_name, scope);
        let impl_bindings = self.impl_type_bindings(concrete_type, type_name, scope);
        self.interface_mismatch_reason_with_bindings(
            type_name,
            interface_name,
            &interface_bindings,
            &impl_bindings,
            scope,
        )
    }

    fn interface_type_bindings(
        &self,
        interface_type: &TypeExpr,
        interface_name: &str,
        scope: &TypeScope,
    ) -> BTreeMap<String, TypeExpr> {
        let mut bindings = BTreeMap::new();
        let Some(interface_info) = scope.get_interface(interface_name) else {
            return bindings;
        };
        if let TypeExpr::Applied { args, .. } = interface_type {
            for (type_param, arg) in interface_info.type_params.iter().zip(args.iter()) {
                bindings.insert(type_param.name.clone(), arg.clone());
            }
        }
        bindings
    }

    fn impl_type_bindings(
        &self,
        concrete_type: &TypeExpr,
        type_name: &str,
        scope: &TypeScope,
    ) -> BTreeMap<String, TypeExpr> {
        let mut bindings = BTreeMap::new();
        let TypeExpr::Applied { args, .. } = concrete_type else {
            return bindings;
        };
        if let Some(struct_info) = scope.get_struct(type_name) {
            for (type_param, arg) in struct_info.type_params.iter().zip(args.iter()) {
                bindings.insert(type_param.name.clone(), arg.clone());
            }
        } else if let Some(enum_info) = scope.get_enum(type_name) {
            for (type_param, arg) in enum_info.type_params.iter().zip(args.iter()) {
                bindings.insert(type_param.name.clone(), arg.clone());
            }
        }
        bindings
    }

    fn interface_mismatch_reason_with_bindings(
        &self,
        type_name: &str,
        interface_name: &str,
        interface_bindings: &BTreeMap<String, TypeExpr>,
        impl_bindings: &BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Option<String> {
        let interface_info = match scope.get_interface(interface_name) {
            Some(info) => info,
            None => return Some(format!("interface '{interface_name}' not found")),
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
                    let actual = Self::apply_type_bindings(actual, impl_bindings);
                    if let Err(message) = Self::extract_type_bindings(
                        expected,
                        &actual,
                        &associated_type_names,
                        &mut bindings,
                    ) {
                        return Some(message);
                    }
                    let expected = Self::apply_type_bindings(expected, &bindings);
                    if !self.types_compatible(&expected, &actual, scope) {
                        return Some(format!(
                            "method '{}' parameter {} has type '{}', expected '{}'",
                            iface_method.name,
                            i + 1,
                            format_type(&actual),
                            format_type(&expected),
                        ));
                    }
                }
            }
            // Check return type where both sides specify it
            if let (Some(expected_ret), Some(actual_ret)) =
                (&iface_method.return_type, &impl_method.return_type)
            {
                let actual_ret = Self::apply_type_bindings(actual_ret, impl_bindings);
                if let Err(message) = Self::extract_type_bindings(
                    expected_ret,
                    &actual_ret,
                    &associated_type_names,
                    &mut bindings,
                ) {
                    return Some(message);
                }
                let expected_ret = Self::apply_type_bindings(expected_ret, &bindings);
                if !self.types_compatible(&expected_ret, &actual_ret, scope) {
                    return Some(format!(
                        "method '{}' returns '{}', expected '{}'",
                        iface_method.name,
                        format_type(&actual_ret),
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

    fn where_bound_covers_interface(
        &self,
        bound: &TypeExpr,
        expected: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        let Some(bound_name) = Self::base_type_name(bound) else {
            return false;
        };
        let Some(expected_name) = Self::base_type_name(expected) else {
            return false;
        };
        if bound_name != expected_name {
            return false;
        }
        match (bound, expected) {
            (
                TypeExpr::Applied {
                    args: bound_args, ..
                },
                TypeExpr::Applied {
                    args: expected_args,
                    ..
                },
            ) => {
                bound_args.len() == expected_args.len()
                    && expected_args
                        .iter()
                        .zip(bound_args.iter())
                        .all(|(expected, bound)| {
                            self.types_compatible(expected, bound, scope)
                                && self.types_compatible(bound, expected, scope)
                        })
            }
            (TypeExpr::Applied { .. }, TypeExpr::Named(_)) => true,
            (TypeExpr::Named(_), TypeExpr::Applied { .. }) => false,
            _ => true,
        }
    }

    pub(in crate::typechecker) fn types_compatible(
        &self,
        expected: &TypeExpr,
        actual: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        self.types_compatible_at(Polarity::Covariant, expected, actual, scope)
    }

    /// Maximum structural nesting depth of a subtype check before the
    /// coinductive backstop assumes compatibility. Recursive types normally
    /// terminate via reflexivity or the repeated-pair guard; this only fires on
    /// pathological growth and sits far above any realistic hand-written type.
    const SUBTYPE_RECURSION_LIMIT: usize = 256;

    /// Types whose subtype check recurses into components (or resolves through a
    /// user alias) and can therefore close a cycle through a recursive alias.
    /// A `Named` referring to a built-in scalar/gradual type resolves to itself
    /// and cannot recurse, so it stays on the fast path alongside literals and
    /// `never`; every other `Named` is a potential (possibly recursive) alias.
    /// This gates the coinductive guard in [`Self::types_compatible_at`], which
    /// keys on the pre-resolution pair.
    fn type_can_cycle(ty: &TypeExpr) -> bool {
        match ty {
            TypeExpr::Named(name) => !matches!(
                name.as_str(),
                "int"
                    | "float"
                    | "string"
                    | "bool"
                    | "nil"
                    | "list"
                    | "dict"
                    | "set"
                    | "closure"
                    | "bytes"
                    | "any"
                    | "unknown"
                    | "never"
                    | "number"
                    | "_"
            ),
            TypeExpr::Shape(_)
            | TypeExpr::OpenShape { .. }
            | TypeExpr::List(_)
            | TypeExpr::DictType(..)
            | TypeExpr::Applied { .. }
            | TypeExpr::Union(_)
            | TypeExpr::Intersection(_)
            | TypeExpr::Iter(_)
            | TypeExpr::Generator(_)
            | TypeExpr::Stream(_)
            | TypeExpr::FnType { .. }
            | TypeExpr::Owned(_) => true,
            TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => false,
        }
    }

    /// Unfold only the alias chain at the root of `ty`.
    ///
    /// Subtyping already descends through compound types recursively. Fully
    /// normalizing a recursive alias at every descent turns a finite value type
    /// into a progressively deeper tree, so the coinductive pair guard never
    /// sees the same pair twice. Root-chain unfolding preserves aliases inside
    /// the first structural body while still following ordinary forwarding
    /// aliases (`Handle -> Binding -> {shape}`) to their meaningful root.
    fn unfold_alias_root(&self, ty: &TypeExpr, scope: &TypeScope) -> TypeExpr {
        let mut current = ty.clone();
        let mut visited = HashSet::new();
        loop {
            current = match &current {
                TypeExpr::Named(name) if name == "number" => {
                    return TypeExpr::Union(vec![
                        TypeExpr::Named("int".to_string()),
                        TypeExpr::Named("float".to_string()),
                    ]);
                }
                TypeExpr::Named(name) => {
                    if !visited.insert(name.clone()) {
                        return current;
                    }
                    let Some(resolved) = scope.resolve_type(name) else {
                        return current;
                    };
                    resolved.clone()
                }
                TypeExpr::Applied { name, args } => {
                    if !visited.insert(name.clone()) {
                        return current;
                    }
                    let Some(info) = scope.resolve_type_alias(name) else {
                        return current;
                    };
                    if info.type_params.len() != args.len() {
                        return current;
                    }
                    let args: Vec<TypeExpr> = args
                        .iter()
                        .map(|arg| {
                            let resolved = self.resolve_alias(arg, scope);
                            if matches!(resolved, TypeExpr::Union(_)) {
                                resolved
                            } else {
                                arg.clone()
                            }
                        })
                        .collect();
                    let names: Vec<String> = info
                        .type_params
                        .iter()
                        .map(|param| param.name.clone())
                        .collect();
                    instantiate_alias_distributive(&info.body, &names, &args)
                }
                _ => return current,
            };
        }
    }

    /// Check a record's explicit fields against an actual record's known
    /// fields (the shared core of shape / open-shape subtyping). Each expected
    /// field must be present-and-compatible, be optional, or — when missing —
    /// be covered by a gradual actual tail. An explicit `nil` satisfies an
    /// optional field (matching the closed-shape rule).
    fn shape_fields_satisfied(
        &self,
        expected_fields: &[ShapeField],
        actual_fields: &[ShapeField],
        actual_tail_gradual: bool,
        scope: &TypeScope,
    ) -> bool {
        expected_fields.iter().all(
            |ef| match actual_fields.iter().find(|f| f.name == ef.name) {
                None => ef.optional || actual_tail_gradual,
                Some(af) => {
                    if ef.optional && matches!(&af.type_expr, TypeExpr::Named(n) if n == "nil") {
                        return true;
                    }
                    self.types_compatible(&ef.type_expr, &af.type_expr, scope)
                }
            },
        )
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

        // Reflexivity: a type is always a subtype of itself. Besides being a
        // cheap fast path, this is what terminates the common recursive-type
        // comparison (`types_compatible(Tree, Tree)` where
        // `type Tree = {value: int, children: [Tree]}`): each structural step
        // re-expands the alias one level deeper, so the resolved pair grows
        // without bound, but the two sides stay *identical* the whole way down.
        // Bailing out here closes that cycle before it can recurse.
        if expected == actual {
            return true;
        }

        // Coinductive guard for recursive type aliases. A recursive alias
        // (`type Tree = {value: int, children: [Tree]}`) makes the structural
        // walk below re-resolve the alias one layer deeper at every level, so a
        // naive recursion never terminates. The guard is keyed on the
        // *pre-resolution* `(expected, actual)` pair — that is stable across the
        // cycle (`Tree` vs `Tree` recurs as `Tree` vs `Tree`), whereas the
        // resolved shapes grow without bound. A repeat means we have closed a
        // cycle through the recursion, at which point we assume compatibility
        // (equirecursive / greatest-fixpoint subtyping — the same rule
        // TypeScript and Flow use). Only alias names and compound types can
        // cycle, so scalars skip the bookkeeping and stay on the fast path.
        // `_cycle_guard` pops the pushed pair on every exit path via `Drop`.
        let _cycle_guard = if Self::type_can_cycle(expected) {
            let stack = self.subtype_cycle_guard.borrow();
            if stack.iter().any(|(e, a)| e == expected && a == actual) {
                return true;
            }
            // Depth backstop: recursive types that never present an identical or
            // repeated pair (e.g. two structurally-distinct mutually-recursive
            // aliases whose resolved forms grow in lockstep) would otherwise
            // recurse until the stack overflows. Past this generous nesting
            // depth we assume compatibility — the greatest-fixpoint answer, and
            // far deeper than any hand-written non-recursive type.
            if stack.len() >= Self::SUBTYPE_RECURSION_LIMIT {
                return true;
            }
            drop(stack);
            self.subtype_cycle_guard
                .borrow_mut()
                .push((expected.clone(), actual.clone()));
            Some(SubtypeCycleGuard(&self.subtype_cycle_guard))
        } else {
            None
        };

        let expected = self.unfold_alias_root(expected, scope);
        let actual = self.unfold_alias_root(actual, scope);

        // `owned<T>` is transparent to the underlying handle type at the type
        // boundary: the ownership marker only influences scope-exit codegen
        // and the HARN-OWN-005 leak lint, not assignment compatibility.
        // Strip both sides so an `owned<channel>` annotation accepts a bare
        // `channel`, an `owned<T>` flows into a `T` parameter, and so on.
        if let TypeExpr::Owned(inner) = &expected {
            return self.types_compatible_at(Polarity::Covariant, inner, &actual, scope);
        }
        if let TypeExpr::Owned(inner) = &actual {
            return self.types_compatible_at(Polarity::Covariant, &expected, inner, scope);
        }

        // never is the bottom type: assignable to any type.
        if matches!(actual, TypeExpr::Never) {
            return true;
        }
        // Nothing is assignable to never (except never itself, handled above).
        if matches!(expected, TypeExpr::Never) {
            return false;
        }
        // `any` is the escape hatch; `unknown` is the safe top.
        if matches!(&expected, TypeExpr::Named(n) if n == "any" || n == "unknown")
            || matches!(&actual, TypeExpr::Named(n) if n == "any")
        {
            return true;
        }

        // Interface satisfaction: if expected names an interface, check method compatibility.
        if let Some(iface_name) = Self::base_type_name(&expected) {
            if scope.get_interface(iface_name).is_some() {
                if let Some(type_name) = Self::base_type_name(&actual) {
                    if scope.is_generic_type_param(type_name)
                        && scope
                            .get_where_constraints(type_name)
                            .iter()
                            .any(|bound| self.where_bound_covers_interface(bound, &expected, scope))
                    {
                        return true;
                    }
                    return self
                        .interface_mismatch_reason_for_type(&actual, &expected, scope)
                        .is_none();
                }
                return false;
            }
        }

        // Generic type parameters are abstract, not `any`: a value of
        // unknown caller-chosen type `T` cannot flow into `int`, and an
        // `int` cannot manufacture a value for arbitrary `T`. The same
        // abstract parameter remains compatible with itself, while `any`,
        // `unknown`, `never`, and interface-bound cases were handled above.
        let expected_generic =
            matches!(&expected, TypeExpr::Named(name) if scope.is_generic_type_param(name));
        let actual_generic =
            matches!(&actual, TypeExpr::Named(name) if scope.is_generic_type_param(name));
        if expected_generic || actual_generic {
            return matches!((&expected, &actual), (TypeExpr::Named(a), TypeExpr::Named(b)) if a == b);
        }

        match (&expected, &actual) {
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
            // Intersection on the expected side: the actual must satisfy
            // every component (`actual <: A` AND `actual <: B`).
            // Intersection on the actual side: the value already satisfies
            // every component, so it flows into any expected type that one
            // of those components satisfies.
            (TypeExpr::Intersection(exp_members), TypeExpr::Intersection(act_members)) => {
                exp_members.iter().all(|em| {
                    act_members
                        .iter()
                        .any(|am| self.types_compatible(em, am, scope))
                })
            }
            (TypeExpr::Intersection(members), actual_type) => members
                .iter()
                .all(|m| self.types_compatible(m, actual_type, scope)),
            (expected_type, TypeExpr::Intersection(members)) => members
                .iter()
                .any(|m| self.types_compatible(expected_type, m, scope)),
            // A shape widens to the opaque `dict` (a shape *is* a dict with
            // known fields). The reverse is NOT sound: a bare `dict` carries no
            // field guarantees, so flowing it into a specific shape without a
            // narrow (`schema_is` / `.has()`) is exactly the hole that let
            // unvalidated `json_parse` output masquerade as a typed record.
            // `dict` now behaves like `unknown` here — the shape target requires
            // narrowing first.
            (TypeExpr::Named(n), TypeExpr::Shape(_)) if n == "dict" => true,
            // The *empty* shape `{}` is the top object type (TS/Flow `{}`): it
            // carries no field obligations, so any `dict` — indeed any object —
            // satisfies it. This is the one shape a bare `dict` may flow into
            // without narrowing, and it is what lets `let m = {}` accept a later
            // `m = json_parse(...)`. A non-empty shape still requires narrowing.
            (TypeExpr::Shape(ef), TypeExpr::Named(n)) if n == "dict" && ef.is_empty() => true,
            // Open records. Subtyping verifies only the EXPECTED side's
            // explicit fields against the actual's known fields — Harn shapes
            // are already width-subtyped, so extra actual fields (and the
            // expected's own row tail, which absorbs them) impose nothing. A
            // required expected field absent from the actual's known fields is
            // accepted only when the actual carries a *gradual* tail
            // (`dict`/`any`); an abstract row variable is not assumed to supply
            // it (per the row-poly design: absence reasoning needs a closed
            // non-gradual shape).
            (TypeExpr::OpenShape { fields: ef, .. }, TypeExpr::Shape(af)) => {
                self.shape_fields_satisfied(ef, af, false, scope)
            }
            (TypeExpr::Shape(ef), TypeExpr::OpenShape { fields: af, rests }) => {
                self.shape_fields_satisfied(ef, af, open_shape_tail_is_gradual(rests), scope)
            }
            (TypeExpr::OpenShape { fields: ef, .. }, TypeExpr::OpenShape { fields: af, rests }) => {
                self.shape_fields_satisfied(ef, af, open_shape_tail_is_gradual(rests), scope)
            }
            // Gradual map interop. An open record widens to `dict`, and — unlike
            // the closed-`Shape` case — a bare `dict` DOES satisfy an open record:
            // an open record's row tail already absorbs unknown fields, so it
            // imposes no closed-field obligation the way a `Shape` does. Removing
            // this arm breaks row-polymorphism (open-record `dict` tails), so both
            // directions stay `true` here; only the closed `Shape`/`dict`
            // direction is tightened above.
            (TypeExpr::OpenShape { .. }, TypeExpr::Named(n)) if n == "dict" => true,
            (TypeExpr::Named(n), TypeExpr::OpenShape { .. }) if n == "dict" => true,
            (TypeExpr::OpenShape { .. }, TypeExpr::DictType(..)) => true,
            (TypeExpr::DictType(ek, ev), TypeExpr::OpenShape { fields: af, .. }) => {
                matches!(ek.as_ref(), TypeExpr::Named(n) if n == "string")
                    && af
                        .iter()
                        .all(|f| self.types_compatible(ev, &f.type_expr, scope))
            }
            (TypeExpr::Shape(ef), TypeExpr::Shape(af)) => ef.iter().all(|expected_field| {
                let matched = af.iter().find(|f| f.name == expected_field.name);
                match matched {
                    // Optional fields may be omitted, but when supplied the
                    // value type still has to match — a typed
                    // `{drop_nil?: bool}` slot must reject a `drop_nil: string`
                    // literal at the call site instead of silently accepting.
                    None => expected_field.optional,
                    Some(actual_field) => {
                        // Treat an explicit `nil` literal as equivalent to
                        // omitting an optional field so callers can write
                        // `{flag: nil}` to mean "use the default" without
                        // having to drop the key. Required fields still
                        // reject `nil` unless the declared type permits it.
                        if expected_field.optional
                            && matches!(
                                &actual_field.type_expr,
                                TypeExpr::Named(n) if n == "nil"
                            )
                        {
                            return true;
                        }
                        self.types_compatible(
                            &expected_field.type_expr,
                            &actual_field.type_expr,
                            scope,
                        )
                    }
                }
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
            // list<T> is covariant in T. The classic covariance-with-mutation
            // hole — push a `float` through a `list<float>` alias, then read it
            // back as `int` through the original `list<int>` — requires *shared*
            // mutable aliasing, which Harn does not have: values have copy
            // semantics, so binding or passing a list hands over an independent
            // copy (`let b = a; b[0] = x` leaves `a` untouched) and `push` is a
            // functional operation that yields a new list. With no aliasing a
            // widening read is always sound, so `list` widens exactly like the
            // read-only `iter`/`generator`/`stream` sequences below.
            (TypeExpr::List(expected_inner), TypeExpr::List(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
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
            (TypeExpr::Generator(expected_inner), TypeExpr::Generator(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::Generator(_))
                if n == "generator" || n == "Generator" =>
            {
                true
            }
            (TypeExpr::Generator(_), TypeExpr::Named(n))
                if n == "generator" || n == "Generator" =>
            {
                true
            }
            (TypeExpr::Stream(expected_inner), TypeExpr::Stream(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::Stream(_)) if n == "stream" || n == "Stream" => true,
            (TypeExpr::Stream(_), TypeExpr::Named(n)) if n == "stream" || n == "Stream" => true,
            // dict<K, V> is covariant in its value type V, for the same
            // value-semantics reason as `list` above: there is no shared mutable
            // aliasing, so a widening read is sound. The key type K stays
            // invariant — Harn map keys are `string` in practice, and key
            // variance interacts with lookup in ways plain width-subtyping does
            // not, so keeping K exact costs nothing real and avoids that corner.
            (TypeExpr::DictType(ek, ev), TypeExpr::DictType(ak, av)) => {
                self.types_compatible_at(Polarity::Invariant, ek, ak, scope)
                    && self.types_compatible(ev, av, scope)
            }
            (TypeExpr::Named(n), TypeExpr::DictType(_, _)) if n == "dict" => true,
            (TypeExpr::DictType(_, _), TypeExpr::Named(n)) if n == "dict" => true,
            // Function parameters are contravariant (`fn(float)` can stand in
            // for expected `fn(int)` because floats contain ints), while returns
            // stay covariant. A function may also accept fewer parameters than
            // the context supplies; surplus callback arguments are ignored.
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
                ap.len() <= ep.len()
                    && ep.iter().zip(ap.iter()).all(|(e, a)| {
                        self.types_compatible_at(Polarity::Contravariant, e, a, scope)
                    })
                    && self.types_compatible(er, ar, scope)
            }
            // The dynamic `closure` nominal remains compatible with precise
            // function shapes.
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
        let mut visiting = HashSet::new();
        self.resolve_alias_inner(ty, scope, &mut visiting)
    }

    fn resolve_alias_inner(
        &self,
        ty: &TypeExpr,
        scope: &TypeScope,
        visiting: &mut HashSet<String>,
    ) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => {
                // `number` is a built-in alias for `int | float`. The runtime
                // type guard already accepts both, so expanding it here makes
                // the static checker treat `number` exactly like an explicit
                // `int | float` everywhere alias resolution runs (assignment,
                // argument, and return positions) instead of as an opaque name
                // that unifies with neither.
                if name == "number" {
                    return TypeExpr::Union(vec![
                        TypeExpr::Named("int".to_string()),
                        TypeExpr::Named("float".to_string()),
                    ]);
                }
                if let Some(resolved) = scope.resolve_type(name) {
                    if !visiting.insert(name.clone()) {
                        return ty.clone();
                    }
                    let resolved = self.resolve_alias_inner(resolved, scope, visiting);
                    visiting.remove(name);
                    return resolved;
                }
                ty.clone()
            }
            TypeExpr::Union(types) => TypeExpr::Union(
                types
                    .iter()
                    .map(|ty| self.resolve_alias_inner(ty, scope, visiting))
                    .collect(),
            ),
            TypeExpr::Intersection(types) => TypeExpr::Intersection(
                types
                    .iter()
                    .map(|ty| self.resolve_alias_inner(ty, scope, visiting))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.resolve_alias_inner(&field.type_expr, scope, visiting),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            // Resolve aliases inside the explicit fields and the row tails, then
            // re-fold so a tail that resolved to a concrete shape collapses into
            // the merged record.
            TypeExpr::OpenShape { fields, rests } => {
                let fields = fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.resolve_alias_inner(&field.type_expr, scope, visiting),
                        optional: field.optional,
                    })
                    .collect();
                let rests = rests
                    .iter()
                    .map(|rest| self.resolve_alias_inner(rest, scope, visiting))
                    .collect();
                super::super::binary_ops::fold_open_shape(fields, rests)
            }
            TypeExpr::List(inner) => {
                TypeExpr::List(Box::new(self.resolve_alias_inner(inner, scope, visiting)))
            }
            TypeExpr::Iter(inner) => {
                TypeExpr::Iter(Box::new(self.resolve_alias_inner(inner, scope, visiting)))
            }
            TypeExpr::Generator(inner) => {
                TypeExpr::Generator(Box::new(self.resolve_alias_inner(inner, scope, visiting)))
            }
            TypeExpr::Stream(inner) => {
                TypeExpr::Stream(Box::new(self.resolve_alias_inner(inner, scope, visiting)))
            }
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(self.resolve_alias_inner(key, scope, visiting)),
                Box::new(self.resolve_alias_inner(value, scope, visiting)),
            ),
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|param| self.resolve_alias_inner(param, scope, visiting))
                    .collect(),
                return_type: Box::new(self.resolve_alias_inner(return_type, scope, visiting)),
            },
            TypeExpr::Applied { name, args } => {
                let resolved_args: Vec<TypeExpr> = args
                    .iter()
                    .map(|arg| self.resolve_alias_inner(arg, scope, visiting))
                    .collect();
                // If the constructor is a `type T<...> = ...` alias (as
                // opposed to an enum/struct/interface), expand it by
                // substituting the declared parameters with the supplied
                // arguments. When an argument is a closed union, the
                // instantiation distributes over the members to produce
                // a union of substituted bodies. This is what lets
                // `Container<A | B>` flow as `Container<A> | Container<B>`
                // at use sites — each element self-consistently fixes
                // its own `T`.
                if let Some(info) = scope.resolve_type_alias(name) {
                    if info.type_params.len() == resolved_args.len() {
                        if !visiting.insert(name.clone()) {
                            return TypeExpr::Applied {
                                name: name.clone(),
                                args: resolved_args,
                            };
                        }
                        let names: Vec<String> =
                            info.type_params.iter().map(|tp| tp.name.clone()).collect();
                        let expanded =
                            instantiate_alias_distributive(&info.body, &names, &resolved_args);
                        let resolved = self.resolve_alias_inner(&expanded, scope, visiting);
                        visiting.remove(name);
                        return resolved;
                    }
                }
                TypeExpr::Applied {
                    name: name.clone(),
                    args: resolved_args,
                }
            }
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(s) => TypeExpr::LitString(s.clone()),
            TypeExpr::LitInt(v) => TypeExpr::LitInt(*v),
            TypeExpr::Owned(inner) => {
                TypeExpr::Owned(Box::new(self.resolve_alias_inner(inner, scope, visiting)))
            }
        }
    }
}

/// Substitute `param_names[i] := args[i]` into `body`. When an argument is
/// a `Union`, the body is cloned per union member and each result is
/// unioned together — the "distributive" instantiation that makes
/// `Container<A | B>` equal `Container<A> | Container<B>`.
fn instantiate_alias_distributive(
    body: &TypeExpr,
    param_names: &[String],
    args: &[TypeExpr],
) -> TypeExpr {
    debug_assert_eq!(param_names.len(), args.len());
    let mut variants: Vec<std::collections::BTreeMap<String, TypeExpr>> =
        vec![std::collections::BTreeMap::new()];
    for (name, arg) in param_names.iter().zip(args.iter()) {
        let members: Vec<TypeExpr> = match arg {
            TypeExpr::Union(items) if !items.is_empty() => items.clone(),
            other => vec![other.clone()],
        };
        let mut next = Vec::with_capacity(variants.len() * members.len());
        for base in &variants {
            for member in &members {
                let mut extended = base.clone();
                extended.insert(name.clone(), member.clone());
                next.push(extended);
            }
        }
        variants = next;
    }
    let results: Vec<TypeExpr> = variants
        .into_iter()
        .map(|bindings| TypeChecker::apply_type_bindings(body, &bindings))
        .collect();
    collapse_members(results, TypeExpr::Never, TypeExpr::Union)
}
