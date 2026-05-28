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

/// Collapse a list of type members into a single `TypeExpr`: empty → the
/// caller-supplied `empty` (typically `Never` or another sentinel), single
/// → that member, multiple → `wrap(members)`. Used by the 0/1/many fan-out
/// pattern that recurs across union narrowing and intersection collapsing.
pub(super) fn collapse_members(
    members: Vec<TypeExpr>,
    empty: TypeExpr,
    wrap: fn(Vec<TypeExpr>) -> TypeExpr,
) -> TypeExpr {
    match members.len() {
        0 => empty,
        1 => members.into_iter().next().expect("len == 1"),
        _ => wrap(members),
    }
}

/// Like [`collapse_members`] but returns `None` for empty input — the
/// shape callers want when "no remaining members" should propagate as
/// "no narrowing".
pub(super) fn collapse_members_opt(
    members: Vec<TypeExpr>,
    wrap: fn(Vec<TypeExpr>) -> TypeExpr,
) -> Option<TypeExpr> {
    match members.len() {
        0 => None,
        1 => Some(members.into_iter().next().expect("len == 1")),
        _ => Some(wrap(members)),
    }
}

/// Simplify a union by flattening nested unions, removing `Never` members,
/// deduplicating, and collapsing single-member unions.
pub(super) fn simplify_union(members: Vec<TypeExpr>) -> TypeExpr {
    let mut filtered: Vec<TypeExpr> = Vec::new();
    for member in members {
        collect_simplified_union_member(member, &mut filtered);
    }
    collapse_members(filtered, TypeExpr::Never, TypeExpr::Union)
}

fn collect_simplified_union_member(member: TypeExpr, filtered: &mut Vec<TypeExpr>) {
    match member {
        TypeExpr::Never => {}
        TypeExpr::Union(members) => {
            for member in members {
                collect_simplified_union_member(member, filtered);
            }
        }
        member => {
            if !filtered.contains(&member) {
                filtered.push(member);
            }
        }
    }
}

/// Remove every `nil` arm from a type expression, including nested unions.
pub(super) fn without_nil(ty: &TypeExpr) -> InferredType {
    match ty {
        TypeExpr::Named(name) if name == "nil" => None,
        TypeExpr::Union(members) => {
            let non_nil: Vec<TypeExpr> = members.iter().filter_map(without_nil).collect();
            (!non_nil.is_empty()).then(|| simplify_union(non_nil))
        }
        other => Some(other.clone()),
    }
}

pub(super) fn contains_nil(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Named(name) if name == "nil" => true,
        TypeExpr::Union(members) => members.iter().any(contains_nil),
        _ => false,
    }
}

/// Whether a union member corresponds to the `type_of(...)` string
/// `target`. Recognises both the bare `Named(target)` form and the
/// parameterised constructor forms (`List<T>` → "list", `DictType` /
/// `Shape` → "dict", `FnType` → "closure", `Iter` → "iter",
/// `Generator` → "generator", `Stream` → "stream") plus literal
/// refinements (`LitString` → "string", `LitInt` → "int"). Without
/// this, `type_of(x) == "list"` would refuse to narrow `list<int> | int`
/// because the union member is `List(int)`, not `Named("list")`.
fn member_matches_runtime_kind(member: &TypeExpr, target: &str) -> bool {
    match (member, target) {
        (TypeExpr::Named(n), t) => n == t,
        (TypeExpr::List(_), "list") => true,
        (TypeExpr::DictType(_, _), "dict") => true,
        (TypeExpr::Shape(_), "dict") => true,
        (TypeExpr::FnType { .. }, "closure") => true,
        (TypeExpr::Iter(_), "iter") => true,
        (TypeExpr::Generator(_), "generator") => true,
        (TypeExpr::Stream(_), "stream") => true,
        (TypeExpr::LitString(_), "string") => true,
        (TypeExpr::LitInt(_), "int") => true,
        _ => false,
    }
}

/// Remove every union member whose runtime `type_of` would equal
/// `to_remove`, collapsing single-element unions. Returns `Some(Never)`
/// when all members are removed (exhausted).
pub(super) fn remove_from_union(members: &[TypeExpr], to_remove: &str) -> InferredType {
    let remaining: Vec<TypeExpr> = members
        .iter()
        .filter(|m| !member_matches_runtime_kind(m, to_remove))
        .cloned()
        .collect();
    Some(collapse_members(
        remaining,
        TypeExpr::Never,
        TypeExpr::Union,
    ))
}

/// Narrow a union to the (possibly parameterised) members whose
/// runtime `type_of` equals `target`. Returns the single matching
/// member when only one matches, a residual union when several match,
/// and `None` when no member matches.
pub(super) fn narrow_to_single(members: &[TypeExpr], target: &str) -> InferredType {
    let matched: Vec<TypeExpr> = members
        .iter()
        .filter(|m| member_matches_runtime_kind(m, target))
        .cloned()
        .collect();
    collapse_members_opt(matched, TypeExpr::Union)
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

/// Width subtyping for `Shape ∩ Shape`. Pulled out of `intersect_types`
/// so the arm body in the big match stays short and the merge invariant
/// reads top-down. See `intersect_types` for why we merge instead of
/// returning the schema's fields verbatim.
fn intersect_shapes(
    current_fields: &[ShapeField],
    schema_fields: &[ShapeField],
) -> Option<TypeExpr> {
    // Width subtyping: a value typed `current` already has every field
    // in `current_fields`; `schema_is(x, S)` only confirms the value
    // matches `schema_fields` (it adds information, it does not remove
    // it). Returning just `schema_fields` would drop the fields we
    // already knew about. Merge instead:
    //   - keep every field in `current_fields`
    //   - intersect overlapping field types (the value satisfies both
    //     annotations)
    //   - mark overlapping fields required when either side required
    //     them (the post-check value definitely has the field if either
    //     annotation says so)
    //   - append schema-only required fields (the check confirmed they
    //     are present); skip schema-only optional fields (the check
    //     tells us nothing new about them).
    let mut merged: Vec<ShapeField> = Vec::with_capacity(current_fields.len());
    for field in current_fields {
        if let Some(schema_field) = schema_fields.iter().find(|f| f.name == field.name) {
            let intersected = intersect_types(&field.type_expr, &schema_field.type_expr)?;
            merged.push(ShapeField {
                name: field.name.clone(),
                type_expr: intersected,
                optional: field.optional && schema_field.optional,
            });
        } else {
            merged.push(field.clone());
        }
    }
    for schema_field in schema_fields {
        if schema_field.optional {
            continue;
        }
        if merged.iter().any(|f| f.name == schema_field.name) {
            continue;
        }
        merged.push(schema_field.clone());
    }
    Some(TypeExpr::Shape(merged))
}

/// Distribute an intersection over a union: keep the members that overlap
/// the other side, dropping the empty intersections. Used by both halves
/// of the `(Union, _) / (_, Union)` symmetry — `flip` controls which side
/// `member` lands on in the recursive call so the operation stays
/// commutative without forcing equal-by-construction operands.
fn intersect_union_with(members: &[TypeExpr], other: &TypeExpr, flip: bool) -> Option<TypeExpr> {
    let kept = members
        .iter()
        .filter_map(|member| {
            if flip {
                intersect_types(other, member)
            } else {
                intersect_types(member, other)
            }
        })
        .collect::<Vec<_>>();
    collapse_members_opt(kept, TypeExpr::Union)
}

pub(super) fn intersect_types(current: &TypeExpr, schema_type: &TypeExpr) -> Option<TypeExpr> {
    // `owned<T>` is transparent to the underlying handle type at the
    // type-equality boundary (the ownership marker only drives the
    // HARN-OWN-005 leak lint, not value shape). Strip the marker before
    // descending so `intersect_types(owned<channel>, channel)` succeeds
    // the same way `intersect_types(channel, channel)` would — then
    // re-wrap if EITHER side carried it, since the intersection of a
    // marked-and-unmarked pair is the more specific (marked) form.
    match (current, schema_type) {
        (TypeExpr::Owned(c), TypeExpr::Owned(s)) => {
            return intersect_types(c, s).map(|t| TypeExpr::Owned(Box::new(t)));
        }
        (TypeExpr::Owned(inner), other) | (other, TypeExpr::Owned(inner)) => {
            return intersect_types(inner, other).map(|t| TypeExpr::Owned(Box::new(t)));
        }
        _ => {}
    }

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

        // Union distributes over the other side; the `flip` argument
        // preserves the original operand order so commutativity is
        // observed without aliasing the helper's recursive calls.
        (TypeExpr::Union(members), other) => intersect_union_with(members, other, false),
        (other, TypeExpr::Union(members)) => intersect_union_with(members, other, true),

        (TypeExpr::Named(left), TypeExpr::Named(right)) if left == right => {
            Some(TypeExpr::Named(left.clone()))
        }

        // Named-as-runtime-kind ↔ parameterised constructor. Each pair is
        // symmetric — the same `Some(parameterised.clone())` result
        // regardless of operand order — so collapse the two arms into one
        // OR-pattern. The kind table mirrors `member_matches_runtime_kind`
        // and `VmValue::type_name`; keep them aligned if either changes.
        (TypeExpr::Named(name), TypeExpr::Shape(fields))
        | (TypeExpr::Shape(fields), TypeExpr::Named(name))
            if name == "dict" =>
        {
            Some(TypeExpr::Shape(fields.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::DictType(key, value))
        | (TypeExpr::DictType(key, value), TypeExpr::Named(name))
            if name == "dict" =>
        {
            Some(TypeExpr::DictType(key.clone(), value.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::List(inner))
        | (TypeExpr::List(inner), TypeExpr::Named(name))
            if name == "list" =>
        {
            Some(TypeExpr::List(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Iter(inner))
        | (TypeExpr::Iter(inner), TypeExpr::Named(name))
            if name == "iter" =>
        {
            Some(TypeExpr::Iter(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Generator(inner))
        | (TypeExpr::Generator(inner), TypeExpr::Named(name))
            if name == "generator" || name == "Generator" =>
        {
            Some(TypeExpr::Generator(inner.clone()))
        }
        (TypeExpr::Named(name), TypeExpr::Stream(inner))
        | (TypeExpr::Stream(inner), TypeExpr::Named(name))
            if name == "stream" || name == "Stream" =>
        {
            Some(TypeExpr::Stream(inner.clone()))
        }

        // Parameterised ∩ Parameterised: keep the kind, intersect the
        // payload. `intersect_shapes` carries the width-subtyping merge.
        (TypeExpr::Shape(c), TypeExpr::Shape(s)) => intersect_shapes(c, s),
        (TypeExpr::List(c), TypeExpr::List(s)) => {
            intersect_types(c, s).map(|i| TypeExpr::List(Box::new(i)))
        }
        (TypeExpr::Iter(c), TypeExpr::Iter(s)) => {
            intersect_types(c, s).map(|i| TypeExpr::Iter(Box::new(i)))
        }
        (TypeExpr::Generator(c), TypeExpr::Generator(s)) => {
            intersect_types(c, s).map(|i| TypeExpr::Generator(Box::new(i)))
        }
        (TypeExpr::Stream(c), TypeExpr::Stream(s)) => {
            intersect_types(c, s).map(|i| TypeExpr::Stream(Box::new(i)))
        }
        (TypeExpr::DictType(ck, cv), TypeExpr::DictType(sk, sv)) => {
            let key = intersect_types(ck, sk)?;
            let value = intersect_types(cv, sv)?;
            Some(TypeExpr::DictType(Box::new(key), Box::new(value)))
        }

        // `unknown` and `any` are top types in the value-narrowing
        // direction — a successful `schema_is(x, S)` proves `x` matches
        // `S`, so the truthy branch can narrow to `S` regardless of how
        // wide the original type was. Without this, an annotated
        // `let x: unknown = …` stays `unknown` after `schema_is` (the
        // refinement set is empty), and field access on the narrowed
        // value triggers spurious "property access on unknown" warnings.
        (TypeExpr::Named(name), other) | (other, TypeExpr::Named(name))
            if matches!(name.as_str(), "unknown" | "any") =>
        {
            Some(other.clone())
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
        // `unknown` and `any` are top types: the falsy branch of
        // `schema_is(x, S)` knows `x` did not match `S`, but Harn has
        // no negative-type form to express "unknown except S". Stay at
        // `unknown` so the branch keeps requiring narrowing — without
        // this, the new top-type intersect arms would collapse the
        // falsy branch to `Never` and silence real diagnostics.
        TypeExpr::Named(name) if matches!(name.as_str(), "unknown" | "any") => {
            Some(current.clone())
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
