// TypeExpr is a structural representation of a JSON Schema / OAS 3.1 type
// rendered as a TypeScript-ish type string. Anything the extractor cannot
// map cleanly becomes `Unknown` — never fabricate types the runtime won't
// honour.

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) enum TypeExpr {
    /// Primitive type name as used in TypeScript: string, number, boolean, null, any, unknown, void.
    Primitive(String),
    /// A literal value (JSON Schema `const`, or an enum member after fan-out).
    Literal(serde_json::Value),
    /// Array with an element type.
    Array(Box<TypeExpr>),
    /// `oneOf` / `anyOf` / multi-value `enum` → A | B | C.
    Union(Vec<TypeExpr>),
    /// `allOf` composition → A & B & C.
    Intersection(Vec<TypeExpr>),
    /// Nested object schema with named fields.
    Object(Vec<ObjectField>),
    /// Named reference to a reusable type declared in the ComponentRegistry.
    /// Resolved from `$ref` targets like `#/components/schemas/Foo` or from
    /// Harn-side `types/Foo` references.
    Ref(String),
    /// Fallback for shapes we cannot map cleanly.
    Unknown,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ObjectField {
    pub(crate) name: String,
    pub(crate) ty: TypeExpr,
    pub(crate) required: bool,
    pub(crate) description: Option<String>,
    pub(crate) default: Option<serde_json::Value>,
    pub(crate) examples: Vec<serde_json::Value>,
}

/// Collapse a list of `TypeExpr` into a single value: empty → `Unknown`,
/// single → that member, multiple → `wrap(members)`. Centralises the
/// 0/1/many fan-out used by `enum`, `oneOf`/`anyOf`, `allOf`, and the
/// multi-type `"type": ["string", "null"]` syntax in JSON Schema.
pub(super) fn collapse_members(
    members: Vec<TypeExpr>,
    wrap: fn(Vec<TypeExpr>) -> TypeExpr,
) -> TypeExpr {
    match members.len() {
        0 => TypeExpr::Unknown,
        1 => members.into_iter().next().expect("len == 1"),
        _ => wrap(members),
    }
}

/// If a union already contains a primitive `null`, keep it as-is; otherwise
/// return the type unchanged. This exists so we don't end up with `T | null | null`.
pub(super) fn merge_nullable(ty: TypeExpr) -> TypeExpr {
    if let TypeExpr::Union(ref members) = ty {
        let null_count = members
            .iter()
            .filter(|member| matches!(member, TypeExpr::Primitive(name) if name == "null"))
            .count();
        if null_count <= 1 {
            return ty;
        }
        // Dedupe trailing nulls.
        let mut seen_null = false;
        let deduped: Vec<TypeExpr> = members
            .iter()
            .filter(|member| match member {
                TypeExpr::Primitive(name) if name == "null" => {
                    if seen_null {
                        false
                    } else {
                        seen_null = true;
                        true
                    }
                }
                _ => true,
            })
            .cloned()
            .collect();
        return TypeExpr::Union(deduped);
    }
    ty
}
