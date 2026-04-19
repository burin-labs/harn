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

impl TypeExpr {
    /// Render this type expression as a TypeScript-ish string.
    pub(crate) fn render(&self) -> String {
        match self {
            TypeExpr::Primitive(name) => normalize_primitive_name(name).to_string(),
            TypeExpr::Literal(value) => render_literal(value),
            TypeExpr::Array(inner) => {
                // Wrap unions / intersections so `(A | B)[]` parses correctly.
                match inner.as_ref() {
                    TypeExpr::Union(_) | TypeExpr::Intersection(_) => {
                        format!("({})[]", inner.render())
                    }
                    _ => format!("{}[]", inner.render()),
                }
            }
            TypeExpr::Union(members) => members
                .iter()
                .map(|member| member.render())
                .collect::<Vec<_>>()
                .join(" | "),
            TypeExpr::Intersection(members) => members
                .iter()
                .map(|member| {
                    let rendered = member.render();
                    // Parenthesise unions inside intersections for unambiguity.
                    if matches!(member, TypeExpr::Union(_)) {
                        format!("({rendered})")
                    } else {
                        rendered
                    }
                })
                .collect::<Vec<_>>()
                .join(" & "),
            TypeExpr::Object(fields) => {
                if fields.is_empty() {
                    "{}".to_string()
                } else {
                    let rendered = fields
                        .iter()
                        .map(render_object_field)
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!("{{ {rendered} }}")
                }
            }
            TypeExpr::Ref(name) => name.clone(),
            TypeExpr::Unknown => "unknown".to_string(),
        }
    }
}

fn render_object_field(field: &ObjectField) -> String {
    let marker = if field.required { "" } else { "?" };
    let mut rendered = format!("{}{}: {}", field.name, marker, field.ty.render());
    if let Some(comment) = field_inline_comment(field) {
        rendered.push_str(" /* ");
        rendered.push_str(&comment.replace("*/", "* /"));
        rendered.push_str(" */");
    }
    rendered
}

fn field_inline_comment(field: &ObjectField) -> Option<String> {
    let mut parts = Vec::new();
    parts.push(if field.required {
        "required".to_string()
    } else {
        "optional".to_string()
    });
    if let Some(description) = field
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(description.to_string());
    }
    if let Some(default) = &field.default {
        parts.push(format!("default {}", render_literal(default)));
    }
    if !field.examples.is_empty() {
        let rendered = field
            .examples
            .iter()
            .map(render_literal)
            .collect::<Vec<_>>()
            .join(", ");
        let label = if field.examples.len() == 1 {
            "example"
        } else {
            "examples"
        };
        parts.push(format!("{label} {rendered}"));
    }
    (!parts.is_empty()).then(|| parts.join(" — "))
}

fn normalize_primitive_name(raw: &str) -> &str {
    // Accept both JSON-Schema and TypeScript spellings; collapse to the TS
    // spelling. `integer`/`int` are both really numbers in JSON transport.
    match raw {
        "str" | "string" => "string",
        "int" | "integer" | "long" | "number" | "float" | "double" => "number",
        "bool" | "boolean" => "boolean",
        "nil" | "null" | "none" => "null",
        "dict" | "map" => "object",
        "list" | "array" => "unknown[]", // naked list with no items → unknown[]
        "any" => "any",
        "void" => "void",
        other => other,
    }
}

fn render_literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => {
            // TypeScript string literals use double quotes; escape backslash and quote.
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        // Non-scalar literals are unusual for JSON Schema `const`. Fall back
        // to serialised JSON so the model sees the exact shape.
        other => other.to_string(),
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
