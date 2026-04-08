use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::vm_value_to_json;
use crate::value::{VmError, VmValue};

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
/// Handles alias mapping so tool schemas and host implementations stay consistent
/// regardless of which parameter names the model chooses.
pub(crate) fn normalize_tool_args(name: &str, args: &serde_json::Value) -> serde_json::Value {
    let mut obj = match args.as_object() {
        Some(o) => o.clone(),
        None => return args.clone(),
    };

    if name == "edit" {
        // Normalize action aliases: mode, command -> action
        if !obj.contains_key("action") {
            if let Some(v) = obj.remove("mode").or_else(|| obj.remove("command")) {
                obj.insert("action".to_string(), v);
            }
        }

        // For patch actions: normalize find->old_string, content->new_string
        let action = obj
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if action == "patch" || action == "replace" {
            if !obj.contains_key("old_string") {
                if let Some(v) = obj.remove("find") {
                    obj.insert("old_string".to_string(), v);
                }
            }
            if !obj.contains_key("new_string") {
                if let Some(v) = obj.remove("content") {
                    obj.insert("new_string".to_string(), v);
                }
            }
        }

        // Normalize file->path alias
        if !obj.contains_key("path") {
            if let Some(v) = obj.remove("file") {
                obj.insert("path".to_string(), v);
            }
        }
    }

    if name == "run" || name == "exec" {
        if !obj.contains_key("command") {
            if let Some(v) = obj.remove("args").or_else(|| obj.remove("argv")) {
                obj.insert("command".to_string(), v);
            }
        }

        let command_value = obj.get("command").cloned();
        let args_value = obj
            .get("args")
            .cloned()
            .or_else(|| obj.get("argv").cloned());
        if let Some(command) = normalize_run_command(command_value.as_ref(), args_value.as_ref()) {
            obj.insert("command".to_string(), serde_json::json!(command));
        }
        obj.remove("args");
        obj.remove("argv");
    }

    serde_json::Value::Object(obj)
}

fn normalize_run_command(
    command_value: Option<&serde_json::Value>,
    fallback_value: Option<&serde_json::Value>,
) -> Option<String> {
    let command_parts = command_value
        .and_then(run_command_tokens)
        .unwrap_or_default();
    let fallback_parts = fallback_value
        .and_then(run_command_tokens)
        .unwrap_or_default();
    let parts = if command_parts.is_empty() {
        fallback_parts
    } else if fallback_parts.is_empty() {
        command_parts
    } else {
        let mut combined = fallback_parts;
        combined.extend(command_parts);
        combined
    };

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn run_command_tokens(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Array(parts) => {
            let tokens = parts
                .iter()
                .filter_map(|part| part.as_str())
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            (!tokens.is_empty()).then_some(tokens)
        }
        serde_json::Value::String(text) => run_command_tokens_from_str(text),
        _ => None,
    }
}

fn run_command_tokens_from_str(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        if let Ok(parts) = serde_json::from_str::<Vec<String>>(trimmed) {
            let tokens = parts
                .into_iter()
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if !tokens.is_empty() {
                return Some(tokens);
            }
        }
    }

    if (trimmed.contains('[') || trimmed.contains(']') || trimmed.contains("\\\""))
        && trimmed.contains('"')
    {
        let tokens = extract_quoted_tokens(trimmed);
        if !tokens.is_empty() {
            return Some(tokens);
        }
    }

    Some(vec![trimmed.to_string()])
}

fn extract_quoted_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escape = false;

    for ch in text.chars() {
        if !in_quote {
            if ch == '"' {
                in_quote = true;
                current.clear();
            }
            continue;
        }

        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            '"' => {
                if !current.trim().is_empty() {
                    tokens.push(current.trim().to_string());
                }
                current.clear();
                in_quote = false;
            }
            _ => current.push(ch),
        }
    }

    tokens
}

fn resolve_local_tool_path(path: &str) -> std::path::PathBuf {
    let candidate = std::path::PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(cwd) =
        crate::stdlib::process::current_execution_context().and_then(|context| context.cwd)
    {
        return std::path::PathBuf::from(cwd).join(candidate);
    }
    crate::stdlib::process::resolve_source_relative_path(path)
}

/// Handle read-only tools locally in the VM without bridging to the host.
/// This reduces latency and split-brain for passive operations.
pub(crate) fn handle_tool_locally(name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        "read_file" | "read" => {
            let path = args
                .get("path")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                return Some("Error: missing path parameter".to_string());
            }
            let resolved = resolve_local_tool_path(path);
            if resolved.is_dir() {
                return match std::fs::read_dir(&resolved) {
                    Ok(entries) => {
                        let mut names: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| {
                                let name = e.file_name().to_string_lossy().to_string();
                                if e.path().is_dir() {
                                    format!("{}/", name)
                                } else {
                                    name
                                }
                            })
                            .collect();
                        names.sort();
                        Some(names.join("\n"))
                    }
                    Err(e) => Some(format!("Error: cannot list directory '{}': {}", path, e)),
                };
            }
            // Parse optional offset (1-based line number) and limit
            let offset = args
                .get("offset")
                .and_then(|v| v.as_i64())
                .map(|v| v.max(1) as usize)
                .unwrap_or(1);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_i64())
                .map(|v| v.clamp(1, 2000) as usize)
                .unwrap_or(2000);
            match std::fs::read_to_string(&resolved) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let total_lines = lines.len();
                    let start_idx = (offset - 1).min(total_lines);
                    let end_idx = (start_idx + limit).min(total_lines);
                    let mut numbered: String = lines[start_idx..end_idx]
                        .iter()
                        .enumerate()
                        .map(|(i, line)| format!("{}\t{}", start_idx + i + 1, line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if end_idx < total_lines {
                        numbered.push_str(&format!(
                            "\n\n[... {} more lines not shown. Use offset={} to continue reading]",
                            total_lines - end_idx,
                            end_idx + 1
                        ));
                    }
                    Some(numbered)
                }
                Err(e) => Some(format!("Error: cannot read file '{}': {}", path, e)),
            }
        }
        "list_directory" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let resolved = resolve_local_tool_path(path);
            match std::fs::read_dir(&resolved) {
                Ok(entries) => {
                    let mut names: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            if e.path().is_dir() {
                                format!("{}/", name)
                            } else {
                                name
                            }
                        })
                        .collect();
                    names.sort();
                    Some(names.join("\n"))
                }
                Err(e) => Some(format!("Error: cannot list directory '{}': {}", path, e)),
            }
        }
        _ => None,
    }
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
                        .map(|f| {
                            let marker = if f.required { "" } else { "?" };
                            format!("{}{}: {}", f.name, marker, f.ty.render())
                        })
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

impl ToolParamSchema {
    fn rendered_default_suffix(&self) -> String {
        match &self.default {
            Some(v) => format!(" = {}", render_literal(v)),
            None => String::new(),
        }
    }

    fn rendered_examples_suffix(&self) -> String {
        if self.examples.is_empty() {
            return String::new();
        }
        let rendered = self
            .examples
            .iter()
            .map(render_literal)
            .collect::<Vec<_>>()
            .join(", ");
        if self.examples.len() == 1 {
            format!(" Example: {rendered}.")
        } else {
            format!(" Examples: {rendered}.")
        }
    }
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

fn extract_params_from_native_schema(
    input_schema: &serde_json::Value,
    root: &serde_json::Value,
    registry: &mut ComponentRegistry,
) -> Vec<ToolParamSchema> {
    let required_set: BTreeSet<String> = input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    input_schema
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

fn collect_native_tool_schemas(
    native_tools: Option<&[serde_json::Value]>,
    registry: &mut ComponentRegistry,
) -> Vec<ToolSchema> {
    native_tools
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
            let input_schema = function
                .and_then(|value| value.get("parameters"))
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            // For native schemas the root is the tool wrapper itself (or the
            // function object), so `$ref` can resolve against sibling
            // `components.schemas` entries if the provider included them.
            let root = tool.clone();
            Some(ToolSchema {
                name: name.to_string(),
                description,
                params: extract_params_from_native_schema(&input_schema, &root, registry),
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

    for schema in collect_native_tool_schemas(native_tools, &mut registry) {
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
///   declare function edit(args: { ... }): string;
///   /** @param path (required) - Relative path. Example: "a.go". */
///   /** @param action (required) - Which edit. */
///   ...
///
///   ## How to call tools      (only in text mode when include_format = true)
///   Call a tool as a plain TypeScript function call at the start of a line ...
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
    mode: &str,
    include_format: bool,
    tool_examples: Option<&str>,
) -> String {
    let mut prompt = String::from("\n\n## Tool Calling Contract\n");
    prompt.push_str(&format!(
        "Active mode: `{mode}`. Follow this runtime-owned contract even if older prompt text suggests another tool syntax.\n\n"
    ));

    // Front-load format instructions and examples BEFORE schemas so that
    // weaker models encounter the calling convention early, while attention
    // is strongest.
    if mode == "native" {
        prompt.push_str("Use the provider's native tool-calling channel for tool invocations.\n\n");
    } else if include_format {
        prompt.push_str(TS_CALL_CONTRACT_HELP);
        if let Some(examples) = tool_examples {
            let trimmed = examples.trim();
            if !trimmed.is_empty() {
                prompt.push_str("\n## Tool call examples\n\n");
                prompt.push_str(trimmed);
                prompt.push_str("\n\n");
            }
        }
    }

    let (schemas, registry) = collect_tool_schemas_with_registry(tools_val, native_tools);

    let aliases = registry.render_aliases();
    if !aliases.is_empty() {
        prompt.push_str("## Shared types\n\n");
        prompt.push_str(&aliases);
        prompt.push('\n');
    }

    // Split into expanded and compact tools for progressive disclosure.
    let (expanded, compact): (Vec<_>, Vec<_>) = schemas.iter().partition(|s| !s.compact);

    prompt.push_str("## Available tools\n\n");

    for schema in &expanded {
        // Required params come first, then optional. Each tool is presented
        // as a single-arg TypeScript function declaration so the model sees
        // optionality, enums, nested objects, and array item types directly
        // in the type.
        let args_type = build_tool_args_type(&schema.params);
        prompt.push_str(&format!(
            "declare function {}(args: {}): string;\n",
            schema.name,
            args_type.render()
        ));
        if !schema.description.trim().is_empty() {
            prompt.push_str("/**\n");
            for line in schema.description.lines() {
                prompt.push_str(&format!(" * {line}\n"));
            }
            for p in schema.params.iter() {
                let tag = if p.required { "required" } else { "optional" };
                let default_suffix = p.rendered_default_suffix();
                let examples_suffix = p.rendered_examples_suffix();
                if p.description.is_empty()
                    && default_suffix.is_empty()
                    && examples_suffix.is_empty()
                {
                    continue;
                }
                prompt.push_str(&format!(
                    " * @param {} ({tag}){}{} {}{}\n",
                    p.name,
                    if default_suffix.is_empty() { "" } else { " " },
                    default_suffix,
                    if p.description.is_empty() {
                        "".to_string()
                    } else {
                        format!("— {}", p.description.trim())
                    },
                    examples_suffix,
                ));
            }
            prompt.push_str(" */\n");
        } else if schema.params.iter().any(|p| !p.description.is_empty()) {
            prompt.push_str("/**\n");
            for p in schema.params.iter() {
                if p.description.is_empty() {
                    continue;
                }
                let tag = if p.required { "required" } else { "optional" };
                let examples_suffix = p.rendered_examples_suffix();
                prompt.push_str(&format!(
                    " * @param {} ({tag}) — {}{}\n",
                    p.name,
                    p.description.trim(),
                    examples_suffix,
                ));
            }
            prompt.push_str(" */\n");
        }
        prompt.push('\n');
    }

    // Render compact tools as a brief summary section.
    if !compact.is_empty() {
        prompt.push_str("## Other tools (call directly — parameters are intuitive, or call tool_schema for details)\n\n");
        for schema in &compact {
            let args_type = build_tool_args_type(&schema.params);
            // Take first sentence of description as summary
            let summary = schema
                .description
                .split(&['.', '\n'][..])
                .next()
                .unwrap_or("")
                .trim();
            prompt.push_str(&format!(
                "- `{}({})` — {}\n",
                schema.name,
                args_type.render(),
                summary,
            ));
        }
        prompt.push('\n');
    }

    prompt
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
## How to call tools

Write `name({ key: value })` on its own line. Use heredoc for multiline strings:
edit({ action: \"create\", path: \"test.go\", content: <<EOF
package main
func Test() {}
EOF
})

- Heredoc `<<TAG` ... `TAG`: raw content, no escaping needed. TAG alone on closing line.
- Double quotes for single-line strings. Trailing commas optional.
- Tool calls work with or without Markdown fences.
- Prefer tool calls over prose.
";

/// Result of parsing a prose-interleaved TS tool-call stream.
///
/// The scanner walks the model's text once and splits it into three
/// streams for the caller:
///   - `calls`: the parsed structured tool calls.
///   - `errors`: diagnostics for malformed call attempts.
///   - `prose`: the original text with every successfully-parsed call
///     expression removed, whitespace around the hole collapsed. This is
///     what should be shown as "the agent's answer" and replayed back into
///     conversation history — tool calls are structured data, not narration.
pub(crate) struct TextToolParseResult {
    pub calls: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub prose: String,
}

/// Parse every fenceless TS tool call found in a model's text response.
///
/// The model writes prose and tool calls intermixed. A tool call is a
/// TypeScript function expression `name({...})` whose `name` matches a
/// registered tool AND whose call-site `(` immediately follows the name at
/// the start of a line (leading whitespace allowed). Tool names inside
/// Markdown fenced code blocks (```` ``` ````) or inline code spans (`` ` ``)
/// are treated as narration and skipped.
///
/// The returned `prose` field is the input text with every successfully
/// parsed call expression excised — useful for building a clean "what the
/// model said" string separate from the structured tool-call list.
/// Strip leaked thinking tags from model output. Some models (Qwen, Gemma)
/// emit `</think>` or `<think>` markers in their response text when the
/// streaming transport merges thinking and content channels. These tags
/// break tool-call parsing because they appear between or before valid
/// tool invocations.
fn strip_thinking_tags(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("<think>") && !text.contains("</think>") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut result = text.to_string();
    // Remove <think>...</think> blocks entirely (leaked thinking)
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result[start..].find("</think>") {
            result.replace_range(start..start + end + "</think>".len(), "");
        } else {
            // Unclosed <think> — remove just the tag
            result.replace_range(start..start + "<think>".len(), "");
        }
    }
    // Remove stray </think> tags
    while result.contains("</think>") {
        result = result.replace("</think>", "");
    }
    std::borrow::Cow::Owned(result)
}

pub(crate) fn parse_text_tool_calls_with_tools(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    // Strip leaked thinking tags before any parsing
    let cleaned = strip_thinking_tags(text);
    let text = cleaned.as_ref();

    if let Some(unwrapped) = unwrap_exact_code_wrapper(text) {
        let result = parse_text_tool_calls_with_tools(unwrapped, tools_val);
        if !result.calls.is_empty() || !result.errors.is_empty() {
            return result;
        }
    }
    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|s| s.name)
        .collect();
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    // Byte ranges [start, end) to excise from the original text to produce
    // the `prose` field. We collect them during the scan and apply them at
    // the end in a single pass so the scanner's index arithmetic stays
    // simple.
    let mut call_ranges: Vec<(usize, usize)> = Vec::new();

    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut at_line_start = true;
    let mut in_inline_code = false;
    // Track fence line byte ranges so we can strip them from prose when
    // they bracket tool calls. Each entry is (fence_start, fence_end,
    // calls_before_count). After parsing, if calls were added between a
    // pair of fences, both fence lines are added to call_ranges.
    let mut fence_lines: Vec<(usize, usize, usize)> = Vec::new();

    while i < bytes.len() {
        if at_line_start && !in_inline_code {
            // Skip leading whitespace on this line (for call detection).
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            // Skip Markdown fence lines (```lang / ```) themselves — they
            // are never tool calls. But do NOT skip the content between
            // fences: models routinely wrap tool calls in ```python fences,
            // and skipping those silently drops ~24% of real calls.
            if bytes.get(j) == Some(&b'`')
                && bytes.get(j + 1) == Some(&b'`')
                && bytes.get(j + 2) == Some(&b'`')
            {
                let fence_start = i;
                // Consume the fence line itself.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                fence_lines.push((fence_start, i, calls.len()));
                at_line_start = true;
                continue;
            }
            {
                // Strip common model-generated prefixes before the actual
                // tool name.  Models sometimes emit `call:edit(...)` or
                // `tool:read(...)` instead of bare `edit(...)`.
                // Also strip angle brackets: `<read(...)>` — common with
                // Qwen models that wrap tool calls in XML-like tags.
                let mut k = j;
                // Strip leading angle bracket
                if bytes.get(k) == Some(&b'<') {
                    k += 1;
                    // Skip whitespace after <
                    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                        k += 1;
                    }
                }
                for prefix in ["call:", "tool:", "use:"] {
                    if text[k..].starts_with(prefix) {
                        k += prefix.len();
                        // Also skip optional whitespace after the prefix.
                        while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                            k += 1;
                        }
                        break;
                    }
                }
                // Candidate tool call at line start: <ident>( directly.
                if let Some(name_len) = ident_length(&bytes[k..]) {
                    if bytes.get(k + name_len) == Some(&b'(')
                        && known
                            .contains(std::str::from_utf8(&bytes[k..k + name_len]).unwrap_or(""))
                    {
                        let name = std::str::from_utf8(&bytes[k..k + name_len])
                            .unwrap()
                            .to_string();
                        match parse_ts_call_from(&text[k..], name.clone()) {
                            Ok((arguments, consumed)) => {
                                calls.push(serde_json::json!({
                                    "id": format!("tc_{}", calls.len()),
                                    "name": name,
                                    "arguments": arguments,
                                }));
                                // Record the call's byte range so we can
                                // strip it from `prose` below. Use j (original
                                // line start) to also excise the prefix.
                                // Also consume trailing `>` if the model
                                // wrapped the call in angle brackets.
                                let mut end = k + consumed;
                                while end < bytes.len()
                                    && (bytes[end] == b' ' || bytes[end] == b'\t')
                                {
                                    end += 1;
                                }
                                if end < bytes.len() && bytes[end] == b'>' {
                                    end += 1;
                                }
                                call_ranges.push((j, end));
                                i = end;
                                at_line_start = bytes.get(i.saturating_sub(1)) == Some(&b'\n');
                                continue;
                            }
                            Err(msg) => {
                                errors.push(msg);
                                // Advance past the offending `(` so we can
                                // keep scanning and (hopefully) find the
                                // next well-formed call.
                                i = k + name_len + 1;
                                at_line_start = false;
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Inline code spans: `code`. Tool names inside backtick-wrapped
        // prose are references, not invocations.
        if bytes[i] == b'`' {
            in_inline_code = !in_inline_code;
            at_line_start = false;
            i += 1;
            continue;
        }

        if bytes[i] == b'\n' {
            at_line_start = true;
        } else if !bytes[i].is_ascii_whitespace() {
            at_line_start = false;
        }
        i += 1;
    }

    // Strip fence lines that bracketed tool calls. We look at consecutive
    // fence pairs: if new calls appeared between them, both fence lines
    // should be stripped from prose (they were just formatting wrappers).
    for pair in fence_lines.windows(2) {
        let (open_start, open_end, calls_before_open) = pair[0];
        let (_close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close > calls_before_open {
            // Calls were parsed between these fences — strip both fence lines
            call_ranges.push((open_start, open_end));
            call_ranges.push((_close_start, close_end));
        }
    }
    // Also handle a trailing unclosed fence (model didn't close it)
    if fence_lines.len() % 2 == 1 {
        let (start, end, calls_before) = *fence_lines.last().unwrap();
        if calls.len() > calls_before {
            call_ranges.push((start, end));
        }
    }
    // Sort ranges so prose-building iterates them in order
    call_ranges.sort_by_key(|r| r.0);

    // Also strip empty fence pairs (```lang\n```) that don't contain calls.
    // Models often emit these as failed tool-call attempts. If left in prose
    // they accumulate in conversation history and cause duplication loops.
    for pair in fence_lines.windows(2) {
        let (open_start, _open_end, calls_before_open) = pair[0];
        let (_close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close == calls_before_open {
            // No calls between these fences — it's an empty block, strip both
            call_ranges.push((open_start, close_end));
        }
    }
    call_ranges.sort_by_key(|r| r.0);
    // Deduplicate overlapping ranges
    call_ranges.dedup_by(|b, a| a.0 == b.0);

    // Build `prose` by copying every byte range NOT inside a parsed-call
    // window. Collapse runs of blank lines that form purely because a call
    // was removed so the final prose reads naturally.
    let prose = if call_ranges.is_empty() {
        strip_empty_fences(text)
    } else {
        let mut buf = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (start, end) in &call_ranges {
            if *start > cursor {
                buf.push_str(&text[cursor..*start]);
            }
            cursor = *end;
        }
        if cursor < text.len() {
            buf.push_str(&text[cursor..]);
        }
        collapse_blank_lines(&strip_empty_fences(&buf))
            .trim()
            .to_string()
    };

    // Fallback: if no text-format calls were found, check whether the model
    // emitted native OpenAI-style function calling JSON as raw text. This
    // happens when models trained on function calling ignore the text-format
    // instructions and emit `[{"id":"call_...","function":{...}}]` instead.
    // Rather than wasting an iteration nudging the model, parse and execute
    // the calls directly.
    if calls.is_empty() && errors.is_empty() {
        let native_calls = parse_native_json_tool_calls(text, &known);
        if !native_calls.is_empty() {
            return TextToolParseResult {
                calls: native_calls,
                errors: Vec::new(),
                prose: String::new(),
            };
        }
    }

    TextToolParseResult {
        calls,
        errors,
        prose,
    }
}

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Looks for `[{"id":"call_...","function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text.
fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> Vec<serde_json::Value> {
    let mut results = Vec::new();

    // Find the first `[{` or `{"id":"call_` in the text
    let json_start = text
        .find("[{\"id\":")
        .or_else(|| text.find("[{\"id\":"))
        .or_else(|| text.find("{\"id\":\"call_"));

    let Some(start) = json_start else {
        return results;
    };

    // Try to parse as JSON array or single object
    let json_text = &text[start..];
    let parsed: Option<Vec<serde_json::Value>> = serde_json::from_str(json_text)
        .ok()
        .or_else(|| {
            // Try single object
            serde_json::from_str::<serde_json::Value>(json_text)
                .ok()
                .map(|v| vec![v])
        })
        .or_else(|| {
            // The JSON might have trailing text. Try to find the closing bracket.
            for end in (start + 10..text.len()).rev() {
                let slice = &text[start..=end];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                    return Some(arr);
                }
            }
            None
        });

    let Some(items) = parsed else {
        return results;
    };

    for item in items {
        let func = item.get("function").and_then(|f| f.as_object());
        let Some(func) = func else { continue };
        let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.is_empty() || !known_tools.contains(name) {
            continue;
        }
        // Arguments may be a JSON string (OpenAI format) or an object
        let arguments = match func.get("arguments") {
            Some(serde_json::Value::String(s)) => {
                serde_json::from_str(s).unwrap_or(serde_json::Value::Object(Default::default()))
            }
            Some(obj @ serde_json::Value::Object(_)) => obj.clone(),
            _ => serde_json::Value::Object(Default::default()),
        };
        let call_id = item
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("native_fallback");
        results.push(serde_json::json!({
            "id": call_id,
            "name": name,
            "arguments": arguments,
        }));
    }

    results
}

fn unwrap_exact_code_wrapper(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let newline = rest.find('\n')?;
        let after_opener = &rest[newline + 1..];
        let inner = after_opener.strip_suffix("```")?;
        return Some(inner.trim());
    }
    let inner = trimmed.strip_prefix('`')?.strip_suffix('`')?;
    if inner.contains('`') {
        return None;
    }
    Some(inner.trim())
}

/// Collapse runs of ≥3 consecutive newlines down to 2 (one blank line). Used
/// to tidy the `prose` output after tool-call ranges are excised, so the
/// removed bytes don't leave an ugly vertical gap between surrounding prose.
/// Strip empty Markdown fence pairs (```lang\n``` or ```lang\n\n```) from text.
/// Models sometimes emit these as failed tool-call attempts. If left in prose
/// they accumulate in conversation history and cause duplication loops.
fn strip_empty_fences(text: &str) -> String {
    // Match: optional whitespace, ```, optional lang tag, newline(s), ```, newline
    let re = regex::Regex::new(r"(?m)^[ \t]*```[^\n]*\n\s*```[ \t]*\n?").unwrap();
    re.replace_all(text, "").to_string()
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(ch);
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    out
}

/// Length of a JavaScript-ish identifier starting at bytes[0]. Returns None
/// if the first byte is not a valid identifier start.
fn ident_length(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' {
            i += 1;
        } else {
            break;
        }
    }
    Some(i)
}

/// Parse a full `name(args)` TS call expression starting at the beginning of
/// `text`. Returns the parsed argument JSON and the number of bytes consumed
/// (from the start of the name through the closing paren), or an error with
/// a diagnostic suitable to show the model.
fn parse_ts_call_from(text: &str, name: String) -> Result<(serde_json::Value, usize), String> {
    let bytes = text.as_bytes();
    let paren_open = name.len();
    if bytes.get(paren_open) != Some(&b'(') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(` expected immediately after the tool name."
        ));
    }
    let mut p = TsValueParser::new(&text[paren_open + 1..]);
    p.skip_ws_and_comments();
    // An empty arg list `name()` is legal and produces an empty object.
    let args_value = if p.peek() == Some(b')') {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        p.parse_value().map_err(|e| {
            format!(
                "TOOL CALL PARSE ERROR: `{name}(...)` — {e}. \
                 Tool arguments must be a TypeScript object literal: `{{ key: value, key: value }}`."
            )
        })?
    };
    p.skip_ws_and_comments();
    if p.peek() != Some(b')') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — missing closing `)`. \
             Every tool call must be a complete TypeScript expression."
        ));
    }
    let consumed_in_parser = p.position();
    let total_consumed = paren_open + 1 + consumed_in_parser + 1; // +1 for the ')'

    // Coerce positional / non-object calls: the tool contract is that every
    // call takes a single object literal argument. If the model wrote a bare
    // scalar like `lookup("README.md")`, error precisely — we do not silently
    // promote positional args any more.
    match args_value {
        serde_json::Value::Object(map) => Ok((serde_json::Value::Object(map), total_consumed)),
        other => Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — expected an object literal argument, \
             got `{}`. Wrap the value in braces: `{name}({{ key: value }})`.",
            other
        )),
    }
}

/// Minimal recursive-descent parser for a TypeScript value expression. Handles
/// object and array literals, string literals (double-quoted and single-quoted),
/// template literals (backticks) including escape sequences, numbers (int and
/// float, negative), booleans, null, undefined, and identifier keys inside
/// object literals.
struct TsValueParser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl<'a> TsValueParser<'a> {
    fn new(text: &'a str) -> Self {
        TsValueParser {
            bytes: text.as_bytes(),
            text,
            pos: 0,
        }
    }

    fn position(&self) -> usize {
        self.pos
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(b) = self.peek() {
                if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Line comments
            if self.peek() == Some(b'/') && self.bytes.get(self.pos + 1) == Some(&b'/') {
                while let Some(b) = self.peek() {
                    if b == b'\n' {
                        self.pos += 1;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // Block comments
            if self.peek() == Some(b'/') && self.bytes.get(self.pos + 1) == Some(&b'*') {
                self.pos += 2;
                while self.pos + 1 < self.bytes.len() {
                    if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn parse_value(&mut self) -> Result<serde_json::Value, String> {
        self.skip_ws_and_comments();
        let c = self.peek().ok_or("unexpected end of input")?;
        match c {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' | b'\'' => self.parse_string_literal(c),
            b'`' => self.parse_template_literal(),
            b'<' if self.bytes.get(self.pos + 1) == Some(&b'<') => self.parse_heredoc(),
            b't' | b'f' => self.parse_boolean(),
            b'n' => self.parse_null(),
            b'u' => self.parse_undefined(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!(
                "unexpected character `{}` starting a value",
                other as char
            )),
        }
    }

    fn parse_object(&mut self) -> Result<serde_json::Value, String> {
        // consume '{'
        self.advance();
        let mut map = serde_json::Map::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b'}') {
                self.advance();
                return Ok(serde_json::Value::Object(map));
            }
            // Key: bare identifier OR string literal.
            let key = if let Some(b) = self.peek() {
                if b == b'"' || b == b'\'' {
                    match self.parse_string_literal(b)? {
                        serde_json::Value::String(s) => s,
                        _ => unreachable!(),
                    }
                } else {
                    let len = ident_length(&self.bytes[self.pos..])
                        .ok_or("expected an object key (identifier or string) inside `{ ... }`")?;
                    let k = self.text[self.pos..self.pos + len].to_string();
                    self.pos += len;
                    k
                }
            } else {
                return Err("unexpected end of input inside object literal".to_string());
            };
            self.skip_ws_and_comments();
            // TS shorthand `{ foo }` is legal but rare for our tool calls; we
            // disallow it to keep the contract explicit.
            if self.peek() != Some(b':') {
                return Err(format!(
                    "expected `:` after key `{key}` inside object literal"
                ));
            }
            self.advance();
            self.skip_ws_and_comments();
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws_and_comments();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                    continue;
                }
                Some(b'}') => {
                    self.advance();
                    return Ok(serde_json::Value::Object(map));
                }
                Some(other) => {
                    return Err(format!(
                        "expected `,` or `}}` after value inside object literal, got `{}`",
                        other as char
                    ));
                }
                None => {
                    return Err("unexpected end of input inside object literal".to_string());
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<serde_json::Value, String> {
        self.advance(); // '['
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b']') {
                self.advance();
                return Ok(serde_json::Value::Array(items));
            }
            items.push(self.parse_value()?);
            self.skip_ws_and_comments();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                    continue;
                }
                Some(b']') => {
                    self.advance();
                    return Ok(serde_json::Value::Array(items));
                }
                Some(other) => {
                    return Err(format!(
                        "expected `,` or `]` inside array literal, got `{}`",
                        other as char
                    ));
                }
                None => {
                    return Err("unexpected end of input inside array literal".to_string());
                }
            }
        }
    }

    fn parse_string_literal(&mut self, quote: u8) -> Result<serde_json::Value, String> {
        self.advance(); // opening quote
        let mut out = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated string literal".to_string()),
                Some(b) if b == quote => return Ok(serde_json::Value::String(out)),
                Some(b'\\') => {
                    let esc = self
                        .advance()
                        .ok_or("unterminated escape sequence in string literal")?;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'0' => out.push('\0'),
                        b'\\' => out.push('\\'),
                        b'\'' => out.push('\''),
                        b'"' => out.push('"'),
                        b'`' => out.push('`'),
                        b'\n' => { /* line continuation — drop */ }
                        b'u' => {
                            // \uXXXX or \u{XXXXX}
                            let (ch, consumed) = parse_unicode_escape(&self.bytes[self.pos..])
                                .ok_or("invalid \\u escape in string literal")?;
                            out.push(ch);
                            self.pos += consumed;
                        }
                        b'x' => {
                            if self.pos + 2 > self.bytes.len() {
                                return Err("invalid \\x escape in string literal".to_string());
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 2])
                                .map_err(|_| "invalid \\x escape".to_string())?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| "invalid \\x escape".to_string())?;
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                self.pos += 2;
                            } else {
                                return Err("invalid \\x code point".to_string());
                            }
                        }
                        other => out.push(other as char),
                    }
                }
                Some(b) => {
                    // A literal newline inside a double/single quote is a TS
                    // syntax error. We accept it anyway so weaker models that
                    // forget the heredoc/template-literal rule still get their
                    // content through rather than silently dropping the call.
                    out.push(b as char);
                }
            }
        }
    }

    fn parse_template_literal(&mut self) -> Result<serde_json::Value, String> {
        self.advance(); // opening backtick
        let mut out = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated template literal".to_string()),
                Some(b'`') => return Ok(serde_json::Value::String(out)),
                Some(b'\\') => {
                    let esc = self
                        .advance()
                        .ok_or("unterminated escape in template literal")?;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'\\' => out.push('\\'),
                        b'`' => out.push('`'),
                        b'$' => out.push('$'),
                        b'\n' => { /* line continuation — drop */ }
                        other => {
                            out.push('\\');
                            out.push(other as char);
                        }
                    }
                }
                Some(b'$') if self.peek() == Some(b'{') => {
                    // Template literal interpolation. Tool arguments never
                    // evaluate expressions; pass through the literal text.
                    out.push('$');
                    out.push('{');
                    self.advance();
                    let mut depth = 1usize;
                    while depth > 0 {
                        match self.advance() {
                            None => {
                                return Err(
                                    "unterminated ${{...}} interpolation in template literal"
                                        .to_string(),
                                );
                            }
                            Some(b'{') => {
                                depth += 1;
                                out.push('{');
                            }
                            Some(b'}') => {
                                depth -= 1;
                                out.push('}');
                            }
                            Some(b) => out.push(b as char),
                        }
                    }
                }
                Some(b) => {
                    out.push(b as char);
                }
            }
        }
    }

    /// Parse a heredoc string: `<<TAG\n...\nTAG`
    ///
    /// The tag is any sequence of uppercase letters/digits/underscore (e.g. EOF,
    /// END, CONTENT). Content between the opening tag line and a line containing
    /// only the tag is returned raw — no escaping of any kind is needed inside.
    /// This makes heredocs ideal for multiline code that contains backticks,
    /// quotes, or backslashes (Go raw strings, shell scripts, YAML, etc.).
    fn parse_heredoc(&mut self) -> Result<serde_json::Value, String> {
        // Consume "<<"
        self.advance();
        self.advance();
        // Skip optional quotes around the heredoc tag. Models commonly
        // emit <<'EOF' or <<"EOF" (bash-style quoting) instead of bare <<EOF.
        let has_quote = matches!(self.peek(), Some(b'\'') | Some(b'"'));
        let quote_char = self.peek();
        if has_quote {
            self.advance();
        }
        // Read tag: uppercase letters, digits, underscore
        let tag_start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.advance();
            } else {
                break;
            }
        }
        let tag = &self.text[tag_start..self.pos];
        if tag.is_empty() {
            return Err("heredoc requires a tag after << (e.g. <<EOF)".to_string());
        }
        // Skip closing quote if we had an opening one
        if has_quote && self.peek() == quote_char {
            self.advance();
        }
        // Consume the newline after the tag
        if self.peek() == Some(b'\r') {
            self.advance();
        }
        if self.peek() == Some(b'\n') {
            self.advance();
        } else {
            return Err(format!("expected newline after heredoc tag <<{tag}"));
        }
        // Read content until a line consisting of exactly the tag
        let content_start = self.pos;
        loop {
            // Find the start of the current line
            let line_start = self.pos;
            // Read to end of line
            while let Some(b) = self.peek() {
                if b == b'\n' {
                    break;
                }
                self.advance();
            }
            let line = &self.text[line_start..self.pos];
            let trimmed = line.trim();
            // Match the closing tag: the line must start with the tag, and
            // anything after it must be only commas/whitespace/closing parens
            // (to tolerate `OLD,` or `EOF  )` on the same line).
            if trimmed == tag
                || trimmed.starts_with(tag)
                    && trimmed[tag.len()..]
                        .chars()
                        .all(|c| c == ',' || c == ')' || c.is_whitespace())
            {
                let content = &self.text[content_start..line_start];
                let content = content.strip_suffix('\n').unwrap_or(content);
                let content = content.strip_suffix('\r').unwrap_or(content);
                // Rewind position to right after the tag so the outer parser
                // can see the trailing comma/paren.
                self.pos = line_start + line.find(tag).unwrap_or(0) + tag.len();
                return Ok(serde_json::Value::String(content.to_string()));
            }
            // Consume the newline
            if self.peek() == Some(b'\n') {
                self.advance();
            } else {
                // End of input without finding closing tag
                return Err(format!(
                    "unterminated heredoc: expected closing {tag} on its own line"
                ));
            }
        }
    }

    fn parse_boolean(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("true") {
            self.pos += 4;
            Ok(serde_json::Value::Bool(true))
        } else if self.text[self.pos..].starts_with("false") {
            self.pos += 5;
            Ok(serde_json::Value::Bool(false))
        } else {
            Err("expected `true` or `false`".to_string())
        }
    }

    fn parse_null(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("null") {
            self.pos += 4;
            Ok(serde_json::Value::Null)
        } else {
            Err("expected `null`".to_string())
        }
    }

    fn parse_undefined(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("undefined") {
            self.pos += 9;
            Ok(serde_json::Value::Null)
        } else {
            Err("expected `undefined`".to_string())
        }
    }

    fn parse_number(&mut self) -> Result<serde_json::Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' || b == b'+' || b == b'-' {
                self.advance();
            } else {
                break;
            }
        }
        let slice = &self.text[start..self.pos];
        if let Ok(n) = slice.parse::<i64>() {
            return Ok(serde_json::json!(n));
        }
        if let Ok(n) = slice.parse::<f64>() {
            return serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| "non-finite number literal".to_string());
        }
        Err(format!("invalid number literal `{slice}`"))
    }
}

/// Parse a `\uXXXX` or `\u{XXXXXX}` escape starting at bytes[0]. Returns the
/// decoded character AND the number of bytes consumed after the `\u`.
fn parse_unicode_escape(bytes: &[u8]) -> Option<(char, usize)> {
    if bytes.first() == Some(&b'{') {
        // \u{XXXXXX}
        let close = bytes.iter().position(|&b| b == b'}')?;
        let hex = std::str::from_utf8(&bytes[1..close]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        Some((char::from_u32(code)?, close + 1))
    } else if bytes.len() >= 4 {
        let hex = std::str::from_utf8(&bytes[..4]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        Some((char::from_u32(code)?, 4))
    } else {
        None
    }
}

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
#[path = "tools_tests.rs"]
mod tests;
