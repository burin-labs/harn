//! Union/intersection helpers and refinement-application logic.
//!
//! `simplify_union` / `remove_from_union` / `narrow_to_single` are the
//! workhorse helpers that the inference engine uses when narrowing union
//! types via refinements or match arms. `intersect_types` and
//! `subtract_type` power the schema-driven narrowing primitives that drive
//! `schema_is(x, T)` flow refinement. `apply_refinements` materializes a
//! list of narrowings into a `TypeScope`, preserving the pre-narrowing
//! type so reassignment can restore it.

use crate::ast::*;

use super::scope::{InferredType, TypeScope};

/// Simplify a union by removing `Never` members and collapsing.
pub(super) fn simplify_union(members: Vec<TypeExpr>) -> TypeExpr {
    let filtered: Vec<TypeExpr> = members
        .into_iter()
        .filter(|m| !matches!(m, TypeExpr::Never))
        .collect();
    match filtered.len() {
        0 => TypeExpr::Never,
        1 => filtered.into_iter().next().unwrap(),
        _ => TypeExpr::Union(filtered),
    }
}

/// Remove a named type from a union, collapsing single-element unions.
/// Returns `Some(Never)` when all members are removed (exhausted).
pub(super) fn remove_from_union(members: &[TypeExpr], to_remove: &str) -> InferredType {
    let remaining: Vec<TypeExpr> = members
        .iter()
        .filter(|m| !matches!(m, TypeExpr::Named(n) if n == to_remove))
        .cloned()
        .collect();
    match remaining.len() {
        0 => Some(TypeExpr::Never),
        1 => Some(remaining.into_iter().next().unwrap()),
        _ => Some(TypeExpr::Union(remaining)),
    }
}

/// Narrow a union to just one named type, if that type is a member.
pub(super) fn narrow_to_single(members: &[TypeExpr], target: &str) -> InferredType {
    if members
        .iter()
        .any(|m| matches!(m, TypeExpr::Named(n) if n == target))
    {
        Some(TypeExpr::Named(target.to_string()))
    } else {
        None
    }
}

/// Extract the variable name from a `type_of(x)` call.
pub(super) fn extract_type_of_var(node: &SNode) -> Option<String> {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        if name == "type_of" && args.len() == 1 {
            if let Node::Identifier(var) = &args[0].node {
                return Some(var.clone());
            }
        }
    }
    None
}

pub(super) fn intersect_types(current: &TypeExpr, schema_type: &TypeExpr) -> Option<TypeExpr> {
    match (current, schema_type) {
        // Literal intersections: two equal literals keep the literal.
        (TypeExpr::LitString(a), TypeExpr::LitString(b)) if a == b => {
            Some(TypeExpr::LitString(a.clone()))
        }
        (TypeExpr::LitInt(a), TypeExpr::LitInt(b)) if a == b => Some(TypeExpr::LitInt(*a)),
        // Intersecting a literal with its base type keeps the literal.
        (TypeExpr::LitString(s), TypeExpr::Named(n))
        | (TypeExpr::Named(n), TypeExpr::LitString(s))
            if n == "string" =>
        {
            Some(TypeExpr::LitString(s.clone()))
        }
        (TypeExpr::LitInt(v), TypeExpr::Named(n)) | (TypeExpr::Named(n), TypeExpr::LitInt(v))
            if n == "int" || n == "float" =>
        {
            Some(TypeExpr::LitInt(*v))
        }
        (TypeExpr::Union(members), other) => {
            let kept = members
                .iter()
                .filter_map(|member| intersect_types(member, other))
                .collect::<Vec<_>>();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next(),
                _ => Some(TypeExpr::Union(kept)),
            }
        }
        (other, TypeExpr::Union(members)) => {
            let kept = members
                .iter()
                .filter_map(|member| intersect_types(other, member))
                .collect::<Vec<_>>();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next(),
                _ => Some(TypeExpr::Union(kept)),
            }
        }
        (TypeExpr::Named(left), TypeExpr::Named(right)) if left == right => {
            Some(TypeExpr::Named(left.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Shape(fields)) if name == "dict" => {
            Some(TypeExpr::Shape(fields.clone()))
        }
        (TypeExpr::Shape(fields), TypeExpr::Named(name)) if name == "dict" => {
            Some(TypeExpr::Shape(fields.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::List(inner)) if name == "list" => {
            Some(TypeExpr::List(inner.clone()))
        }
        (TypeExpr::List(inner), TypeExpr::Named(name)) if name == "list" => {
            Some(TypeExpr::List(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Generator(inner))
            if name == "generator" || name == "Generator" =>
        {
            Some(TypeExpr::Generator(inner.clone()))
        }
        (TypeExpr::Generator(inner), TypeExpr::Named(name))
            if name == "generator" || name == "Generator" =>
        {
            Some(TypeExpr::Generator(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Stream(inner))
            if name == "stream" || name == "Stream" =>
        {
            Some(TypeExpr::Stream(inner.clone()))
        }
        (TypeExpr::Stream(inner), TypeExpr::Named(name))
            if name == "stream" || name == "Stream" =>
        {
            Some(TypeExpr::Stream(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::DictType(key, value)) if name == "dict" => {
            Some(TypeExpr::DictType(key.clone(), value.clone()))
        }
        (TypeExpr::DictType(key, value), TypeExpr::Named(name)) if name == "dict" => {
            Some(TypeExpr::DictType(key.clone(), value.clone()))
        }
        (TypeExpr::Shape(_), TypeExpr::Shape(fields)) => Some(TypeExpr::Shape(fields.clone())),
        (TypeExpr::List(current_inner), TypeExpr::List(schema_inner)) => {
            intersect_types(current_inner, schema_inner)
                .map(|inner| TypeExpr::List(Box::new(inner)))
        }
        (TypeExpr::Generator(current_inner), TypeExpr::Generator(schema_inner)) => {
            intersect_types(current_inner, schema_inner)
                .map(|inner| TypeExpr::Generator(Box::new(inner)))
        }
        (TypeExpr::Stream(current_inner), TypeExpr::Stream(schema_inner)) => {
            intersect_types(current_inner, schema_inner)
                .map(|inner| TypeExpr::Stream(Box::new(inner)))
        }
        (
            TypeExpr::DictType(current_key, current_value),
            TypeExpr::DictType(schema_key, schema_value),
        ) => {
            let key = intersect_types(current_key, schema_key)?;
            let value = intersect_types(current_value, schema_value)?;
            Some(TypeExpr::DictType(Box::new(key), Box::new(value)))
        }
        _ => None,
    }
}

pub(super) fn subtract_type(current: &TypeExpr, schema_type: &TypeExpr) -> Option<TypeExpr> {
    match current {
        TypeExpr::Union(members) => {
            let remaining = members
                .iter()
                .filter(|member| intersect_types(member, schema_type).is_none())
                .cloned()
                .collect::<Vec<_>>();
            match remaining.len() {
                0 => None,
                1 => remaining.into_iter().next(),
                _ => Some(TypeExpr::Union(remaining)),
            }
        }
        other if intersect_types(other, schema_type).is_some() => None,
        other => Some(other.clone()),
    }
}

/// A literal-typed field value used as a tagged-shape-union discriminant.
/// We carry a reduced enum rather than threading two types through every
/// helper — only string and int literal types are eligible discriminants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DiscriminantValue {
    Str(String),
    Int(i64),
}

impl DiscriminantValue {
    pub(super) fn from_type(t: &TypeExpr) -> Option<Self> {
        match t {
            TypeExpr::LitString(s) => Some(Self::Str(s.clone())),
            TypeExpr::LitInt(v) => Some(Self::Int(*v)),
            _ => None,
        }
    }
}

/// Auto-detect the discriminant field for a tagged shape union.
///
/// Rule: a field that (a) is present and non-optional in every shape
/// member, and (b) has a literal type (`LitString` / `LitInt`) distinct
/// per member, qualifies. If multiple fields qualify, pick the first in
/// source order across the first variant. Returns `None` if no member is
/// a `Shape` or no field qualifies.
///
/// The non-distinct check rules out cases like
///   { tag: "a", x: int } | { tag: "a", x: string }
/// where "tag" can't refine the union because both members share a value.
pub(super) fn discriminant_field(members: &[TypeExpr]) -> Option<String> {
    if members.len() < 2 {
        return None;
    }
    let shapes: Vec<&[ShapeField]> = members
        .iter()
        .map(|m| match m {
            TypeExpr::Shape(fields) => Some(fields.as_slice()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let first = shapes[0];
    'fields: for candidate in first {
        if candidate.optional {
            continue;
        }
        let Some(first_value) = DiscriminantValue::from_type(&candidate.type_expr) else {
            continue;
        };
        let mut seen: Vec<DiscriminantValue> = vec![first_value];
        for fields in &shapes[1..] {
            let Some(field) = fields.iter().find(|f| f.name == candidate.name) else {
                continue 'fields;
            };
            if field.optional {
                continue 'fields;
            }
            let Some(value) = DiscriminantValue::from_type(&field.type_expr) else {
                continue 'fields;
            };
            if seen.contains(&value) {
                continue 'fields;
            }
            seen.push(value);
        }
        return Some(candidate.name.clone());
    }
    None
}

/// Narrow a tagged shape union to the variant whose discriminant field
/// matches `tag_value`. Returns:
///   `Some(matched_shape, residual)` when exactly one variant matches;
///     `residual` is the union of every other member, collapsed via
///     [`simplify_union`] (so a single residual is a `Shape`, not a
///     length-1 `Union`).
///   `None` when the input isn't a recognised tagged shape union or no
///     variant matches the requested tag value.
pub(super) fn narrow_shape_union_by_tag(
    members: &[TypeExpr],
    tag_field: &str,
    tag_value: &DiscriminantValue,
) -> Option<(TypeExpr, TypeExpr)> {
    let mut matched: Option<TypeExpr> = None;
    let mut residual: Vec<TypeExpr> = Vec::with_capacity(members.len());
    for member in members {
        let TypeExpr::Shape(fields) = member else {
            return None;
        };
        let field = fields.iter().find(|f| f.name == tag_field)?;
        let value = DiscriminantValue::from_type(&field.type_expr)?;
        if &value == tag_value {
            if matched.is_some() {
                return None;
            }
            matched = Some(member.clone());
        } else {
            residual.push(member.clone());
        }
    }
    let matched = matched?;
    Some((matched, simplify_union(residual)))
}

/// Apply a list of refinements to a scope, tracking pre-narrowing types.
pub(super) fn apply_refinements(scope: &mut TypeScope, refinements: &[(String, InferredType)]) {
    for (var_name, narrowed_type) in refinements {
        // Save the pre-narrowing type so we can restore it on reassignment
        if !scope.narrowed_vars.contains_key(var_name) {
            if let Some(original) = scope.get_var(var_name).cloned() {
                scope.narrowed_vars.insert(var_name.clone(), original);
            }
        }
        scope.define_var(var_name, narrowed_type.clone());
    }
}
