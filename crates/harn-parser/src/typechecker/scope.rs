//! Scope tracking, refinements, and shared support types for the type checker.
//!
//! This module owns the lexical scope chain (`TypeScope`), the bidirectional
//! refinement record (`Refinements`), and the small declaration-info helper
//! structs (`EnumDeclInfo` / `StructDeclInfo` / `InterfaceDeclInfo` /
//! `ImplMethodSig` / `FnSignature`). It also re-exports the type-checker's
//! gradual-typing alias (`InferredType`) and the variance polarity tracker.

use std::collections::BTreeMap;

use crate::ast::*;
use crate::builtin_signatures;

use super::union::apply_refinements;

/// Inferred type of an expression. None means unknown/untyped (gradual typing).
pub(super) type InferredType = Option<TypeExpr>;

/// The polarity of a position in a type when checking subtyping.
///
/// Polarity propagates through compound types: `FnType` flips it on
/// its parameters; `Invariant` absorbs any further flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Polarity {
    Covariant,
    Contravariant,
    Invariant,
}

impl Polarity {
    /// Compose the outer position polarity with the declared variance
    /// of a type parameter (or the hard-coded variance of a compound
    /// type constructor's slot).
    pub(super) fn compose(self, child: Variance) -> Polarity {
        match (self, child) {
            (_, Variance::Invariant) | (Polarity::Invariant, _) => Polarity::Invariant,
            (p, Variance::Covariant) => p,
            (Polarity::Covariant, Variance::Contravariant) => Polarity::Contravariant,
            (Polarity::Contravariant, Variance::Contravariant) => Polarity::Covariant,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct EnumDeclInfo {
    pub(super) type_params: Vec<TypeParam>,
    pub(super) variants: Vec<EnumVariant>,
}

/// Full metadata for a `type T<...> = ...` alias. The type parameters are
/// retained so that `Applied { name, args }` references can be expanded by
/// substituting `type_params[i] := args[i]` into `body`, and closed-union
/// arguments can be distributed into a union of instantiations.
#[derive(Debug, Clone)]
pub(super) struct TypeAliasInfo {
    pub(super) type_params: Vec<TypeParam>,
    pub(super) body: TypeExpr,
}

#[derive(Debug, Clone)]
pub(super) struct StructDeclInfo {
    pub(super) type_params: Vec<TypeParam>,
    pub(super) fields: Vec<StructField>,
}

#[derive(Debug, Clone)]
pub(super) struct InterfaceDeclInfo {
    pub(super) type_params: Vec<TypeParam>,
    pub(super) associated_types: Vec<(String, Option<TypeExpr>)>,
    pub(super) methods: Vec<InterfaceMethod>,
}

/// Scope for tracking variable types.
#[derive(Debug, Clone)]
pub(super) struct TypeScope {
    /// Variable name → inferred type.
    pub(super) vars: BTreeMap<String, InferredType>,
    /// Function name → (param types, return type).
    pub(super) functions: BTreeMap<String, FnSignature>,
    /// Named type aliases. Retains the declared type parameters so that
    /// generic aliases can be expanded via substitution on `Applied`.
    pub(super) type_aliases: BTreeMap<String, TypeAliasInfo>,
    /// Enum declarations with generic and variant metadata.
    pub(super) enums: BTreeMap<String, EnumDeclInfo>,
    /// Interface declarations with associated types and methods.
    pub(super) interfaces: BTreeMap<String, InterfaceDeclInfo>,
    /// Struct declarations with generic and field metadata.
    pub(super) structs: BTreeMap<String, StructDeclInfo>,
    /// Impl block methods: type_name → method signatures.
    pub(super) impl_methods: BTreeMap<String, Vec<ImplMethodSig>>,
    /// Generic type parameter names in scope (treated as compatible with any type).
    pub(super) generic_type_params: std::collections::BTreeSet<String>,
    /// Where-clause constraints: type_param → interface_bound.
    /// Used for definition-site checking of generic function bodies.
    pub(super) where_constraints: BTreeMap<String, String>,
    /// Variables declared with `var` (mutable). Variables not in this set
    /// are immutable (`let`, function params, loop vars, etc.).
    pub(super) mutable_vars: std::collections::BTreeSet<String>,
    /// Variables that have been narrowed by flow-sensitive refinement.
    /// Maps var name → pre-narrowing type (used to restore on reassignment).
    pub(super) narrowed_vars: BTreeMap<String, InferredType>,
    /// Mutable vars declared as unannotated `var x = nil`. A local `false`
    /// entry shadows a parent widenable marker after a new declaration or
    /// after the first successful widening assignment.
    pub(super) nil_widenable_vars: BTreeMap<String, bool>,
    /// Schema literals bound to variables, reduced to a TypeExpr subset so
    /// `schema_is(x, some_schema)` can participate in flow refinement.
    pub(super) schema_bindings: BTreeMap<String, InferredType>,
    /// Variables holding unvalidated values from boundary APIs (json_parse, llm_call, etc.).
    /// Maps var name → source function name (e.g. "json_parse").
    /// Empty string = explicitly cleared (shadows parent scope entry).
    pub(super) untyped_sources: BTreeMap<String, String>,
    /// Concrete `type_of` variants ruled out on the current flow path for each
    /// `unknown`-typed variable. Drives the exhaustive-narrowing warning at
    /// `unreachable()` / `throw` / `never`-returning calls.
    pub(super) unknown_ruled_out: BTreeMap<String, Vec<String>>,
    pub(super) parent: Option<Box<TypeScope>>,
}

/// Method signature extracted from an impl block (for interface checking).
#[derive(Debug, Clone)]
pub(super) struct ImplMethodSig {
    pub(super) name: String,
    /// Number of parameters excluding `self`.
    pub(super) param_count: usize,
    /// Parameter types (excluding `self`), None means untyped.
    pub(super) param_types: Vec<Option<TypeExpr>>,
    /// Return type, None means untyped.
    pub(super) return_type: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
pub(super) struct FnSignature {
    pub(super) params: Vec<(String, InferredType)>,
    pub(super) return_type: InferredType,
    /// Generic type parameter names declared on the function.
    pub(super) type_param_names: Vec<String>,
    /// Number of required parameters (those without defaults).
    pub(super) required_params: usize,
    /// Where-clause constraints: (type_param_name, interface_bound).
    pub(super) where_clauses: Vec<(String, String)>,
    /// True if the last parameter is a rest parameter.
    pub(super) has_rest: bool,
}

impl TypeScope {
    pub(super) fn new() -> Self {
        let mut scope = Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            structs: BTreeMap::new(),
            impl_methods: BTreeMap::new(),
            generic_type_params: std::collections::BTreeSet::new(),
            where_constraints: BTreeMap::new(),
            mutable_vars: std::collections::BTreeSet::new(),
            narrowed_vars: BTreeMap::new(),
            nil_widenable_vars: BTreeMap::new(),
            schema_bindings: BTreeMap::new(),
            untyped_sources: BTreeMap::new(),
            unknown_ruled_out: BTreeMap::new(),
            parent: None,
        };
        scope.enums.insert(
            "Result".into(),
            EnumDeclInfo {
                type_params: vec![
                    TypeParam {
                        name: "T".into(),
                        variance: Variance::Covariant,
                    },
                    TypeParam {
                        name: "E".into(),
                        variance: Variance::Covariant,
                    },
                ],
                variants: vec![
                    EnumVariant {
                        name: "Ok".into(),
                        fields: vec![TypedParam {
                            name: "value".into(),
                            type_expr: Some(TypeExpr::Named("T".into())),
                            default_value: None,
                            rest: false,
                        }],
                    },
                    EnumVariant {
                        name: "Err".into(),
                        fields: vec![TypedParam {
                            name: "error".into(),
                            type_expr: Some(TypeExpr::Named("E".into())),
                            default_value: None,
                            rest: false,
                        }],
                    },
                ],
            },
        );
        scope
    }

    pub(super) fn child(&self) -> Self {
        Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            structs: BTreeMap::new(),
            impl_methods: BTreeMap::new(),
            generic_type_params: std::collections::BTreeSet::new(),
            where_constraints: BTreeMap::new(),
            mutable_vars: std::collections::BTreeSet::new(),
            narrowed_vars: BTreeMap::new(),
            nil_widenable_vars: BTreeMap::new(),
            schema_bindings: BTreeMap::new(),
            untyped_sources: BTreeMap::new(),
            unknown_ruled_out: BTreeMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    pub(super) fn get_var(&self, name: &str) -> Option<&InferredType> {
        self.vars
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_var(name))
    }

    /// Record that a concrete `type_of` variant has been ruled out for
    /// an `unknown`-typed variable on the current flow path.
    pub(super) fn add_unknown_ruled_out(&mut self, var_name: &str, type_name: &str) {
        if !self.unknown_ruled_out.contains_key(var_name) {
            let inherited = self.lookup_unknown_ruled_out(var_name);
            self.unknown_ruled_out
                .insert(var_name.to_string(), inherited);
        }
        let entry = self
            .unknown_ruled_out
            .get_mut(var_name)
            .expect("just inserted");
        if !entry.iter().any(|t| t == type_name) {
            entry.push(type_name.to_string());
        }
    }

    /// Return the ruled-out concrete types recorded for `var_name` across
    /// the current scope chain (child entries mask parent entries).
    pub(super) fn lookup_unknown_ruled_out(&self, var_name: &str) -> Vec<String> {
        if let Some(list) = self.unknown_ruled_out.get(var_name) {
            list.clone()
        } else if let Some(parent) = &self.parent {
            parent.lookup_unknown_ruled_out(var_name)
        } else {
            Vec::new()
        }
    }

    /// Collect every `unknown`-typed variable that has at least one ruled-out
    /// concrete type on the current flow path (merged across parent scopes).
    pub(super) fn collect_unknown_ruled_out(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        self.collect_unknown_ruled_out_inner(&mut out);
        out
    }

    fn collect_unknown_ruled_out_inner(&self, acc: &mut BTreeMap<String, Vec<String>>) {
        if let Some(parent) = &self.parent {
            parent.collect_unknown_ruled_out_inner(acc);
        }
        for (name, list) in &self.unknown_ruled_out {
            acc.insert(name.clone(), list.clone());
        }
    }

    /// Drop the ruled-out set for a variable (used on reassignment).
    pub(super) fn clear_unknown_ruled_out(&mut self, var_name: &str) {
        // Shadow any parent entry with an empty list so lookups in this
        // scope (and its children) treat the variable as un-narrowed.
        self.unknown_ruled_out
            .insert(var_name.to_string(), Vec::new());
    }

    /// Collect every function name visible through this scope chain.
    /// Used by the strict cross-module check to offer "did you mean"
    /// suggestions that span the whole lexical visibility set.
    pub(super) fn all_fn_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.functions.keys().cloned().collect();
        if let Some(parent) = &self.parent {
            names.extend(parent.all_fn_names());
        }
        names
    }

    /// Collect every struct name visible through this scope chain.
    /// Used for typo suggestions on struct construction sites.
    pub(super) fn all_struct_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.structs.keys().cloned().collect();
        if let Some(parent) = &self.parent {
            names.extend(parent.all_struct_names());
        }
        names
    }

    pub(super) fn get_fn(&self, name: &str) -> Option<&FnSignature> {
        self.functions
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_fn(name))
    }

    pub(super) fn get_schema_binding(&self, name: &str) -> Option<&InferredType> {
        self.schema_bindings
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_schema_binding(name))
    }

    pub(super) fn resolve_type(&self, name: &str) -> Option<&TypeExpr> {
        self.type_aliases
            .get(name)
            .map(|info| &info.body)
            .or_else(|| self.parent.as_ref()?.resolve_type(name))
    }

    /// Full alias metadata including declared type parameters. Used by
    /// `resolve_alias` to expand `Applied { name, args }` references into
    /// the alias body with substitutions applied.
    pub(super) fn resolve_type_alias(&self, name: &str) -> Option<&TypeAliasInfo> {
        self.type_aliases
            .get(name)
            .or_else(|| self.parent.as_ref()?.resolve_type_alias(name))
    }

    pub(super) fn is_generic_type_param(&self, name: &str) -> bool {
        self.generic_type_params.contains(name)
            || self
                .parent
                .as_ref()
                .is_some_and(|p| p.is_generic_type_param(name))
    }

    pub(super) fn get_where_constraint(&self, type_param: &str) -> Option<&str> {
        self.where_constraints
            .get(type_param)
            .map(|s| s.as_str())
            .or_else(|| {
                self.parent
                    .as_ref()
                    .and_then(|p| p.get_where_constraint(type_param))
            })
    }

    pub(super) fn get_enum(&self, name: &str) -> Option<&EnumDeclInfo> {
        self.enums
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_enum(name))
    }

    pub(super) fn get_interface(&self, name: &str) -> Option<&InterfaceDeclInfo> {
        self.interfaces
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_interface(name))
    }

    pub(super) fn get_struct(&self, name: &str) -> Option<&StructDeclInfo> {
        self.structs
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_struct(name))
    }

    pub(super) fn get_impl_methods(&self, name: &str) -> Option<&Vec<ImplMethodSig>> {
        self.impl_methods
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_impl_methods(name))
    }

    /// Look up declared variance for each type parameter of a
    /// constructor (enum / struct / interface). Returns `None` if the
    /// name is not a known user-declared generic. Built-in type
    /// constructors that live outside `Applied` (list, dict, iter,
    /// FnType) are handled directly in the subtype match arms rather
    /// than via this table.
    pub(super) fn variance_of(&self, name: &str) -> Option<Vec<Variance>> {
        if let Some(info) = self.get_enum(name) {
            return Some(info.type_params.iter().map(|tp| tp.variance).collect());
        }
        if let Some(info) = self.get_struct(name) {
            return Some(info.type_params.iter().map(|tp| tp.variance).collect());
        }
        if let Some(info) = self.get_interface(name) {
            return Some(info.type_params.iter().map(|tp| tp.variance).collect());
        }
        None
    }

    pub(super) fn define_var(&mut self, name: &str, ty: InferredType) {
        if is_discard_name(name) {
            return;
        }
        self.vars.insert(name.to_string(), ty);
    }

    pub(super) fn define_var_mutable(&mut self, name: &str, ty: InferredType) {
        if is_discard_name(name) {
            return;
        }
        self.vars.insert(name.to_string(), ty);
        self.mutable_vars.insert(name.to_string());
    }

    pub(super) fn mark_nil_widenable(&mut self, name: &str) {
        if is_discard_name(name) {
            return;
        }
        self.nil_widenable_vars.insert(name.to_string(), true);
    }

    pub(super) fn clear_nil_widenable(&mut self, name: &str) {
        if is_discard_name(name) {
            return;
        }
        self.nil_widenable_vars.insert(name.to_string(), false);
    }

    pub(super) fn is_nil_widenable(&self, name: &str) -> bool {
        if let Some(enabled) = self.nil_widenable_vars.get(name) {
            return *enabled;
        }
        self.parent
            .as_ref()
            .is_some_and(|p| p.is_nil_widenable(name))
    }

    pub(super) fn define_schema_binding(&mut self, name: &str, ty: InferredType) {
        if is_discard_name(name) {
            return;
        }
        self.schema_bindings.insert(name.to_string(), ty);
    }

    /// Check whether a variable holds an unvalidated boundary-API value.
    /// Returns the source function name (e.g. "json_parse") or `None`.
    pub(super) fn is_untyped_source(&self, name: &str) -> Option<&str> {
        if let Some(source) = self.untyped_sources.get(name) {
            if source.is_empty() {
                return None; // explicitly cleared in this scope
            }
            return Some(source.as_str());
        }
        self.parent.as_ref()?.is_untyped_source(name)
    }

    pub(super) fn mark_untyped_source(&mut self, name: &str, source: &str) {
        if is_discard_name(name) {
            return;
        }
        self.untyped_sources
            .insert(name.to_string(), source.to_string());
    }

    /// Clear the untyped-source flag for a variable, shadowing any parent entry.
    pub(super) fn clear_untyped_source(&mut self, name: &str) {
        self.untyped_sources.insert(name.to_string(), String::new());
    }

    /// Check if a variable is mutable (declared with `var`).
    pub(super) fn is_mutable(&self, name: &str) -> bool {
        self.mutable_vars.contains(name) || self.parent.as_ref().is_some_and(|p| p.is_mutable(name))
    }

    pub(super) fn define_fn(&mut self, name: &str, sig: FnSignature) {
        self.functions.insert(name.to_string(), sig);
    }
}

/// Bidirectional type refinements extracted from a condition.
/// Each path contains a list of (variable_name, narrowed_type) pairs.
#[derive(Debug, Clone, Default)]
pub(super) struct Refinements {
    /// Narrowings when the condition evaluates to true/truthy.
    pub(super) truthy: Vec<(String, InferredType)>,
    /// Narrowings when the condition evaluates to false/falsy.
    pub(super) falsy: Vec<(String, InferredType)>,
    /// Concrete `type_of` variants (var_name, type_name) to add to the
    /// ruled-out coverage set on the truthy branch. Only populated for
    /// `type_of(x) != "T"` patterns against `unknown`-typed values.
    pub(super) truthy_ruled_out: Vec<(String, String)>,
    /// Same as `truthy_ruled_out` but applied on the falsy branch — the
    /// common case for `if type_of(x) == "T" { return }` exhaustiveness
    /// patterns.
    pub(super) falsy_ruled_out: Vec<(String, String)>,
}

impl Refinements {
    pub(super) fn empty() -> Self {
        Self::default()
    }

    /// Swap truthy and falsy (used for negation).
    pub(super) fn inverted(self) -> Self {
        Self {
            truthy: self.falsy,
            falsy: self.truthy,
            truthy_ruled_out: self.falsy_ruled_out,
            falsy_ruled_out: self.truthy_ruled_out,
        }
    }

    /// Apply the truthy-branch narrowings and ruled-out additions to `scope`.
    pub(super) fn apply_truthy(&self, scope: &mut TypeScope) {
        apply_refinements(scope, &self.truthy);
        for (var, ty) in &self.truthy_ruled_out {
            scope.add_unknown_ruled_out(var, ty);
        }
    }

    /// Apply the falsy-branch narrowings and ruled-out additions to `scope`.
    pub(super) fn apply_falsy(&self, scope: &mut TypeScope) {
        apply_refinements(scope, &self.falsy);
        for (var, ty) in &self.falsy_ruled_out {
            scope.add_unknown_ruled_out(var, ty);
        }
    }
}

/// Known return types for builtin functions. Delegates to the shared
/// [`builtin_signatures`] registry — see that module for the full table.
pub(super) fn builtin_return_type(name: &str) -> InferredType {
    builtin_signatures::builtin_return_type(name)
}

/// Check if a name is a known builtin. Delegates to the shared
/// [`builtin_signatures`] registry.
pub(super) fn is_builtin(name: &str) -> bool {
    builtin_signatures::is_builtin(name)
}
