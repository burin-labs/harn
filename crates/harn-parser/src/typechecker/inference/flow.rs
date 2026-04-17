//! Flow-sensitive narrowing: refinement extraction and exhaustiveness checks.
//!
//! `extract_refinements` is the dispatch entry — given a condition AST node
//! it yields a `Refinements` describing the narrowings to apply on the
//! truthy and falsy branches. The supporting `extract_*_refinements`
//! helpers cover the specific patterns the type checker recognises:
//! `x != nil`, `type_of(x) == "T"`, `x.has("k")`, `schema_is(x, S)`, and
//! their negations.
//!
//! The match-exhaustiveness checks (`check_match_exhaustiveness`,
//! `check_match_exhaustiveness_union`) and the `unknown`-variant
//! exhaustiveness check (`check_unknown_exhaustiveness`) live here too —
//! they all consume the same `unknown_ruled_out` ledger that refinement
//! extraction populates.

use crate::ast::*;
use harn_lexer::Span;

use super::super::exits::block_definitely_exits;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{Refinements, TypeScope};
use super::super::union::{
    extract_type_of_var, intersect_types, narrow_to_single, remove_from_union, subtract_type,
};
use super::super::TypeChecker;

impl TypeChecker {
    /// Extract bidirectional type refinements from a condition expression.
    pub(in crate::typechecker) fn extract_refinements(
        condition: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        match &condition.node {
            Node::BinaryOp { op, left, right } if op == "!=" || op == "==" => {
                let nil_ref = Self::extract_nil_refinements(op, left, right, scope);
                if !nil_ref.truthy.is_empty() || !nil_ref.falsy.is_empty() {
                    return nil_ref;
                }
                let typeof_ref = Self::extract_typeof_refinements(op, left, right, scope);
                if !typeof_ref.truthy.is_empty() || !typeof_ref.falsy.is_empty() {
                    return typeof_ref;
                }
                Refinements::empty()
            }

            // Logical AND: both operands must be truthy, so truthy refinements compose.
            Node::BinaryOp { op, left, right } if op == "&&" => {
                let left_ref = Self::extract_refinements(left, scope);
                let right_ref = Self::extract_refinements(right, scope);
                let mut truthy = left_ref.truthy;
                truthy.extend(right_ref.truthy);
                let mut truthy_ruled_out = left_ref.truthy_ruled_out;
                truthy_ruled_out.extend(right_ref.truthy_ruled_out);
                Refinements {
                    truthy,
                    falsy: vec![],
                    truthy_ruled_out,
                    falsy_ruled_out: vec![],
                }
            }

            // Logical OR: both operands must be falsy for the whole to be falsy.
            Node::BinaryOp { op, left, right } if op == "||" => {
                let left_ref = Self::extract_refinements(left, scope);
                let right_ref = Self::extract_refinements(right, scope);
                let mut falsy = left_ref.falsy;
                falsy.extend(right_ref.falsy);
                let mut falsy_ruled_out = left_ref.falsy_ruled_out;
                falsy_ruled_out.extend(right_ref.falsy_ruled_out);
                Refinements {
                    truthy: vec![],
                    falsy,
                    truthy_ruled_out: vec![],
                    falsy_ruled_out,
                }
            }

            Node::UnaryOp { op, operand } if op == "!" => {
                Self::extract_refinements(operand, scope).inverted()
            }

            // Bare identifier in condition position: narrow `T | nil` to `T`.
            Node::Identifier(name) => {
                if let Some(Some(TypeExpr::Union(members))) = scope.get_var(name) {
                    if members
                        .iter()
                        .any(|m| matches!(m, TypeExpr::Named(n) if n == "nil"))
                    {
                        if let Some(narrowed) = remove_from_union(members, "nil") {
                            return Refinements {
                                truthy: vec![(name.clone(), Some(narrowed))],
                                falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                                truthy_ruled_out: vec![],
                                falsy_ruled_out: vec![],
                            };
                        }
                    }
                }
                Refinements::empty()
            }

            Node::MethodCall {
                object,
                method,
                args,
            } if method == "has" && args.len() == 1 => {
                Self::extract_has_refinements(object, args, scope)
            }

            Node::FunctionCall { name, args }
                if (name == "schema_is" || name == "is_type") && args.len() == 2 =>
            {
                Self::extract_schema_refinements(args, scope)
            }

            _ => Refinements::empty(),
        }
    }

    /// Extract nil-check refinements from `x != nil` / `x == nil` patterns.
    fn extract_nil_refinements(
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let var_node = if matches!(right.node, Node::NilLiteral) {
            left
        } else if matches!(left.node, Node::NilLiteral) {
            right
        } else {
            return Refinements::empty();
        };

        if let Node::Identifier(name) = &var_node.node {
            let var_type = scope.get_var(name).cloned().flatten();
            match var_type {
                Some(TypeExpr::Union(ref members)) => {
                    if let Some(narrowed) = remove_from_union(members, "nil") {
                        let neq_refs = Refinements {
                            truthy: vec![(name.clone(), Some(narrowed))],
                            falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                            ..Refinements::default()
                        };
                        return if op == "!=" {
                            neq_refs
                        } else {
                            neq_refs.inverted()
                        };
                    }
                }
                Some(TypeExpr::Named(ref n)) if n == "nil" => {
                    // Single nil type: == nil is always true, != nil narrows to never.
                    let eq_refs = Refinements {
                        truthy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                        falsy: vec![(name.clone(), Some(TypeExpr::Never))],
                        ..Refinements::default()
                    };
                    return if op == "==" {
                        eq_refs
                    } else {
                        eq_refs.inverted()
                    };
                }
                _ => {}
            }
        }
        Refinements::empty()
    }

    /// Extract type_of refinements from `type_of(x) == "typename"` patterns.
    fn extract_typeof_refinements(
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let (var_name, type_name) = if let (Some(var), Node::StringLiteral(tn)) =
            (extract_type_of_var(left), &right.node)
        {
            (var, tn.clone())
        } else if let (Node::StringLiteral(tn), Some(var)) =
            (&left.node, extract_type_of_var(right))
        {
            (var, tn.clone())
        } else {
            return Refinements::empty();
        };

        const KNOWN_TYPES: &[&str] = &[
            "int", "string", "float", "bool", "nil", "list", "dict", "closure",
        ];
        if !KNOWN_TYPES.contains(&type_name.as_str()) {
            return Refinements::empty();
        }

        let var_type = scope.get_var(&var_name).cloned().flatten();
        match var_type {
            Some(TypeExpr::Union(ref members)) => {
                let narrowed = narrow_to_single(members, &type_name);
                let remaining = remove_from_union(members, &type_name);
                if narrowed.is_some() || remaining.is_some() {
                    let eq_refs = Refinements {
                        truthy: narrowed
                            .map(|n| vec![(var_name.clone(), Some(n))])
                            .unwrap_or_default(),
                        falsy: remaining
                            .map(|r| vec![(var_name.clone(), Some(r))])
                            .unwrap_or_default(),
                        ..Refinements::default()
                    };
                    return if op == "==" {
                        eq_refs
                    } else {
                        eq_refs.inverted()
                    };
                }
            }
            Some(TypeExpr::Named(ref n)) if n == &type_name => {
                // Single named type matches the typeof check:
                // truthy = same type, falsy = never (type is fully ruled out).
                let eq_refs = Refinements {
                    truthy: vec![(var_name.clone(), Some(TypeExpr::Named(type_name)))],
                    falsy: vec![(var_name.clone(), Some(TypeExpr::Never))],
                    ..Refinements::default()
                };
                return if op == "==" {
                    eq_refs
                } else {
                    eq_refs.inverted()
                };
            }
            Some(TypeExpr::Named(ref n)) if n == "unknown" => {
                // `unknown` narrows to the tested concrete type on the truthy
                // branch. The falsy branch keeps `unknown` — subtracting one
                // concrete type from an open top still leaves an open top —
                // but we remember which concrete variants have been ruled
                // out so `unreachable()` / `throw` can detect incomplete
                // exhaustive-narrowing chains.
                let eq_refs = Refinements {
                    truthy: vec![(var_name.clone(), Some(TypeExpr::Named(type_name.clone())))],
                    falsy: vec![],
                    truthy_ruled_out: vec![],
                    falsy_ruled_out: vec![(var_name.clone(), type_name)],
                };
                return if op == "==" {
                    eq_refs
                } else {
                    eq_refs.inverted()
                };
            }
            _ => {}
        }
        Refinements::empty()
    }

    /// Extract .has("key") refinements on shape types.
    fn extract_has_refinements(object: &SNode, args: &[SNode], scope: &TypeScope) -> Refinements {
        if let Node::Identifier(var_name) = &object.node {
            if let Node::StringLiteral(key) = &args[0].node {
                if let Some(Some(TypeExpr::Shape(fields))) = scope.get_var(var_name) {
                    if fields.iter().any(|f| f.name == *key && f.optional) {
                        let narrowed_fields: Vec<ShapeField> = fields
                            .iter()
                            .map(|f| {
                                if f.name == *key {
                                    ShapeField {
                                        name: f.name.clone(),
                                        type_expr: f.type_expr.clone(),
                                        optional: false,
                                    }
                                } else {
                                    f.clone()
                                }
                            })
                            .collect();
                        return Refinements {
                            truthy: vec![(
                                var_name.clone(),
                                Some(TypeExpr::Shape(narrowed_fields)),
                            )],
                            falsy: vec![],
                            ..Refinements::default()
                        };
                    }
                }
            }
        }
        Refinements::empty()
    }

    fn extract_schema_refinements(args: &[SNode], scope: &TypeScope) -> Refinements {
        let Node::Identifier(var_name) = &args[0].node else {
            return Refinements::empty();
        };
        let Some(schema_type) = schema_type_expr_from_node(&args[1], scope) else {
            return Refinements::empty();
        };
        let Some(Some(var_type)) = scope.get_var(var_name).cloned() else {
            return Refinements::empty();
        };

        let truthy = intersect_types(&var_type, &schema_type)
            .map(|ty| vec![(var_name.clone(), Some(ty))])
            .unwrap_or_default();
        let falsy = subtract_type(&var_type, &schema_type)
            .map(|ty| vec![(var_name.clone(), Some(ty))])
            .unwrap_or_default();

        Refinements {
            truthy,
            falsy,
            ..Refinements::default()
        }
    }

    /// Check whether a block definitely exits (delegates to the free function).
    pub(in crate::typechecker) fn block_definitely_exits(stmts: &[SNode]) -> bool {
        block_definitely_exits(stmts)
    }

    pub(in crate::typechecker) fn check_match_exhaustiveness(
        &mut self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
        span: Span,
    ) {
        // Detect pattern: match <expr>.variant { "VariantA" -> ... }
        let enum_name = match &value.node {
            Node::PropertyAccess { object, property } if property == "variant" => {
                // Infer the type of the object
                match self.infer_type(object, scope) {
                    Some(TypeExpr::Named(name)) => {
                        if scope.get_enum(&name).is_some() {
                            Some(name)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => {
                // Direct match on an enum value: match <expr> { ... }
                match self.infer_type(value, scope) {
                    Some(TypeExpr::Named(name)) if scope.get_enum(&name).is_some() => Some(name),
                    _ => None,
                }
            }
        };

        let Some(enum_name) = enum_name else {
            // Try union type exhaustiveness instead
            self.check_match_exhaustiveness_union(value, arms, scope, span);
            return;
        };
        let Some(variants) = scope.get_enum(&enum_name) else {
            return;
        };

        // Collect variant names covered by match arms
        let mut covered: Vec<String> = Vec::new();
        let mut has_wildcard = false;

        for arm in arms {
            match &arm.pattern.node {
                // String literal pattern (matching on .variant): "VariantA"
                Node::StringLiteral(s) => covered.push(s.clone()),
                // Identifier pattern acts as a wildcard/catch-all
                Node::Identifier(name)
                    if name == "_"
                        || !variants
                            .variants
                            .iter()
                            .any(|variant| variant.name == *name) =>
                {
                    has_wildcard = true;
                }
                // Direct enum construct pattern: EnumName.Variant
                Node::EnumConstruct { variant, .. } => covered.push(variant.clone()),
                // PropertyAccess pattern: EnumName.Variant (no args)
                Node::PropertyAccess { property, .. } => covered.push(property.clone()),
                _ => {
                    // Unknown pattern shape — conservatively treat as wildcard
                    has_wildcard = true;
                }
            }
        }

        if has_wildcard {
            return;
        }

        let missing: Vec<&String> = variants
            .variants
            .iter()
            .map(|variant| &variant.name)
            .filter(|variant| !covered.contains(variant))
            .collect();
        if !missing.is_empty() {
            let missing_str = missing
                .iter()
                .map(|s| format!("\"{}\"", s))
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(
                format!(
                    "Non-exhaustive match on enum {}: missing variants {}",
                    enum_name, missing_str
                ),
                span,
            );
        }
    }

    /// Check exhaustiveness for match on union types (e.g. `string | int | nil`).
    fn check_match_exhaustiveness_union(
        &mut self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(TypeExpr::Union(members)) = self.infer_type(value, scope) else {
            return;
        };
        // Only check unions of named types (string, int, nil, bool, etc.)
        if !members.iter().all(|m| matches!(m, TypeExpr::Named(_))) {
            return;
        }

        let mut has_wildcard = false;
        let mut covered_types: Vec<String> = Vec::new();

        for arm in arms {
            match &arm.pattern.node {
                // type_of(x) == "string" style patterns are common but hard to detect here
                // Literal patterns cover specific types
                Node::NilLiteral => covered_types.push("nil".into()),
                Node::BoolLiteral(_) => {
                    if !covered_types.contains(&"bool".into()) {
                        covered_types.push("bool".into());
                    }
                }
                Node::IntLiteral(_) => {
                    if !covered_types.contains(&"int".into()) {
                        covered_types.push("int".into());
                    }
                }
                Node::FloatLiteral(_) => {
                    if !covered_types.contains(&"float".into()) {
                        covered_types.push("float".into());
                    }
                }
                Node::StringLiteral(_) => {
                    if !covered_types.contains(&"string".into()) {
                        covered_types.push("string".into());
                    }
                }
                Node::Identifier(name) if name == "_" => {
                    has_wildcard = true;
                }
                _ => {
                    has_wildcard = true;
                }
            }
        }

        if has_wildcard {
            return;
        }

        let type_names: Vec<&str> = members
            .iter()
            .filter_map(|m| match m {
                TypeExpr::Named(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&&str> = type_names
            .iter()
            .filter(|t| !covered_types.iter().any(|c| c == **t))
            .collect();
        if !missing.is_empty() {
            let missing_str = missing
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(
                format!(
                    "Non-exhaustive match on union type: missing {}",
                    missing_str
                ),
                span,
            );
        }
    }

    /// Complete set of concrete variants that `type_of` may return, used as
    /// the reference for exhaustive-narrowing warnings on `unknown`.
    const UNKNOWN_CONCRETE_TYPES: &'static [&'static str] = &[
        "int", "string", "float", "bool", "nil", "list", "dict", "closure",
    ];

    /// Emit a warning if any `unknown`-typed variable in scope has been
    /// partially narrowed via `type_of(v) == "T"` checks but the current
    /// control-flow path reaches a never-returning site (`unreachable()`,
    /// a function with `Never` return, or a `throw`) without covering every
    /// concrete `type_of` variant.
    ///
    /// The ruled-out set must be non-empty — reaching `throw`/`unreachable`
    /// without any narrowing isn't an exhaustiveness claim, so it stays
    /// silent and avoids false positives on plain error paths.
    pub(in crate::typechecker) fn check_unknown_exhaustiveness(
        &mut self,
        scope: &TypeScope,
        span: Span,
        site_label: &str,
    ) {
        let entries = scope.collect_unknown_ruled_out();
        for (var_name, covered) in entries {
            if covered.is_empty() {
                continue;
            }
            // Only warn if `v` is still typed `unknown` at this point —
            // if it was fully narrowed elsewhere the ruled-out set is stale.
            if !matches!(
                scope.get_var(&var_name),
                Some(Some(TypeExpr::Named(n))) if n == "unknown"
            ) {
                continue;
            }
            let missing: Vec<&str> = Self::UNKNOWN_CONCRETE_TYPES
                .iter()
                .copied()
                .filter(|t| !covered.iter().any(|c| c == t))
                .collect();
            if missing.is_empty() {
                continue;
            }
            let missing_str = missing
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(
                format!(
                    "`{site}` reached but `{var}: unknown` was not fully narrowed — uncovered concrete type(s): {missing}",
                    site = site_label,
                    var = var_name,
                    missing = missing_str,
                ),
                span,
            );
        }
    }
}
