//! Declaration-site variance enforcement.
//!
//! Each `check_*_decl_variance` shim hands the relevant set of
//! `(TypeExpr, Polarity)` positions to `check_decl_variance`, which then
//! recursively walks each position via `walk_variance` and reports any
//! type parameter that appears in a position incompatible with its
//! declared variance marker (`out` / `in` / unannotated).

use std::collections::BTreeMap;

use crate::ast::*;
use harn_lexer::Span;

use super::super::scope::Polarity;
use super::super::TypeChecker;

impl TypeChecker {
    /// Check declared variance on a `fn` declaration. Parameter
    /// types are contravariant positions; the return type is a
    /// covariant position.
    pub(in crate::typechecker) fn check_fn_decl_variance(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: Option<&TypeExpr>,
        name: &str,
        span: Span,
    ) {
        let mut positions: Vec<(&TypeExpr, Polarity)> = Vec::new();
        for p in params {
            if let Some(te) = &p.type_expr {
                positions.push((te, Polarity::Contravariant));
            }
        }
        if let Some(rt) = return_type {
            positions.push((rt, Polarity::Covariant));
        }
        let kind = format!("function '{name}'");
        self.check_decl_variance(&kind, type_params, &positions, span);
    }

    /// Check declared variance on a `type` alias. The alias body is
    /// treated as a covariant position (what the alias "produces").
    pub(in crate::typechecker) fn check_type_alias_decl_variance(
        &mut self,
        type_params: &[TypeParam],
        type_expr: &TypeExpr,
        name: &str,
        span: Span,
    ) {
        let positions = [(type_expr, Polarity::Covariant)];
        let kind = format!("type alias '{name}'");
        self.check_decl_variance(&kind, type_params, &positions, span);
    }

    /// Check declared variance on an `enum` declaration. Variant
    /// field types are covariant positions (enums are produced,
    /// not mutated in place).
    pub(in crate::typechecker) fn check_enum_decl_variance(
        &mut self,
        type_params: &[TypeParam],
        variants: &[EnumVariant],
        name: &str,
        span: Span,
    ) {
        let mut positions: Vec<(&TypeExpr, Polarity)> = Vec::new();
        for variant in variants {
            for field in &variant.fields {
                if let Some(te) = &field.type_expr {
                    positions.push((te, Polarity::Covariant));
                }
            }
        }
        let kind = format!("enum '{name}'");
        self.check_decl_variance(&kind, type_params, &positions, span);
    }

    /// Check declared variance on a `struct` declaration. Field
    /// types are invariant positions because struct fields are
    /// mutable in Harn. (If Harn ever gains read-only fields, those
    /// could be covariant.)
    pub(in crate::typechecker) fn check_struct_decl_variance(
        &mut self,
        type_params: &[TypeParam],
        fields: &[StructField],
        name: &str,
        span: Span,
    ) {
        let positions: Vec<(&TypeExpr, Polarity)> = fields
            .iter()
            .filter_map(|f| f.type_expr.as_ref().map(|te| (te, Polarity::Invariant)))
            .collect();
        let kind = format!("struct '{name}'");
        self.check_decl_variance(&kind, type_params, &positions, span);
    }

    /// Check declared variance on an `interface` declaration.
    /// Method parameter types are contravariant; method return types
    /// are covariant. Associated types are invariant positions.
    pub(in crate::typechecker) fn check_interface_decl_variance(
        &mut self,
        type_params: &[TypeParam],
        methods: &[InterfaceMethod],
        name: &str,
        span: Span,
    ) {
        let mut positions: Vec<(&TypeExpr, Polarity)> = Vec::new();
        for method in methods {
            for p in &method.params {
                if let Some(te) = &p.type_expr {
                    positions.push((te, Polarity::Contravariant));
                }
            }
            if let Some(rt) = &method.return_type {
                positions.push((rt, Polarity::Covariant));
            }
        }
        let kind = format!("interface '{name}'");
        self.check_decl_variance(&kind, type_params, &positions, span);
    }

    /// Declaration-site variance check.
    ///
    /// Given a set of declared type parameters (with their `Variance`
    /// annotations) and a list of positions in the declaration body
    /// where those parameters may appear, walk each position tracking
    /// polarity. A parameter declared `out T` (`Covariant`) may appear
    /// only in covariant positions; `in T` (`Contravariant`) only in
    /// contravariant positions; unannotated (`Invariant`) may appear
    /// anywhere.
    ///
    /// Callers pass each top-level expression together with its
    /// starting polarity. For example, a function's return type
    /// starts at `Covariant`, each parameter starts at `Contravariant`,
    /// an enum variant field starts at `Covariant`, etc.
    fn check_decl_variance(
        &mut self,
        decl_kind: &str,
        type_params: &[TypeParam],
        positions: &[(&TypeExpr, Polarity)],
        span: Span,
    ) {
        // Build a quick lookup: param name -> declared variance.
        // If no parameter has a non-invariant marker, skip the walk —
        // invariant params can appear in any polarity.
        if type_params
            .iter()
            .all(|tp| tp.variance == Variance::Invariant)
        {
            return;
        }
        let declared: BTreeMap<String, Variance> = type_params
            .iter()
            .map(|tp| (tp.name.clone(), tp.variance))
            .collect();
        for (ty, polarity) in positions {
            self.walk_variance(decl_kind, ty, *polarity, &declared, span);
        }
    }

    /// Recursive walker used by [`check_decl_variance`].
    #[allow(clippy::only_used_in_recursion)]
    fn walk_variance(
        &mut self,
        decl_kind: &str,
        ty: &TypeExpr,
        polarity: Polarity,
        declared: &BTreeMap<String, Variance>,
        span: Span,
    ) {
        match ty {
            TypeExpr::Named(name) => {
                if let Some(&declared_variance) = declared.get(name) {
                    let ok = matches!(
                        (declared_variance, polarity),
                        (Variance::Invariant, _)
                            | (Variance::Covariant, Polarity::Covariant)
                            | (Variance::Contravariant, Polarity::Contravariant)
                    );
                    if !ok {
                        let (marker, declared_word) = match declared_variance {
                            Variance::Covariant => ("out", "covariant"),
                            Variance::Contravariant => ("in", "contravariant"),
                            Variance::Invariant => unreachable!(),
                        };
                        let position_word = match polarity {
                            Polarity::Covariant => "covariant",
                            Polarity::Contravariant => "contravariant",
                            Polarity::Invariant => "invariant",
                        };
                        self.error_at(
                            format!(
                                "type parameter '{name}' is declared '{marker}' \
                                 ({declared_word}) but appears in a \
                                 {position_word} position in {decl_kind}"
                            ),
                            span,
                        );
                    }
                }
            }
            TypeExpr::List(inner)
            | TypeExpr::Iter(inner)
            | TypeExpr::Generator(inner)
            | TypeExpr::Stream(inner) => {
                // `list<T>` is invariant; iter/generator/stream are covariant.
                let sub = match ty {
                    TypeExpr::List(_) => Polarity::Invariant,
                    TypeExpr::Iter(_) | TypeExpr::Generator(_) | TypeExpr::Stream(_) => polarity,
                    _ => unreachable!(),
                };
                self.walk_variance(decl_kind, inner, sub, declared, span);
            }
            TypeExpr::DictType(k, v) => {
                // dict<K, V> is invariant in both.
                self.walk_variance(decl_kind, k, Polarity::Invariant, declared, span);
                self.walk_variance(decl_kind, v, Polarity::Invariant, declared, span);
            }
            TypeExpr::Shape(fields) => {
                for f in fields {
                    self.walk_variance(decl_kind, &f.type_expr, polarity, declared, span);
                }
            }
            TypeExpr::Union(members) => {
                for m in members {
                    self.walk_variance(decl_kind, m, polarity, declared, span);
                }
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                // Parameters are contravariant; return is covariant,
                // composed with the outer polarity.
                let param_polarity = polarity.compose(Variance::Contravariant);
                for p in params {
                    self.walk_variance(decl_kind, p, param_polarity, declared, span);
                }
                self.walk_variance(decl_kind, return_type, polarity, declared, span);
            }
            TypeExpr::Applied { name, args } => {
                // Consult the declared variance of this constructor.
                // Built-in `Result` has covariant-covariant; user
                // generics carry their own variance annotations;
                // unknown constructors fall back to invariance (safe).
                let variances: Option<Vec<Variance>> = self
                    .scope
                    .get_enum(name)
                    .map(|info| info.type_params.iter().map(|tp| tp.variance).collect())
                    .or_else(|| {
                        self.scope
                            .get_struct(name)
                            .map(|info| info.type_params.iter().map(|tp| tp.variance).collect())
                    })
                    .or_else(|| {
                        self.scope
                            .get_interface(name)
                            .map(|info| info.type_params.iter().map(|tp| tp.variance).collect())
                    });
                for (idx, arg) in args.iter().enumerate() {
                    let child_variance = variances
                        .as_ref()
                        .and_then(|v| v.get(idx).copied())
                        .unwrap_or(Variance::Invariant);
                    let sub = polarity.compose(child_variance);
                    self.walk_variance(decl_kind, arg, sub, declared, span);
                }
            }
            TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => {}
        }
    }
}
