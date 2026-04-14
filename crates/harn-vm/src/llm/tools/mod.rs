mod handle_local;
mod parse;
mod ts_value_parser;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::vm_value_to_json;
use crate::value::{VmError, VmValue};

use handle_local::coerce_integer_like_tool_args;
pub(crate) use handle_local::handle_tool_locally;
pub(crate) use parse::parse_text_tool_calls_with_tools;
#[cfg(test)]
pub(crate) use parse::{parse_bare_calls_in_body, parse_native_json_tool_calls};

/// Build an assistant message with tool_calls for the conversation history.
/// Format varies by API style (OpenAI-compatible vs Anthropic).
pub(crate) fn build_assistant_tool_message(
    text: &str,
    tool_calls: &[serde_json::Value],
    provider: &str,
) -> serde_json::Value {
    let is_anthropic = super::helpers::ResolvedProvider::resolve(provider).is_anthropic_style;
    if is_anthropic {
        // Anthropic format: content blocks with text and tool_use
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(serde_json::json!({"type": "text", "text": text}));
        }
        for tc in tool_calls {
            content.push(serde_json::json!({
                "type": "tool_use",
                "id": tc["id"],
                "name": tc["name"],
                "input": tc["arguments"],
            }));
        }
        serde_json::json!({"role": "assistant", "content": content})
    } else {
        // OpenAI-compatible format: assistant message with tool_calls array
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc["id"],
                    "type": "function",
                    "function": {
                        "name": tc["name"],
                        "arguments": serde_json::to_string(&tc["arguments"]).unwrap_or_default(),
                    }
                })
            })
            .collect();
        let msg = serde_json::json!({
            "role": "assistant",
            "content": if text.is_empty() { serde_json::Value::String(String::new()) } else { serde_json::json!(text) },
            "tool_calls": calls,
        });
        msg
    }
}

/// Build a durable assistant message for transcript/run-record storage.
/// Prefer canonical structured blocks when available so hosts can restore
/// richer assistant state without reparsing visible text.
pub(crate) fn build_assistant_response_message(
    text: &str,
    blocks: &[serde_json::Value],
    tool_calls: &[serde_json::Value],
    reasoning: Option<&str>,
    provider: &str,
) -> serde_json::Value {
    let mut message = if !tool_calls.is_empty() {
        build_assistant_tool_message(text, tool_calls, provider)
    } else if !blocks.is_empty() {
        serde_json::json!({
            "role": "assistant",
            "content": blocks,
        })
    } else {
        serde_json::json!({
            "role": "assistant",
            "content": text,
        })
    };
    if let Some(reasoning) = reasoning.filter(|value| !value.is_empty()) {
        message["reasoning"] = serde_json::json!(reasoning);
    }
    message
}

/// Build a tool result message for the conversation history.
pub(crate) fn build_tool_result_message(
    tool_call_id: &str,
    result: &str,
    provider: &str,
) -> serde_json::Value {
    let is_anthropic = super::helpers::ResolvedProvider::resolve(provider).is_anthropic_style;
    if is_anthropic {
        // Anthropic: tool_result inside a user message
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": result,
            }]
        })
    } else {
        // OpenAI-compatible: distinct "tool" role
        serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": result,
        })
    }
}

/// Normalize tool call arguments before dispatch.
///
/// The VM walks the active policy's
/// `tool_annotations[name].arg_schema.arg_aliases` table and rewrites any
/// aliases present in the arguments object to their canonical keys. This
/// is purely driven by pipeline declarations — the VM has no hardcoded
/// tool-name branches. If a tool isn't annotated, no aliases are rewritten.
pub(crate) fn normalize_tool_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let mut obj = match args.as_object() {
        Some(o) => o.clone(),
        None => return args.clone(),
    };

    if let Some(annotations) = crate::orchestration::current_tool_annotations(name) {
        for (alias, canonical) in &annotations.arg_schema.arg_aliases {
            if obj.contains_key(canonical) {
                continue;
            }
            if let Some(value) = obj.remove(alias) {
                obj.insert(canonical.clone(), value);
            }
        }
    }

    let mut normalized = serde_json::Value::Object(obj);
    coerce_integer_like_tool_args(&mut normalized);
    normalized
}

// ── Recursive type expression ───────────────────────────────────────────────
//
// TypeExpr is a structural representation of a JSON Schema / OAS 3.1 type that
// we know how to render as a TypeScript-ish type string. Anything the extractor
// cannot map cleanly becomes `Unknown`, which renders as `unknown` — we never
// fabricate a type the model could read but the runtime would not honour.

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

/// Registry of reusable named types discovered during schema extraction.
/// Each tool-contract prompt build produces one registry; the renderer emits
/// `type X = ...;` aliases at the top, and tool signatures can reference them
/// by name to keep individual signatures short.
#[derive(Clone, Debug, Default)]
pub(crate) struct ComponentRegistry {
    /// Registered types by their resolved short name. Names are derived from
    /// the last path segment of the `$ref` (e.g. `#/components/schemas/Foo` → `Foo`).
    types: BTreeMap<String, TypeExpr>,
    /// Insertion order, so `type` aliases render in a deterministic stable order.
    order: Vec<String>,
    /// Set of names currently being resolved. Used to break cycles: if we
    /// encounter the same ref while it's still being resolved, we emit a
    /// `Ref(name)` placeholder and leave the alias definition to the outer
    /// call. Without this, a recursive schema would infinite-loop.
    in_progress: BTreeSet<String>,
}

impl ComponentRegistry {
    fn register(&mut self, name: String, ty: TypeExpr) {
        if !self.types.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.types.insert(name, ty);
    }

    fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }

    /// Render all registered types as `type Name = Expr;` lines in insertion
    /// order. Returns an empty string when the registry is empty.
    pub(crate) fn render_aliases(&self) -> String {
        if self.order.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for name in &self.order {
            if let Some(ty) = self.types.get(name) {
                out.push_str(&format!("type {} = {};\n", name, ty.render()));
            }
        }
        out
    }
}

/// Extract the short name from a JSON Pointer `$ref`. Supports common shapes:
/// `#/components/schemas/Foo`, `#/definitions/Foo`, and Harn-native
/// `types/Foo` / `#/types/Foo`. Returns None if we cannot find a name-like tail.
fn ref_name_from_pointer(pointer: &str) -> Option<String> {
    let stripped = pointer.trim_start_matches('#').trim_start_matches('/');
    let last = stripped.rsplit('/').next()?;
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

/// Resolve a JSON Pointer `$ref` against a root schema document. Supports
/// fragments like `#/components/schemas/Foo` by walking each path segment.
fn resolve_json_ref<'a>(
    root: &'a serde_json::Value,
    pointer: &str,
) -> Option<&'a serde_json::Value> {
    let stripped = pointer.trim_start_matches('#').trim_start_matches('/');
    if stripped.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in stripped.split('/') {
        let decoded = segment.replace("~1", "/").replace("~0", "~");
        current = match current {
            serde_json::Value::Object(obj) => obj.get(&decoded)?,
            serde_json::Value::Array(arr) => {
                let idx: usize = decoded.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current)
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
                .map(|m| m.render())
                .collect::<Vec<_>>()
                .join(" | "),
            TypeExpr::Intersection(members) => members
                .iter()
                .map(|m| {
                    let rendered = m.render();
                    // Parenthesise unions inside intersections for unambiguity.
                    if matches!(m, TypeExpr::Union(_)) {
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

/// Convert a JSON Schema fragment into a TypeExpr, recursing through
/// oneOf/anyOf/allOf, items, properties, const/enum, and $ref. The `root`
/// document is required to resolve ref pointers; the `registry` accumulates
/// named types so they can be rendered as top-of-prompt `type X = ...` aliases.
fn json_schema_to_type_expr(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> TypeExpr {
    let obj = match schema.as_object() {
        Some(obj) => obj,
        None => {
            // A bare string in the schema slot means "type name" — be forgiving.
            if let Some(s) = schema.as_str() {
                return TypeExpr::Primitive(s.to_string());
            }
            return TypeExpr::Unknown;
        }
    };

    // $ref — resolve against root, register the resolved type under its short
    // name, and return a Ref so the rendered prompt can share the alias.
    if let Some(serde_json::Value::String(pointer)) = obj.get("$ref") {
        if let Some(name) = ref_name_from_pointer(pointer) {
            if !registry.contains(&name) && !registry.in_progress.contains(&name) {
                if let Some(resolved) = resolve_json_ref(root, pointer) {
                    registry.in_progress.insert(name.clone());
                    let expanded = json_schema_to_type_expr(resolved, root, registry);
                    registry.in_progress.remove(&name);
                    registry.register(name.clone(), expanded);
                }
            }
            return TypeExpr::Ref(name);
        }
        return TypeExpr::Unknown;
    }

    // const — single-literal type.
    if let Some(c) = obj.get("const") {
        return TypeExpr::Literal(c.clone());
    }

    // enum — union of literals.
    if let Some(serde_json::Value::Array(values)) = obj.get("enum") {
        let members: Vec<TypeExpr> = values
            .iter()
            .map(|v| TypeExpr::Literal(v.clone()))
            .collect();
        return match members.len() {
            0 => TypeExpr::Unknown,
            1 => members.into_iter().next().unwrap(),
            _ => TypeExpr::Union(members),
        };
    }

    // oneOf / anyOf — union. Render both the same way for our purposes
    // (model doesn't care about structural-disambiguation semantics here).
    for key in ["oneOf", "anyOf"] {
        if let Some(serde_json::Value::Array(variants)) = obj.get(key) {
            let members: Vec<TypeExpr> = variants
                .iter()
                .map(|v| json_schema_to_type_expr(v, root, registry))
                .filter(|t| !matches!(t, TypeExpr::Unknown))
                .collect();
            return match members.len() {
                0 => TypeExpr::Unknown,
                1 => members.into_iter().next().unwrap(),
                _ => merge_nullable(TypeExpr::Union(members)),
            };
        }
    }

    // allOf — intersection of all component schemas.
    if let Some(serde_json::Value::Array(variants)) = obj.get("allOf") {
        let members: Vec<TypeExpr> = variants
            .iter()
            .map(|v| json_schema_to_type_expr(v, root, registry))
            .filter(|t| !matches!(t, TypeExpr::Unknown))
            .collect();
        return match members.len() {
            0 => TypeExpr::Unknown,
            1 => members.into_iter().next().unwrap(),
            _ => TypeExpr::Intersection(members),
        };
    }

    // type — may be a string (`"string"`) or an array of strings (`["string", "null"]`).
    let nullable = obj
        .get("nullable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let core_type = match obj.get("type") {
        Some(serde_json::Value::Array(type_list)) => {
            let primitives: Vec<TypeExpr> = type_list
                .iter()
                .filter_map(|v| v.as_str().map(|s| TypeExpr::Primitive(s.to_string())))
                .collect();
            match primitives.len() {
                0 => TypeExpr::Unknown,
                1 => primitives.into_iter().next().unwrap(),
                _ => TypeExpr::Union(primitives),
            }
        }
        Some(serde_json::Value::String(t)) => match t.as_str() {
            "array" => {
                let item_schema = obj.get("items").cloned().unwrap_or(serde_json::json!({}));
                let item_type = json_schema_to_type_expr(&item_schema, root, registry);
                TypeExpr::Array(Box::new(item_type))
            }
            "object" => {
                if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
                    let required_set: BTreeSet<String> = obj
                        .get("required")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut fields: Vec<ObjectField> = props
                        .iter()
                        .map(|(name, sub_schema)| ObjectField {
                            name: name.clone(),
                            ty: json_schema_to_type_expr(sub_schema, root, registry),
                            required: required_set.contains(name),
                            description: sub_schema
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            default: sub_schema.get("default").cloned(),
                            examples: sub_schema
                                .as_object()
                                .map(extract_examples)
                                .unwrap_or_default(),
                        })
                        .collect();
                    // Required first, then optional, stable within each group.
                    fields.sort_by_key(|f| !f.required);
                    TypeExpr::Object(fields)
                } else {
                    TypeExpr::Primitive("object".to_string())
                }
            }
            other => TypeExpr::Primitive(other.to_string()),
        },
        _ => TypeExpr::Unknown,
    };

    if nullable {
        merge_nullable(TypeExpr::Union(vec![
            core_type,
            TypeExpr::Primitive("null".to_string()),
        ]))
    } else {
        core_type
    }
}

/// If a union already contains a primitive `null`, keep it as-is; otherwise
/// return the type unchanged. This exists so we don't end up with `T | null | null`.
fn merge_nullable(ty: TypeExpr) -> TypeExpr {
    if let TypeExpr::Union(ref members) = ty {
        let null_count = members
            .iter()
            .filter(|m| matches!(m, TypeExpr::Primitive(name) if name == "null"))
            .count();
        if null_count <= 1 {
            return ty;
        }
        // Dedupe trailing nulls.
        let mut seen_null = false;
        let deduped: Vec<TypeExpr> = members
            .iter()
            .filter(|m| match m {
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

/// Extract parameter info from a Harn VmValue dict (tool_registry entry).
/// Harn tool definitions default to `required: true`; a param is optional only
/// when its dict explicitly contains `required: false`. The per-param dict
/// carries a JSON-Schema-ish subset (type / enum / const / items / properties
/// / oneOf / anyOf / allOf / default / examples / $ref) which we recursively
/// lift into TypeExpr. The `root_json` is the whole tool-registry converted
/// to JSON so `$ref` pointers can resolve against it.
fn extract_params_from_vm_dict(
    td: &BTreeMap<String, VmValue>,
    root_json: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> Vec<ToolParamSchema> {
    let mut params = Vec::new();
    if let Some(VmValue::Dict(pd)) = td.get("parameters") {
        for (pname, pval) in pd.iter() {
            let (ty, desc, required, default, examples) = if let VmValue::Dict(pdef) = pval {
                let desc = pdef
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let required = match pdef.get("required") {
                    Some(VmValue::Bool(b)) => *b,
                    _ => true,
                };
                let json = vm_dict_to_json(pdef);
                let ty = json_schema_to_type_expr(&json, root_json, registry);
                let default = json.get("default").cloned();
                let examples = extract_examples_vm(pdef);
                (ty, desc, required, default, examples)
            } else {
                // Simple string description — treat as required string.
                (
                    TypeExpr::Primitive("string".to_string()),
                    pval.display(),
                    true,
                    None,
                    Vec::new(),
                )
            };
            params.push(ToolParamSchema {
                name: pname.clone(),
                ty,
                description: desc,
                required,
                default,
                examples,
            });
        }
    }
    // Required params first so the rendered TS signature — and any downstream
    // consumers that iterate `params` in order — sees the critical fields up
    // front. Stable-alphabetical within each group (BTreeMap iteration is
    // already alphabetical).
    params.sort_by_key(|p| !p.required);
    params
}

/// Convert a VmValue dict fragment into a serde_json::Value using the crate's
/// canonical VmValue → JSON conversion (re-exported via `super::vm_value_to_json`
/// at the top of this file). We wrap the dict contents in `VmValue::Dict` so
/// the single shared conversion path handles every field uniformly.
fn vm_dict_to_json(dict: &BTreeMap<String, VmValue>) -> serde_json::Value {
    vm_value_to_json(&VmValue::Dict(Rc::new(dict.clone())))
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolParamSchema {
    pub(crate) name: String,
    pub(crate) ty: TypeExpr,
    pub(crate) description: String,
    pub(crate) required: bool,
    pub(crate) default: Option<serde_json::Value>,
    /// JSON Schema `examples` (plural) or `example` (singular, legacy). Shown
    /// inline after the description so models see concrete valid values
    /// alongside the type constraint.
    pub(crate) examples: Vec<serde_json::Value>,
}

/// Pull examples from a JSON-schema-ish fragment, accepting both plural
/// `examples: [...]` (OAS 3.1 preferred) and the legacy singular `example: v`.
fn extract_examples(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<serde_json::Value> {
    if let Some(serde_json::Value::Array(arr)) = obj.get("examples") {
        return arr.clone();
    }
    if let Some(single) = obj.get("example") {
        return vec![single.clone()];
    }
    Vec::new()
}

/// Pull examples from a VmValue dict, same dual-key convention.
fn extract_examples_vm(pdef: &BTreeMap<String, VmValue>) -> Vec<serde_json::Value> {
    if let Some(VmValue::List(items)) = pdef.get("examples") {
        return items.iter().map(vm_value_to_json).collect();
    }
    if let Some(single) = pdef.get("example") {
        return vec![vm_value_to_json(single)];
    }
    Vec::new()
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ToolSchema {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) params: Vec<ToolParamSchema>,
    /// When true, render as a compact one-liner (name + params type + first
    /// sentence of description) instead of the full TypeScript declaration with
    /// JSDoc.  Tools marked compact are still fully dispatchable — only the
    /// prompt rendering changes.  The model can call `tool_schema({ name })`
    /// to get the full description on demand.
    pub(crate) compact: bool,
}

fn collect_vm_tool_schemas(
    tools_val: Option<&VmValue>,
    registry: &mut ComponentRegistry,
) -> Vec<ToolSchema> {
    // Build a JSON mirror of the root tool-registry so `$ref` pointers inside
    // individual param schemas can resolve against sibling `types` / `definitions`
    // / `components.schemas` declarations.
    let root_json = match tools_val {
        Some(value) => vm_value_to_json(value),
        None => serde_json::Value::Null,
    };

    let entries: Vec<&VmValue> = match tools_val {
        Some(VmValue::List(list)) => list.iter().collect(),
        Some(VmValue::Dict(d)) => {
            if let Some(VmValue::List(tools)) = d.get("tools") {
                tools.iter().collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    entries
        .into_iter()
        .filter_map(|value| match value {
            VmValue::Dict(td) => {
                let name = td.get("name")?.display();
                let description = td
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params = extract_params_from_vm_dict(td, &root_json, registry);
                let compact = td
                    .get("compact")
                    .map(|v| matches!(v, VmValue::Bool(true)))
                    .unwrap_or(false);
                Some(ToolSchema {
                    name,
                    description,
                    params,
                    compact,
                })
            }
            _ => None,
        })
        .collect()
}
fn schema_description_from_json(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("description")
                .and_then(|inner| inner.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_default()
}

fn extract_params_from_provider_input_schema(
    provider_input_schema: &serde_json::Value,
    root: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> Vec<ToolParamSchema> {
    let required_set: BTreeSet<String> = provider_input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    provider_input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            let mut params = properties
                .iter()
                .map(|(name, value)| {
                    let examples = value.as_object().map(extract_examples).unwrap_or_default();
                    ToolParamSchema {
                        name: name.clone(),
                        ty: json_schema_to_type_expr(value, root, registry),
                        description: schema_description_from_json(value),
                        required: required_set.contains(name),
                        default: value.get("default").cloned(),
                        examples,
                    }
                })
                .collect::<Vec<_>>();
            // Stable sort: required first (matching vm dict extractor), then
            // alphabetical within each group so the signature is deterministic.
            params.sort_by(|a, b| {
                (!a.required)
                    .cmp(&!b.required)
                    .then_with(|| a.name.cmp(&b.name))
            });
            params
        })
        .unwrap_or_default()
}

fn collect_provider_declared_tool_schemas(
    provider_tools: Option<&[serde_json::Value]>,
    registry: &mut ComponentRegistry,
) -> Vec<ToolSchema> {
    provider_tools
        .unwrap_or(&[])
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function");
            let name = function
                .and_then(|value| value.get("name"))
                .or_else(|| tool.get("name"))
                .and_then(|value| value.as_str())?;
            let description = function
                .and_then(|value| value.get("description"))
                .or_else(|| tool.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let provider_input_schema = function
                .and_then(|value| value.get("parameters"))
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            // Provider-declared JSON schemas hang off the tool wrapper itself
            // (or the nested `function` object), so `$ref` must resolve
            // against siblings such as `components.schemas` on that wrapper.
            let root = tool.clone();
            Some(ToolSchema {
                name: name.to_string(),
                description,
                params: extract_params_from_provider_input_schema(
                    &provider_input_schema,
                    &root,
                    registry,
                ),
                compact: false,
            })
        })
        .collect()
}

/// Collect the full tool schema set AND the reusable type registry populated
/// by any `$ref` encounters during extraction. Callers that only need the
/// list of schemas can ignore the registry.
pub(crate) fn collect_tool_schemas_with_registry(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
) -> (Vec<ToolSchema>, ComponentRegistry) {
    let mut registry = ComponentRegistry::default();
    let mut merged = collect_vm_tool_schemas(tools_val, &mut registry);
    let mut seen = merged
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<BTreeSet<_>>();

    for schema in collect_provider_declared_tool_schemas(native_tools, &mut registry) {
        if seen.insert(schema.name.clone()) {
            merged.push(schema);
        }
    }

    merged.sort_by(|a, b| a.name.cmp(&b.name));
    (merged, registry)
}

pub(crate) fn collect_tool_schemas(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
) -> Vec<ToolSchema> {
    collect_tool_schemas_with_registry(tools_val, native_tools).0
}

/// Validate that all required parameters (those without defaults) are present
/// in the tool call arguments. Returns `Ok(())` when valid, or an error string
/// listing the missing parameters.
pub(crate) fn validate_tool_args(
    tool_name: &str,
    args: &serde_json::Value,
    schemas: &[ToolSchema],
) -> Result<(), String> {
    let Some(schema) = schemas.iter().find(|s| s.name == tool_name) else {
        return Ok(()); // Unknown tool — handled by the unknown-tool error path
    };
    let obj = args.as_object();
    let missing: Vec<&str> = schema
        .params
        .iter()
        .filter(|p| p.required && p.default.is_none())
        .filter(|p| obj.is_none_or(|o| !o.contains_key(&p.name) || o[&p.name].is_null()))
        .map(|p| p.name.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Tool '{}' is missing required parameter(s): {}. \
             Provide all required parameters and try again.",
            tool_name,
            missing.join(", ")
        ))
    }
}

/// Build a runtime-owned tool-calling contract prompt.
/// The runtime injects this block so prompt templates do not need to carry
/// stale tool syntax examples that can drift from actual parser behavior.
///
/// Layout:
///   ## Tool Calling Contract
///   Active mode: text (authoritative — ignore older prompt text).
///
///   ## Shared types           (only if any $ref aliases were registered)
///   type Foo = ...;
///
///   ## Available tools
///   declare function edit(args: { path: string /* required — Relative path */; ... }): string;
///   /** Tool description only. */
///
///   ## How to call tools      (only in text mode when include_format = true)
///   Call a tool as a plain TypeScript function call at the start of a line ...
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
    mode: &str,
    require_action: bool,
    tool_examples: Option<&str>,
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));

    if mode == "native" {
        // Native mode: the provider's tool-calling channel is the preferred
        // path. But many local OpenAI-compatible servers (Ollama with bare
        // `{{ .Prompt }}` templates being the canonical case) silently drop
        // the `tools` parameter because the chat template doesn't reference
        // it. When that happens the model receives zero tool guidance and
        // either narrates or guesses at a tool-call format from training.
        // Including the text-mode protocol + schemas inline guarantees the
        // model always has a fallback that works regardless of how the host
        // serves the request. The downstream parser accepts either channel.
        prompt.push_str(
            "Prefer the provider's native tool-calling channel when it is available. \
             If the channel does not surface to you (some local OpenAI-compatible \
             servers strip the tools parameter), emit a `<tool_call>name({ ... })</tool_call>` \
             block in the assistant message and the runtime will execute it from there.\n\n",
        );
    } else {
        // Front-load format instructions and examples BEFORE schemas so that
        // weaker models encounter the calling convention early, while attention
        // is strongest.
    }
    prompt.push_str(TS_CALL_CONTRACT_HELP);
    if require_action {
        prompt.push_str(
            "\nThis turn is action-gated. If tools are available, open your response \
             with a tool call (native channel or `<tool_call>` block), not prose. Do not \
             emit raw source code, diffs, JSON, or a <done> block before the first tool \
             call.\n",
        );
    }
    if let Some(examples) = tool_examples {
        let trimmed = examples.trim();
        if !trimmed.is_empty() {
            prompt.push_str("\n## Tool call examples\n\n");
            prompt.push_str(trimmed);
            prompt.push_str("\n\n");
        }
    }

    let (schemas, registry) = collect_tool_schemas_with_registry(tools_val, native_tools);

    let aliases = registry.render_aliases();
    if !aliases.is_empty() {
        prompt.push_str("## Shared types\n\n");
        prompt.push_str(&aliases);
        prompt.push('\n');
    }

    let (expanded, compact): (Vec<_>, Vec<_>) = schemas.iter().partition(|s| !s.compact);

    prompt.push_str("## Available tools\n\n");
    for schema in &expanded {
        prompt.push_str(&render_text_tool_schema(schema));
    }

    if !compact.is_empty() {
        prompt.push_str(
            "## Other tools (call directly — parameters are intuitive, or call tool_schema for details)\n\n",
        );
        for schema in &compact {
            prompt.push_str(&render_compact_text_tool_schema(schema));
        }
        prompt.push('\n');
    }

    prompt
}

fn render_text_tool_schema(schema: &ToolSchema) -> String {
    let mut rendered = String::new();
    let args_type = build_tool_args_type(&schema.params);
    rendered.push_str(&format!(
        "declare function {}(args: {}): string;\n",
        schema.name,
        args_type.render()
    ));
    if !schema.description.trim().is_empty() {
        rendered.push_str("/**\n");
        for line in schema.description.lines() {
            rendered.push_str(&format!(" * {line}\n"));
        }
        rendered.push_str(" */\n");
    }
    rendered.push('\n');
    rendered
}

fn render_compact_text_tool_schema(schema: &ToolSchema) -> String {
    let args_type = build_tool_args_type(&schema.params);
    let summary = schema
        .description
        .split(&['.', '\n'][..])
        .next()
        .unwrap_or("")
        .trim();
    format!(
        "- `{}({})` — {}\n",
        schema.name,
        args_type.render(),
        summary,
    )
}

/// Build the single-arg TypeScript object type that a tool takes. Each
/// top-level parameter becomes a field in the object (optional via `?`, with
/// a JSDoc @example rendered by the containing comment block), with required
/// fields listed first for consistency with the per-param comment order.
fn build_tool_args_type(params: &[ToolParamSchema]) -> TypeExpr {
    let fields: Vec<ObjectField> = params
        .iter()
        .map(|p| ObjectField {
            name: p.name.clone(),
            ty: p.ty.clone(),
            required: p.required,
            description: if p.description.is_empty() {
                None
            } else {
                Some(p.description.clone())
            },
            default: p.default.clone(),
            examples: p.examples.clone(),
        })
        .collect();
    TypeExpr::Object(fields)
}

/// Help text for the fenceless TS call syntax. Declared as a constant so tests
/// can assert on its content without duplicating the string.
///
/// The text is written to minimise backtick-counting demands on weaker models:
/// prose references to single-character syntax use quoted descriptions
/// ('backtick', 'double quote') and the ONE code example is embedded in the
/// paragraph without any wrapping fence. Wrapping the example in a Markdown
/// fenced code block caused confusion because models had to balance several
/// levels of backticks at once.
pub(crate) const TS_CALL_CONTRACT_HELP: &str = "
## Response protocol

Every response must be a sequence of these tags, with only whitespace between them:

<tool_call>
name({ key: value })
</tool_call>

<assistant_prose>
Short narration. Optional.
</assistant_prose>

<done>##DONE##</done>

Rules the runtime enforces:

- No text, code, diffs, JSON, or reasoning outside these tags. Any stray content is rejected with structured feedback.
- `<tool_call>` wraps exactly one bare call `name({ key: value })`. Do not quote or JSON-encode the call. Use heredoc `<<TAG` ... `TAG` for multiline string fields — raw content, no escaping. Place TAG at the start of the closing line; closing punctuation like `},` may follow on that same line.
- `<assistant_prose>` is optional and must be brief. Never paste source code, file contents, command transcripts, or long plans here — wrap those in the relevant tool call instead.
- `<done>##DONE##</done>` signals task completion. Emit it only after a successful verifying tool call; the runtime rejects it otherwise.
- Do not prefix calls with labels like `tool_code:`, `python:`, `shell:`, or any language tag, and do not wrap tool calls in Markdown fences.
- Prefer `<tool_call>` over `<assistant_prose>`. If you have nothing concrete to say, omit prose entirely.

Example of a well-formed response:

<assistant_prose>Creating the test file.</assistant_prose>
<tool_call>
edit({ action: \"create\", path: \"tests/test_foo.py\", content: <<EOF
def test_foo():
    assert foo() == 42
EOF
})
</tool_call>

## Task ledger

The runtime maintains a durable `<task_ledger>` of the user's deliverables (injected into each turn above this prompt). The `<done>` block is REJECTED while any deliverable is `open` or `blocked`. Use the always-available `ledger` tool to mutate it:

- `ledger({ action: \"add\", text: \"what needs to happen\" })` — declare a new sub-deliverable.
- `ledger({ action: \"mark\", id: \"deliverable-N\", status: \"done\" })` — mark a deliverable complete after a real tool call satisfied it.
- `ledger({ action: \"mark\", id: \"deliverable-N\", status: \"dropped\", note: \"why\" })` — escape hatch when scope truly changed; the note is required.
- `ledger({ action: \"rationale\", text: \"one-sentence answer to why the user will call this done\" })` — commit to an interpretation of the success criterion.
- `ledger({ action: \"note\", text: \"observation worth remembering across turns\" })` — durable cross-stage memory.

Prefer marking deliverables done only AFTER a concrete tool call demonstrates completion (an edit landed, a run() returned exit 0, a read confirmed an invariant). Don't mark done on prose alone.
";

pub(crate) fn vm_tools_to_native(
    tools_val: &VmValue,
    provider: &str,
) -> Result<Vec<serde_json::Value>, VmError> {
    // Accept either a tool_registry dict or a list of tool dicts
    let tools_list = match tools_val {
        VmValue::Dict(d) => {
            // tool_registry -- extract tools list
            match d.get("tools") {
                Some(VmValue::List(list)) => list.as_ref().clone(),
                _ => Vec::new(),
            }
        }
        VmValue::List(list) => list.as_ref().clone(),
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tools must be a tool_registry or a list of tool definition dicts",
            ))));
        }
    };

    let mut native_tools = Vec::new();
    for tool in &tools_list {
        match tool {
            VmValue::Dict(entry) => {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params = entry.get("parameters").and_then(|v| v.as_dict());
                let output_schema = entry.get("outputSchema").map(vm_value_to_json);

                let input_schema = vm_build_json_schema(params);

                // Use API style, not provider name, to determine schema format.
                // Anthropic uses {name, description, input_schema}; everything
                // else (OpenAI-compatible) uses {type: "function", function: {...}}.
                let is_anthropic =
                    super::helpers::ResolvedProvider::resolve(provider).is_anthropic_style;
                if is_anthropic {
                    let mut tool_json = serde_json::json!({
                        "name": name,
                        "description": description,
                        "input_schema": input_schema,
                    });
                    if let Some(output_schema) = output_schema {
                        tool_json["x-harn-output-schema"] = output_schema;
                    }
                    native_tools.push(tool_json);
                } else {
                    let mut tool_json = serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": input_schema,
                        }
                    });
                    if let Some(output_schema) = output_schema {
                        tool_json["function"]["x-harn-output-schema"] = output_schema;
                    }
                    native_tools.push(tool_json);
                }
            }
            VmValue::String(_) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tools must be declared as tool definition dicts or a tool_registry",
                ))));
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tools must contain only tool definition dicts",
                ))));
            }
        }
    }
    Ok(native_tools)
}

fn vm_build_json_schema(params: Option<&BTreeMap<String, VmValue>>) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    if let Some(params) = params {
        for (name, type_val) in params {
            let type_str = type_val.display();
            let json_type = match type_str.as_str() {
                "int" | "integer" => "integer",
                "float" | "number" => "number",
                "bool" | "boolean" => "boolean",
                "list" | "array" => "array",
                "dict" | "object" => "object",
                _ => "string",
            };
            properties.insert(name.clone(), serde_json::json!({"type": json_type}));
            required.push(serde_json::Value::String(name.clone()));
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests;
