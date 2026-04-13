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

    let mut normalized = serde_json::Value::Object(obj);
    coerce_integer_like_tool_args(&mut normalized);
    normalized
}

fn coerce_integer_like_tool_args(value: &mut serde_json::Value) {
    const INTEGER_KEYS: &[&str] = &[
        "range_start",
        "range_end",
        "offset",
        "limit",
        "timeout",
        "line",
        "start_line",
        "end_line",
        "count",
    ];

    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if INTEGER_KEYS.contains(&key.as_str()) {
                    if let Some(raw) = child.as_str() {
                        if let Ok(parsed) = raw.trim().parse::<i64>() {
                            *child = serde_json::json!(parsed);
                            continue;
                        }
                    }
                }
                coerce_integer_like_tool_args(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                coerce_integer_like_tool_args(item);
            }
        }
        _ => {}
    }
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
    /// Protocol-level grammar violations (stray text outside tags, unknown
    /// tags, unclosed tags, malformed `<done>` contents). Distinct from
    /// `errors`, which carry per-call parse diagnostics. The agent loop
    /// replays these to the model as structured `protocol_violation`
    /// feedback so it can self-correct.
    pub violations: Vec<String>,
    /// Body of the `<done>` block when one was emitted, trimmed of
    /// surrounding whitespace. The agent compares this against the
    /// pipeline's configured `done_sentinel` (default `##DONE##`) to
    /// decide whether to honor completion. Replaces substring matching
    /// against a bare sentinel string.
    pub done_marker: Option<String>,
    /// Canonical reconstruction of the response in the tagged grammar.
    /// Used as the assistant's history entry so future turns see the
    /// well-formed shape instead of the raw provider bytes.
    pub canonical: String,
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

/// Scan a text body for bare `name({ ... })` tool calls and diagnostics.
///
/// This is the body-level parser used inside `<tool_call>` tags by the
/// tagged-protocol scanner. It is also called on whole responses as a
/// diagnostic fallback: when a model emits calls without wrapping them in
/// `<tool_call>` tags, we detect the calls here, report a grammar violation
/// at the outer layer, and refuse to execute until the model re-emits
/// properly wrapped.
pub(crate) fn parse_bare_calls_in_body(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    // Strip leaked thinking tags before any parsing
    let cleaned = strip_thinking_tags(text);
    let text = cleaned.as_ref();

    if let Some(unwrapped) = unwrap_exact_code_wrapper(text) {
        let result = parse_bare_calls_in_body(unwrapped, tools_val);
        if !result.calls.is_empty() || !result.errors.is_empty() {
            return result;
        }
    }
    let mut known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|s| s.name)
        .collect();
    // `ledger` is a runtime-owned pseudo-tool: always available during
    // agent_loop so the agent can maintain task-wide deliverables state
    // without each host having to register it separately. Handled inline
    // in `agent.rs` rather than dispatched through the tool executor.
    known.insert("ledger".to_string());
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
                // Gemma-family models RL-trained for tool use fall back to
                // their native `tool_code: fn(args)` inline prefix when text
                // mode asks them to emit bare calls. Strip it alongside the
                // other common labels so the call still parses. `python:` /
                // `javascript:` / etc. are language-tag labels some models
                // add when they think the runtime wants a code block.
                for prefix in [
                    "tool_code:",
                    "tool_call:",
                    "tool_output:",
                    "call:",
                    "tool:",
                    "use:",
                    "python:",
                    "javascript:",
                    "typescript:",
                    "shell:",
                    "bash:",
                ] {
                    if text[k..].starts_with(prefix) {
                        k += prefix.len();
                        // Also skip optional whitespace after the prefix.
                        while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                            k += 1;
                        }
                        break;
                    }
                }
                // Near-miss detection: a line shaped like
                // `some_label: known_tool(...)` where `some_label` isn't
                // in our strip allowlist. Silently treating the whole line
                // as prose gives the model no signal about its syntax
                // mistake, so it keeps emitting the same bad form. Only
                // fire when the SECOND identifier is a known tool name —
                // that guard prevents false positives on incidental prose
                // like `Tip: edit(...)` where `edit` happens to match or
                // on arbitrary `Note: something(args)` phrasing.
                //
                // We only emit the diagnostic; we do NOT treat the line
                // as a call. A false positive on an innocent prose line
                // like "Reminder: read(src/mod.rs) carefully" would still
                // be safe because the tool would not actually execute —
                // the model just sees an instruction on next turn not to
                // use that prefix.
                if let Some(label_len) = ident_length(&bytes[k..]) {
                    if bytes.get(k + label_len) == Some(&b':') {
                        let mut after_colon = k + label_len + 1;
                        while after_colon < bytes.len()
                            && (bytes[after_colon] == b' ' || bytes[after_colon] == b'\t')
                        {
                            after_colon += 1;
                        }
                        if let Some(inner_len) = ident_length(&bytes[after_colon..]) {
                            if bytes.get(after_colon + inner_len) == Some(&b'(') {
                                let inner_name = std::str::from_utf8(
                                    &bytes[after_colon..after_colon + inner_len],
                                )
                                .unwrap_or("");
                                if known.contains(inner_name) {
                                    let label =
                                        std::str::from_utf8(&bytes[k..k + label_len]).unwrap_or("");
                                    errors.push(format!(
                                        "Saw `{label}: {inner_name}(...)`. Do not prefix tool \
                                         calls with `{label}:` — emit bare \
                                         `{inner_name}({{ ... }})` on its own line. The \
                                         previous line was treated as prose and no tool \
                                         ran; re-emit it without the prefix."
                                    ));
                                    // Skip past the rest of this line without parsing it as
                                    // a call. The model re-emits cleanly on the next turn.
                                    while i < bytes.len() && bytes[i] != b'\n' {
                                        i += 1;
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Candidate tool call at line start: <ident>( directly.
                if let Some(name_len) = ident_length(&bytes[k..]) {
                    if bytes.get(k + name_len) == Some(&b'(') {
                        let name_str = std::str::from_utf8(&bytes[k..k + name_len]).unwrap_or("");
                        let object_arg_start = has_object_literal_arg_start(text, k + name_len + 1);
                        if known.contains(name_str) {
                            if !object_arg_start {
                                errors.push(format!(
                                    "Tool '{}' must be called with an object literal argument like {}({{ ... }}).",
                                    name_str, name_str
                                ));
                                i = k + name_len + 1;
                                at_line_start = false;
                                continue;
                            }
                            let name = name_str.to_string();
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
                        } else if object_arg_start {
                            let available: Vec<_> = known.iter().take(20).cloned().collect();
                            errors.push(format!(
                                "Unknown tool '{}'. Available tools: [{}]",
                                name_str,
                                available.join(", ")
                            ));
                            i = k + name_len + 1;
                            at_line_start = false;
                            continue;
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
        let (native_calls, native_errors) = parse_native_json_tool_calls(text, &known);
        if !native_calls.is_empty() || !native_errors.is_empty() {
            return TextToolParseResult {
                calls: native_calls,
                errors: native_errors,
                prose: String::new(),
                violations: Vec::new(),
                done_marker: None,
                canonical: String::new(),
            };
        }
    }

    TextToolParseResult {
        calls,
        errors,
        prose,
        violations: Vec::new(),
        done_marker: None,
        canonical: String::new(),
    }
}

/// Parse a model response under the strict tagged response protocol.
///
/// The grammar accepts a sequence of top-level blocks separated by
/// whitespace only:
///
/// ```text
///   <tool_call> <bare `name({...})` expression> </tool_call>
///   <assistant_prose> short narration </assistant_prose>
///   <done>##DONE##</done>
/// ```
///
/// Anything else at the top level — stray prose, code, unknown tags,
/// unclosed tags — is reported as a `violation`. Malformed call bodies
/// are reported as `errors` (per-call diagnostics). The function always
/// runs to completion so every violation can be surfaced to the model
/// on the next turn.
///
/// The `canonical` field is the response re-emitted in the tagged form.
/// It's what should be replayed as the assistant history entry, not the
/// raw provider bytes — that closes the self-poison loop where a turn
/// with leading raw code becomes "what the agent said" on the next turn.
pub(crate) fn parse_text_tool_calls_with_tools(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    let cleaned = strip_thinking_tags(text);
    let src = cleaned.as_ref();

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut violations: Vec<String> = Vec::new();
    let mut prose_parts: Vec<String> = Vec::new();
    let mut canonical_parts: Vec<String> = Vec::new();
    let mut done_marker: Option<String> = None;

    let mut cursor = 0usize;
    let bytes = src.as_bytes();

    while cursor < bytes.len() {
        // Skip whitespace between top-level tags.
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }

        // Collect any stray non-tag bytes up to the next `<`. A naive
        // scan-to-next-`<` truncates bare `name({ key: <<EOF\n...\nEOF })`
        // tool calls at the heredoc opener, leaving the salvage path with
        // a fragment that can't parse. Skip past `<<TAG ... TAG` heredoc
        // bodies in-line so a complete bare call survives the chunker.
        if bytes[cursor] != b'<' {
            let start = cursor;
            loop {
                while cursor < bytes.len() && bytes[cursor] != b'<' {
                    cursor += 1;
                }
                if cursor + 1 < bytes.len()
                    && bytes[cursor] == b'<'
                    && bytes[cursor + 1] == b'<'
                {
                    if let Some(after) = skip_heredoc_body(src, cursor) {
                        cursor = after;
                        continue;
                    }
                }
                break;
            }
            report_stray(
                &src[start..cursor],
                &mut violations,
                tools_val,
                &mut calls,
                &mut canonical_parts,
            );
            continue;
        }

        // Try to match a known top-level tag.
        if let Some((body, after)) = match_block(src, cursor, "tool_call") {
            match parse_single_tool_call(body, tools_val) {
                Ok(call) => {
                    let name = call
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = call
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));
                    canonical_parts.push(format!(
                        "<tool_call>\n{}\n</tool_call>",
                        render_canonical_call(&name, &args)
                    ));
                    calls.push(call);
                }
                Err(msg) => errors.push(msg),
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "assistant_prose") {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                prose_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<assistant_prose>\n{trimmed}\n</assistant_prose>"));
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "done") {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                violations.push(
                    "<done> block is empty. Emit the configured done sentinel \
                     (default `##DONE##`) inside the block."
                        .to_string(),
                );
            } else {
                done_marker = Some(trimmed.to_string());
                canonical_parts.push(format!("<done>{trimmed}</done>"));
            }
            cursor = after;
        } else if let Some((call, after_call)) =
            try_parse_angle_wrapped_call(src, cursor, tools_val)
        {
            // `<name({ ... })>` — angle-bracket-wrapped tool call (Qwen
            // family commonly falls back to this when their template puts
            // tools inside generic XML brackets). Execute the call and
            // record a soft violation so the model learns to use
            // `<tool_call>` wrapping next turn. Pre-v0.5.82 the parser
            // dropped these silently and the model spun emitting the same
            // wrong-wrapper response indefinitely.
            let name = call
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = call
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            canonical_parts.push(format!(
                "<tool_call>\n{}\n</tool_call>",
                render_canonical_call(&name, &args)
            ));
            calls.push(call);
            violations.push(format!(
                "Tool call `{name}` was emitted as `<{name}(...)>` instead of \
                 `<tool_call>{name}({{ ... }})</tool_call>`. Executed this turn \
                 so work moves forward; wrap each call in `<tool_call>` tags on \
                 subsequent turns."
            ));
            cursor = after_call;
        } else {
            // Unclosed or unknown tag — skip to end of line or `>` and record.
            let start = cursor;
            let mut end = cursor + 1;
            while end < bytes.len() && bytes[end] != b'>' && bytes[end] != b'\n' {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'>' {
                end += 1;
            }
            let fragment = &src[start..end];
            if fragment.starts_with('<') && !fragment.contains('>') {
                violations.push(format!(
                    "Unclosed tag starting at {:?}. Close it or remove it; only \
                     <tool_call>, <assistant_prose>, and <done> are accepted.",
                    preview_str(fragment, 40)
                ));
            } else {
                violations.push(format!(
                    "Unknown top-level tag {:?}. Use <tool_call>, <assistant_prose>, \
                     or <done> — no other tags are accepted at the top level.",
                    preview_str(fragment, 40)
                ));
            }
            cursor = end;
        }
    }

    // Detect empty responses: nothing parseable and no violations already recorded.
    let response_is_effectively_empty = calls.is_empty()
        && prose_parts.is_empty()
        && done_marker.is_none()
        && violations.is_empty()
        && errors.is_empty();
    if response_is_effectively_empty && !src.trim().is_empty() {
        violations.push(
            "Response contained no <tool_call>, <assistant_prose>, or <done> block. \
             Every response must be composed of these tags only."
                .to_string(),
        );
    }

    TextToolParseResult {
        calls,
        errors,
        prose: prose_parts.join("\n\n"),
        violations,
        done_marker,
        canonical: canonical_parts.join("\n\n"),
    }
}

/// Try to parse `<name({...})>` (or `<name({...})` with the closing `>`
/// optional / on a later line) at `cursor`. Returns the parsed call and
/// the byte position after the call (including any trailing `>`).
/// Only succeeds when `name` resolves to a registered tool.
fn try_parse_angle_wrapped_call(
    src: &str,
    cursor: usize,
    tools_val: Option<&VmValue>,
) -> Option<(serde_json::Value, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(cursor) != Some(&b'<') {
        return None;
    }
    // Identifier immediately after `<`.
    let name_start = cursor + 1;
    let name_len = ident_length(&bytes[name_start..])?;
    if name_len == 0 {
        return None;
    }
    if bytes.get(name_start + name_len) != Some(&b'(') {
        return None;
    }
    let name_str = std::str::from_utf8(&bytes[name_start..name_start + name_len]).ok()?;
    // Only known tools are eligible — keeps `<notes>...` out of the path.
    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|s| s.name)
        .chain(std::iter::once("ledger".to_string()))
        .collect();
    if !known.contains(name_str) {
        return None;
    }
    // Reuse the TS-call parser. It scans for the matching `)` honoring
    // heredocs, template literals, and nested object/array literals, so
    // multi-line calls with `<<EOF ... EOF` bodies are handled.
    let (arguments, consumed) =
        parse_ts_call_from(&src[name_start..], name_str.to_string()).ok()?;
    let mut end = name_start + consumed;
    // Step past optional whitespace and a single trailing `>`.
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    if bytes.get(end) == Some(&b'>') {
        end += 1;
    }
    let call = serde_json::json!({
        "id": format!("tc_angle_{name_str}"),
        "name": name_str,
        "arguments": arguments,
    });
    Some((call, end))
}

/// Report stray text that sits outside any recognized top-level tag.
/// When the stray content contains parseable tool calls, execute them
/// (route them through the canonical-call path) and add a soft violation
/// so the model still gets the signal to wrap calls properly. Pre-v0.5.82
/// the parser flagged-and-dropped these calls, which was correct in
/// principle but stranded weaker locally-hosted models in loops where
/// they kept re-emitting the same right-shape-wrong-wrapper response.
fn report_stray(
    fragment: &str,
    violations: &mut Vec<String>,
    tools_val: Option<&VmValue>,
    calls: &mut Vec<serde_json::Value>,
    canonical_parts: &mut Vec<String>,
) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return;
    }
    let sniff = parse_bare_calls_in_body(trimmed, tools_val);
    if !sniff.calls.is_empty() {
        let names: Vec<_> = sniff
            .calls
            .iter()
            .filter_map(|c| {
                c.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        for call in &sniff.calls {
            let name = call
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = call
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            canonical_parts.push(format!(
                "<tool_call>\n{}\n</tool_call>",
                render_canonical_call(&name, &args)
            ));
            calls.push(call.clone());
        }
        violations.push(format!(
            "Tool call(s) ({}) were emitted as bare text outside `<tool_call>` tags. \
             Executed this turn so work moves forward; please wrap each call in \
             `<tool_call>...</tool_call>` on subsequent turns.",
            names.join(", ")
        ));
    } else {
        violations.push(format!(
            "Stray text outside response tags: {:?}. Wrap all prose in \
             <assistant_prose>...</assistant_prose> and every tool call in \
             <tool_call>...</tool_call>.",
            preview_str(trimmed, 120)
        ));
    }
}

/// Parse a single `<tool_call>` body. Expects exactly one bare
/// `name({ ... })` expression (possibly with surrounding whitespace).
fn parse_single_tool_call(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<serde_json::Value, String> {
    let inner = parse_bare_calls_in_body(body, tools_val);
    if let Some(err) = inner.errors.into_iter().next() {
        return Err(err);
    }
    if inner.calls.is_empty() {
        return Err(format!(
            "<tool_call> body did not contain a bare `name({{ ... }})` expression. \
             Got: {:?}",
            preview_str(body.trim(), 120)
        ));
    }
    if inner.calls.len() > 1 {
        return Err(format!(
            "<tool_call> body contained {} calls; emit one call per <tool_call> block.",
            inner.calls.len()
        ));
    }
    Ok(inner.calls.into_iter().next().expect("len == 1"))
}

/// Match a balanced `<tag>...</tag>` block starting at `start` in `src`.
/// Returns `(body_slice, end_cursor)` on success. Does not support nested
/// same-name tags — not needed for this grammar and attempting to support
/// them bloats the error surface for no real benefit.
fn match_block<'a>(src: &'a str, start: usize, tag: &str) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    if !src[start..].starts_with(&open) {
        return None;
    }
    let body_start = start + open.len();
    let close = format!("</{tag}>");
    let close_idx = src[body_start..].find(&close)?;
    let body_end = body_start + close_idx;
    let after = body_end + close.len();
    Some((&src[body_start..body_end], after))
}

/// Render a parsed tool call back to the bare TS syntax used inside
/// `<tool_call>` tags. Used to build the canonical history entry.
fn render_canonical_call(name: &str, args: &serde_json::Value) -> String {
    // serde_json gives us valid JSON, which parses as a TS object literal
    // under our tool-call grammar (strings become double-quoted, no
    // trailing commas, keys quoted). That's enough for replay purposes —
    // the next turn's parser accepts JSON-style object literals just
    // like TS-style ones.
    let rendered_args = serde_json::to_string_pretty(args).unwrap_or_else(|_| "{}".to_string());
    format!("{name}({rendered_args})")
}

fn preview_str(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let kept: String = chars.into_iter().take(max).collect();
    format!("{kept}…")
}

fn has_object_literal_arg_start(text: &str, open_paren_idx: usize) -> bool {
    let bytes = text.as_bytes();
    let mut idx = open_paren_idx;
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    bytes.get(idx) == Some(&b'{')
}

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Looks for `[{"id":"call_...","function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text.
fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    // Find the first `[{` or `{"id":"call_` in the text
    let json_start = text
        .find("[{\"id\":")
        .or_else(|| text.find("[{\"id\":"))
        .or_else(|| text.find("{\"id\":\"call_"));

    let Some(start) = json_start else {
        return (results, errors);
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
        return (results, errors);
    };

    for item in items {
        let func = item.get("function").and_then(|f| f.as_object());
        let Some(func) = func else { continue };
        let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if !known_tools.contains(name) {
            let available: Vec<_> = known_tools.iter().take(20).cloned().collect();
            errors.push(format!(
                "Unknown tool '{}'. Available tools: [{}]",
                name,
                available.join(", ")
            ));
            continue;
        }
        // Arguments may be a JSON string (OpenAI format) or an object
        let arguments = match func.get("arguments") {
            Some(serde_json::Value::String(s)) => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!(
                        "Could not parse arguments for tool '{}': {}. Raw: {}",
                        name,
                        e,
                        &s[..s.len().min(200)]
                    ));
                    continue;
                }
            },
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

    (results, errors)
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

/// Skip past a `<<TAG\n...\nTAG` heredoc body starting at `start` in `src`.
/// Returns the byte position immediately after the closing tag (mirroring
/// `Parser::parse_heredoc`'s rewind), or `None` when the heredoc is malformed
/// or unterminated. Used by the top-level scanner so a stray-bytes chunker
/// doesn't truncate bare `name({ key: <<EOF\n...\nEOF })` tool calls at the
/// `<<` opener.
fn skip_heredoc_body(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') || bytes.get(start + 1) != Some(&b'<') {
        return None;
    }
    let mut pos = start + 2;
    let has_quote = matches!(bytes.get(pos), Some(b'\'') | Some(b'"'));
    let quote_char = bytes.get(pos).copied();
    if has_quote {
        pos += 1;
    }
    let tag_start = pos;
    while let Some(b) = bytes.get(pos) {
        if b.is_ascii_alphanumeric() || *b == b'_' {
            pos += 1;
        } else {
            break;
        }
    }
    if pos == tag_start {
        return None;
    }
    let tag = &src[tag_start..pos];
    if has_quote && bytes.get(pos).copied() == quote_char {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\r') {
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'\n') {
        return None;
    }
    pos += 1;
    while pos < bytes.len() {
        let line_start = pos;
        while let Some(b) = bytes.get(pos) {
            if *b == b'\n' {
                break;
            }
            pos += 1;
        }
        let line = &src[line_start..pos];
        let leading_ws_len = line.len() - line.trim_start().len();
        let after_ws = &line[leading_ws_len..];
        if let Some(rest) = after_ws.strip_prefix(tag) {
            let at_word_boundary = rest
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
            if at_word_boundary {
                return Some(line_start + leading_ws_len + tag.len());
            }
        }
        if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            return None;
        }
    }
    None
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
        if self.peek() == Some(b'<') && self.bytes.get(self.pos + 1) == Some(&b'<') {
            return self.parse_quoted_heredoc_literal(quote);
        }
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

    /// Recover malformed `"content": "<<EOF ... EOF` values by treating the
    /// quoted heredoc opener as intent to write a heredoc string rather than a
    /// normal string literal. Models commonly forget to drop the opening quote
    /// before `<<EOF`, and often omit the closing quote entirely.
    fn parse_quoted_heredoc_literal(&mut self, quote: u8) -> Result<serde_json::Value, String> {
        let value = self.parse_heredoc()?;
        if self.peek() == Some(quote) {
            self.advance();
        }
        Ok(value)
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
    /// END, CONTENT). Content between the opening tag line and a closing line
    /// that starts with the tag is returned raw — no escaping of any kind is
    /// needed inside. Closing punctuation may follow the tag on that same line,
    /// so tightly-collapsed tails like `EOF },` still parse correctly. This
    /// makes heredocs ideal for multiline code that contains backticks, quotes,
    /// or backslashes (Go raw strings, shell scripts, YAML, etc.).
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
            // Match the closing tag: after leading whitespace, the line must
            // start with the tag followed by a word boundary (end of line or
            // any non-identifier character). Anything after the tag is handed
            // back to the outer parser verbatim, which naturally absorbs
            // trailing commas, closing brackets, parens, braces, etc. without
            // the heredoc lexer maintaining a brittle allowlist of accepted
            // punctuation.
            let leading_ws_len = line.len() - line.trim_start().len();
            let after_ws = &line[leading_ws_len..];
            if let Some(rest) = after_ws.strip_prefix(tag) {
                let at_word_boundary = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
                if at_word_boundary {
                    let content = &self.text[content_start..line_start];
                    let content = content.strip_suffix('\n').unwrap_or(content);
                    let content = content.strip_suffix('\r').unwrap_or(content);
                    // Rewind position to right after the tag so the outer
                    // parser sees whatever followed it on the same line.
                    self.pos = line_start + leading_ws_len + tag.len();
                    return Ok(serde_json::Value::String(content.to_string()));
                }
            }
            // Consume the newline
            if self.peek() == Some(b'\n') {
                self.advance();
            } else {
                // End of input without finding closing tag
                return Err(format!(
                    "unterminated heredoc: expected closing {tag} at the start of a line"
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
