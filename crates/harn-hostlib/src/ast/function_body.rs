//! `ast.function_body` — extract a single function's body by name, plus
//! `ast.function_bodies` — extract many at once into a map keyed by name.
//!
//! Provides a single hostlib round-trip for hosts that need function-body
//! text plus return-object field hints.
//!
//! ## Wire format
//!
//! Both builtins accept either an in-memory `source` string or a file
//! `path` (with optional `language` hint). At least one must be present.
//! All line coordinates in the response are **1-based**. Symbols,
//! outline, and parse_file responses are 0-based; this builtin is the one
//! exception and the field name `start_line` vs. `start_row` flags it.
//!
//! ## Strategy
//!
//! Tree-sitter for all supported languages. For each function-like node
//! whose name matches we read the `body` /
//! `block` / `result` field, slice the source by row range, and return
//! the joined text plus a regex-derived list of return-object field
//! names for hosts that build API contract summaries.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{dict_arg, optional_string, require_string, str_value};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::symbols::helpers::{children, node_text};

const SINGLE_BUILTIN: &str = "hostlib_ast_function_body";
const BULK_BUILTIN: &str = "hostlib_ast_function_bodies";

/// One extracted function body.
#[derive(Debug, Clone)]
pub(super) struct ExtractedBody {
    pub name: String,
    pub body_text: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based end line.
    pub end_line: u32,
    pub return_object_fields: Vec<String>,
}

impl ExtractedBody {
    fn to_vm_value(&self) -> VmValue {
        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        dict.insert("name".into(), str_value(&self.name));
        dict.insert("body_text".into(), str_value(&self.body_text));
        dict.insert("start_line".into(), VmValue::Int(self.start_line as i64));
        dict.insert("end_line".into(), VmValue::Int(self.end_line as i64));
        let fields: Vec<VmValue> = self.return_object_fields.iter().map(str_value).collect();
        dict.insert(
            "return_object_fields".into(),
            VmValue::List(Rc::new(fields)),
        );
        VmValue::Dict(Rc::new(dict))
    }
}

pub(super) fn run_single(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SINGLE_BUILTIN, args)?;
    let dict = raw.as_ref();

    let function_name = require_string(SINGLE_BUILTIN, dict, "function_name")?;
    let container = optional_string(SINGLE_BUILTIN, dict, "container")?;
    let (source, language, path_for_response) = load_input(SINGLE_BUILTIN, dict)?;

    let tree = parse_source(&source, language)?;
    let body = extract_body(
        &tree,
        &source,
        language,
        &function_name,
        container.as_deref(),
    );
    let brace_based = !matches!(language, Language::Python);

    let mut response: BTreeMap<String, VmValue> = BTreeMap::new();
    response.insert(
        "path".into(),
        match path_for_response {
            Some(ref p) => str_value(p),
            None => VmValue::Nil,
        },
    );
    response.insert("language".into(), str_value(language.name()));
    response.insert("name".into(), str_value(&function_name));
    response.insert("brace_based".into(), VmValue::Bool(brace_based));
    if let Some(body) = body {
        response.insert("found".into(), VmValue::Bool(true));
        response.insert("body_text".into(), str_value(&body.body_text));
        response.insert("start_line".into(), VmValue::Int(body.start_line as i64));
        response.insert("end_line".into(), VmValue::Int(body.end_line as i64));
        let fields: Vec<VmValue> = body.return_object_fields.iter().map(str_value).collect();
        response.insert(
            "return_object_fields".into(),
            VmValue::List(Rc::new(fields)),
        );
    } else {
        response.insert("found".into(), VmValue::Bool(false));
        response.insert("body_text".into(), str_value(""));
        response.insert("start_line".into(), VmValue::Int(0));
        response.insert("end_line".into(), VmValue::Int(0));
        response.insert(
            "return_object_fields".into(),
            VmValue::List(Rc::new(Vec::new())),
        );
    }
    Ok(VmValue::Dict(Rc::new(response)))
}

pub(super) fn run_bulk(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BULK_BUILTIN, args)?;
    let dict = raw.as_ref();

    let names = require_string_list(BULK_BUILTIN, dict, "names")?;
    let container = optional_string(BULK_BUILTIN, dict, "container")?;
    let (source, language, path_for_response) = load_input(BULK_BUILTIN, dict)?;

    let tree = parse_source(&source, language)?;
    let unique: BTreeSet<String> = names.iter().cloned().collect();
    let mut bodies_dict: BTreeMap<String, VmValue> = BTreeMap::new();
    for name in &unique {
        if let Some(body) = extract_body(&tree, &source, language, name, container.as_deref()) {
            bodies_dict.insert(name.clone(), body.to_vm_value());
        }
    }

    let brace_based = !matches!(language, Language::Python);

    let mut missing: Vec<VmValue> = Vec::new();
    for name in &unique {
        if !bodies_dict.contains_key(name) {
            missing.push(str_value(name));
        }
    }

    let mut response: BTreeMap<String, VmValue> = BTreeMap::new();
    response.insert(
        "path".into(),
        match path_for_response {
            Some(ref p) => str_value(p),
            None => VmValue::Nil,
        },
    );
    response.insert("language".into(), str_value(language.name()));
    response.insert("brace_based".into(), VmValue::Bool(brace_based));
    response.insert("bodies".into(), VmValue::Dict(Rc::new(bodies_dict)));
    response.insert("missing".into(), VmValue::List(Rc::new(missing)));
    Ok(VmValue::Dict(Rc::new(response)))
}

fn require_string_list(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Vec<String>, HostlibError> {
    let Some(raw) = dict.get(key) else {
        return Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        });
    };
    let VmValue::List(list) = raw else {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of strings, got {}", raw.type_name()),
        });
    };
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        let VmValue::String(s) = item else {
            return Err(HostlibError::InvalidParameter {
                builtin,
                param: key,
                message: format!("entries must be strings, got {}", item.type_name()),
            });
        };
        out.push(s.to_string());
    }
    if out.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must contain at least one name".into(),
        });
    }
    Ok(out)
}

/// Resolve `(source, language, path)` from a request dict that may
/// supply either an in-memory `source` plus `language` or a `path`
/// (with optional `language` override).
fn load_input(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
) -> Result<(String, Language, Option<String>), HostlibError> {
    let source_in = optional_string(builtin, dict, "source")?;
    let path_in = optional_string(builtin, dict, "path")?;
    let language_in = optional_string(builtin, dict, "language")?;

    if source_in.is_none() && path_in.is_none() {
        return Err(HostlibError::MissingParameter {
            builtin,
            param: "source",
        });
    }

    let language = if let Some(ref name) = language_in {
        Language::from_name(name).ok_or_else(|| HostlibError::InvalidParameter {
            builtin,
            param: "language",
            message: format!("unrecognized language `{name}`"),
        })?
    } else if let Some(ref path_str) = path_in {
        let path = std::path::Path::new(path_str);
        Language::detect(path, None).ok_or_else(|| HostlibError::InvalidParameter {
            builtin,
            param: "language",
            message: format!(
                "could not infer a tree-sitter grammar for `{path_str}` \
                 (extension or `language` field unrecognized)"
            ),
        })?
    } else {
        return Err(HostlibError::MissingParameter {
            builtin,
            param: "language",
        });
    };

    let source = match (source_in, &path_in) {
        (Some(s), _) => s,
        (None, Some(p)) => read_source(p, 0)?,
        (None, None) => unreachable!("guarded above"),
    };

    Ok((source, language, path_in))
}

/// Walk the parse tree and return the first function body that matches
/// `function_name` (and `container`, when supplied).
pub(super) fn extract_body(
    tree: &Tree,
    source: &str,
    language: Language,
    function_name: &str,
    container_filter: Option<&str>,
) -> Option<ExtractedBody> {
    let lines: Vec<&str> = split_lines(source);
    let root = tree.root_node();
    let mut stack: Vec<String> = Vec::new();
    walk_for_body(
        root,
        source,
        language,
        function_name,
        container_filter,
        &lines,
        &mut stack,
    )
}

fn split_lines(source: &str) -> Vec<&str> {
    // `split('\n')` keeps a trailing newline as a final empty element, but
    // slicing by row range is safe.
    source.split('\n').collect()
}

fn walk_for_body(
    node: Node<'_>,
    source: &str,
    language: Language,
    function_name: &str,
    container_filter: Option<&str>,
    lines: &[&str],
    stack: &mut Vec<String>,
) -> Option<ExtractedBody> {
    if is_function_like(node, language)
        && matches_function_name(node, source, language, function_name)
    {
        let container_ok = match container_filter {
            None => true,
            Some(want) => stack.iter().any(|n| n == want),
        };
        if container_ok {
            if let Some(body) = body_from_function_node(node, function_name, lines, language) {
                return Some(body);
            }
        }
    }

    let pushed_container = container_name_if_any(node, source, language);
    if let Some(ref name) = pushed_container {
        stack.push(name.clone());
    }

    for child in children(node) {
        if !child.is_named() {
            continue;
        }
        if let Some(found) = walk_for_body(
            child,
            source,
            language,
            function_name,
            container_filter,
            lines,
            stack,
        ) {
            if pushed_container.is_some() {
                stack.pop();
            }
            return Some(found);
        }
    }

    if pushed_container.is_some() {
        stack.pop();
    }
    None
}

/// Tree-sitter node kinds that introduce function-like declarations,
/// gathered from the per-language extractors in
/// [`super::symbols`]. Kept as an inclusive superset so we never miss
/// a candidate; the name-match filter culls false positives.
fn is_function_like(node: Node<'_>, _language: Language) -> bool {
    matches!(
        node.kind(),
        // TS / JS / Swift / Lua / Bash / Zig / C# / Kotlin / Scala
        "function_declaration"
        // TS / JS
        | "method_definition"
        // Java / C# / PHP
        | "method_declaration"
        // Rust
        | "function_item"
        // C / C++ / Python / PHP / Scala
        | "function_definition"
        // Java
        | "constructor_declaration"
        // Ruby
        | "method"
        | "singleton_method"
        // Lua
        | "local_function"
        // Haskell
        | "function"
        // Elixir uses `call` for `def name(...)`; matched by name below.
        | "call"
        // R `name <- function(...)` pattern.
        | "binary_operator"
        // JS arrow funcs declared via `const`/`let`/`var`.
        | "lexical_declaration"
        | "variable_declaration"
    )
}

fn matches_function_name(node: Node<'_>, source: &str, language: Language, target: &str) -> bool {
    if let Some(name_node) = node.child_by_field_name("name") {
        if node_text(name_node, source) == target {
            return true;
        }
    }

    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            for child in children(node) {
                if !child.is_named() || child.kind() != "variable_declarator" {
                    continue;
                }
                if let Some(name_node) = child.child_by_field_name("name") {
                    if node_text(name_node, source) == target {
                        return true;
                    }
                }
            }
            false
        }
        "call" if matches!(language, Language::Elixir) => {
            // `def name(args) do ... end` — second child holds the head.
            let Some(head_keyword) = node.child(0u32) else {
                return false;
            };
            let kw = node_text(head_keyword, source);
            if kw != "def" && kw != "defp" {
                return false;
            }
            let Some(arg) = node.child(1u32) else {
                return false;
            };
            let head = node_text(arg, source);
            let head_first_line = head.lines().next().unwrap_or("");
            let head_name = head_first_line
                .split('(')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            head_name == target
        }
        "binary_operator" if matches!(language, Language::R) => {
            let Some(lhs) = node.child_by_field_name("lhs") else {
                return false;
            };
            let Some(rhs) = node.child_by_field_name("rhs") else {
                return false;
            };
            if rhs.kind() != "function_definition" {
                return false;
            }
            node_text(lhs, source) == target
        }
        _ => false,
    }
}

/// Pull the body from a matched function-like node. Tries the
/// canonical body fields (`body`, `block`, `result`) first; falls back
/// to "whole node minus first line" so we always return *something* if
/// the grammar lacks a body field.
fn body_from_function_node(
    node: Node<'_>,
    function_name: &str,
    lines: &[&str],
    language: Language,
) -> Option<ExtractedBody> {
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
        for child in children(node) {
            if !child.is_named() || child.kind() != "variable_declarator" {
                continue;
            }
            let Some(value) = child.child_by_field_name("value") else {
                continue;
            };
            if matches!(
                value.kind(),
                "arrow_function" | "function" | "function_expression"
            ) {
                if let Some(body) = body_field(value) {
                    return shape_body(body, function_name, lines);
                }
                return whole_minus_first(value, function_name, lines);
            }
        }
        return None;
    }

    if matches!(node.kind(), "call") && matches!(language, Language::Elixir) {
        // For Elixir, body lives inside the second arg's do-block; return
        // the entire call node minus the first line as a stable
        // approximation.
        return whole_minus_first(node, function_name, lines);
    }

    if matches!(node.kind(), "binary_operator") && matches!(language, Language::R) {
        let rhs = node.child_by_field_name("rhs")?;
        if let Some(body) = body_field(rhs) {
            return shape_body(body, function_name, lines);
        }
        return whole_minus_first(rhs, function_name, lines);
    }

    if let Some(body) = body_field(node) {
        return shape_body(body, function_name, lines);
    }
    whole_minus_first(node, function_name, lines)
}

fn body_field(node: Node<'_>) -> Option<Node<'_>> {
    for field in ["body", "block", "result"] {
        if let Some(n) = node.child_by_field_name(field) {
            return Some(n);
        }
    }
    None
}

fn shape_body(body: Node<'_>, function_name: &str, lines: &[&str]) -> Option<ExtractedBody> {
    let start = body.start_position().row;
    let end = body.end_position().row;
    let body_text = slice_lines(lines, start, end);
    let fields = extract_return_fields(&body_text);
    Some(ExtractedBody {
        name: function_name.to_string(),
        body_text,
        start_line: (start + 1) as u32,
        end_line: (end + 1) as u32,
        return_object_fields: fields,
    })
}

fn whole_minus_first(node: Node<'_>, function_name: &str, lines: &[&str]) -> Option<ExtractedBody> {
    let start = node.start_position().row;
    let end = node.end_position().row;
    if end <= start {
        return None;
    }
    let body_text = slice_lines(lines, start + 1, end);
    let fields = extract_return_fields(&body_text);
    Some(ExtractedBody {
        name: function_name.to_string(),
        body_text,
        start_line: (start + 2) as u32,
        end_line: (end + 1) as u32,
        return_object_fields: fields,
    })
}

fn slice_lines(lines: &[&str], start_row: usize, end_row: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let last = lines.len().saturating_sub(1);
    let s = start_row.min(last);
    let e = end_row.min(last);
    if s > e {
        return String::new();
    }
    lines[s..=e].join("\n")
}

fn container_name_if_any(node: Node<'_>, source: &str, language: Language) -> Option<String> {
    let kind = node.kind();
    let known = matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "struct_declaration"
            | "type_declaration"
            | "trait_definition"
            | "object_definition"
            | "object_declaration"
            | "namespace_declaration"
            | "namespace_definition"
            | "file_scoped_namespace_declaration"
            | "type_alias_declaration"
            | "class_definition"
            | "class_specifier"
            | "struct_specifier"
            | "enum_specifier"
            | "type_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "protocol_declaration"
            | "module"
            | "class"
    );
    if !known {
        return None;
    }
    if matches!(node.kind(), "impl_item") && matches!(language, Language::Rust) {
        return node
            .child_by_field_name("type")
            .map(|n| node_text(n, source));
    }
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Walks the body line-by-line, tracking whether we're inside a `return {
/// ... }` (or arrow-function `=> { ... }`) object literal, and pulls field
/// names off `name:` / `'name':` / `"name":` lines.
pub(super) fn extract_return_fields(body_text: &str) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut in_return_object = false;
    let mut depth: i32 = 0;
    let keywords: &[&str] = &[
        "return", "if", "else", "const", "let", "var", "function", "async", "await", "for",
        "while", "switch", "case", "break", "def", "class", "import", "from", "try", "catch",
        "finally",
    ];

    for line in body_text.split('\n') {
        let trimmed = line.trim();
        if !in_return_object
            && (trimmed.starts_with("return {")
                || trimmed.starts_with("return{")
                || trimmed.contains("=> ({")
                || trimmed.contains("=> {"))
        {
            in_return_object = true;
            depth = 0;
        }
        if in_return_object {
            depth += trimmed.chars().filter(|c| *c == '{').count() as i32;
            depth -= trimmed.chars().filter(|c| *c == '}').count() as i32;
            if let Some(field) = leading_field_name(trimmed) {
                if !keywords.contains(&field.as_str()) {
                    fields.push(field);
                }
            }
            if depth <= 0 {
                in_return_object = false;
            }
        }
    }
    fields
}

/// Match `^\s*['"]?(\w+)['"]?\s*[,:]` — a field-name regex. Hand-rolled
/// to avoid a regex dependency just for this one check.
fn leading_field_name(trimmed: &str) -> Option<String> {
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let opening = if i < bytes.len() && (bytes[i] == b'\'' || bytes[i] == b'"') {
        let q = bytes[i];
        i += 1;
        Some(q)
    } else {
        None
    };
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = std::str::from_utf8(&bytes[name_start..i]).ok()?.to_string();
    if let Some(q) = opening {
        if i >= bytes.len() || bytes[i] != q {
            return None;
        }
        i += 1;
    }
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b',' || bytes[i] == b':' {
        Some(name)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_fields_picks_up_object_literal() {
        // Each captured field must be followed by `,` or `:`. A bare
        // shorthand identifier on its own line (no trailing comma)
        // intentionally doesn't count.
        let body = "return {\n  a: 1,\n  b: 2,\n  c,\n};";
        let fields = extract_return_fields(body);
        assert_eq!(fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn return_fields_skips_keywords_inside_object() {
        let body = "return {\n  if: cond,\n  foo: 1\n};";
        let fields = extract_return_fields(body);
        assert_eq!(fields, vec!["foo"]);
    }

    #[test]
    fn return_fields_handles_arrow_function_returns() {
        let body = "items.map(x => ({\n  id: x.id,\n  label: x.label\n}));";
        let fields = extract_return_fields(body);
        assert_eq!(fields, vec!["id", "label"]);
    }

    #[test]
    fn return_fields_handles_quoted_keys() {
        let body = "return {\n  \"a\": 1,\n  'b': 2\n};";
        let fields = extract_return_fields(body);
        assert_eq!(fields, vec!["a", "b"]);
    }
}
