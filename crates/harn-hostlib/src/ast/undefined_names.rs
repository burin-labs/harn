//! `ast.undefined_names` — language-aware undefined-identifier detection.
//!
//! Mirrors `TreeSitterUndefinedNames.diagnose` in burin-code's Swift
//! `ASTEngine` (`Sources/ASTEngine/TreeSitterUndefinedNames.swift`). The
//! contract:
//!
//! - Walk the tree-sitter parse, collect every identifier reference and
//!   every name *defined* in this file (imports, parameters, locals,
//!   class names, etc.).
//! - Subtract definitions and a curated language-builtins stop-list from
//!   the references to produce the "undefined name" set.
//! - Deduplicate by name on first occurrence so callers see one
//!   diagnostic per missing import / typo, not one per usage.
//!
//! Profiles ship for Python, JavaScript, TypeScript, Go, and Ruby —
//! exactly the language set the Swift fallback supports today. Other
//! languages return `supported = false` so callers can fall back to an
//! external linter.
//!
//! Single-file scope is a deliberate restriction: cross-file resolution,
//! re-exports, dynamic attribute access, and `exec`/`eval`-style name
//! discovery are out of scope, matching the Swift surface.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, optional_string, str_value};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::types::UndefinedName;

const BUILTIN: &str = "hostlib_ast_undefined_names";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let content = optional_string(BUILTIN, dict, "content")?;
    let path_str = optional_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_bytes = optional_int(BUILTIN, dict, "max_bytes", 0)?;
    if max_bytes < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_bytes",
            message: "must be >= 0".into(),
        });
    }

    if content.is_none() && path_str.is_none() {
        return Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "content_or_path",
        });
    }

    let language = resolve_language(path_str.as_deref(), language_hint.as_deref())?;

    if !is_supported(language) {
        return Ok(unsupported_response(path_str.as_deref(), language));
    }

    let source = match (&content, &path_str) {
        (Some(text), _) => clip(text, max_bytes as usize),
        (None, Some(path)) => read_source(path, max_bytes as usize)?,
        _ => unreachable!("guarded above"),
    };

    let tree = match parse_source(&source, language) {
        Ok(tree) => tree,
        Err(_) => {
            return Ok(empty_response(path_str.as_deref(), language));
        }
    };

    let diagnostics = diagnose(&tree, &source, language);
    let dlist: Vec<VmValue> = diagnostics.iter().map(UndefinedName::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(path_str.as_deref().unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("diagnostics", VmValue::List(Rc::new(dlist))),
    ]))
}

fn unsupported_response(path: Option<&str>, language: Language) -> VmValue {
    build_dict([
        ("path", str_value(path.unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(false)),
        ("diagnostics", VmValue::List(Rc::new(Vec::new()))),
    ])
}

fn empty_response(path: Option<&str>, language: Language) -> VmValue {
    build_dict([
        ("path", str_value(path.unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("diagnostics", VmValue::List(Rc::new(Vec::new()))),
    ])
}

fn resolve_language(
    path: Option<&str>,
    language_hint: Option<&str>,
) -> Result<Language, HostlibError> {
    if let Some(name) = language_hint.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::from_name(name) {
            return Ok(lang);
        }
        if let Some(lang) = Language::from_extension(name) {
            return Ok(lang);
        }
    }
    if let Some(p) = path.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::detect(&PathBuf::from(p), language_hint) {
            return Ok(lang);
        }
    }
    Err(HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param: "language",
        message: format!(
            "could not infer a tree-sitter grammar (path = `{}`, language = `{}`)",
            path.unwrap_or(""),
            language_hint.unwrap_or("")
        ),
    })
}

fn clip(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Whether we ship a language profile for `language`.
pub(super) fn is_supported(language: Language) -> bool {
    matches!(
        language,
        Language::Python
            | Language::JavaScript
            | Language::Jsx
            | Language::TypeScript
            | Language::Tsx
            | Language::Go
            | Language::Ruby
    )
}

/// Run the appropriate per-language profile against `tree` / `source`
/// and return the deduplicated undefined-name list.
fn diagnose(tree: &Tree, source: &str, language: Language) -> Vec<UndefinedName> {
    let mut defined: HashSet<String> = HashSet::new();
    let mut references: Vec<UndefinedName> = Vec::new();
    let root = tree.root_node();

    match language {
        Language::Python => python::collect(root, source, &mut defined, &mut references),
        Language::JavaScript | Language::Jsx => {
            javascript::collect(root, source, &mut defined, &mut references, false)
        }
        Language::TypeScript | Language::Tsx => {
            javascript::collect(root, source, &mut defined, &mut references, true)
        }
        Language::Go => go::collect(root, source, &mut defined, &mut references),
        Language::Ruby => ruby::collect(root, source, &mut defined, &mut references),
        _ => return Vec::new(),
    }

    let builtins = builtins_for(language);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<UndefinedName> = Vec::new();
    for refr in references {
        if defined.contains(&refr.name) || builtins.contains(refr.name.as_str()) {
            continue;
        }
        if !seen.insert(refr.name.clone()) {
            continue;
        }
        out.push(refr);
    }
    out
}

fn builtins_for(language: Language) -> &'static HashSet<&'static str> {
    match language {
        Language::Python => &python::BUILTINS,
        Language::JavaScript | Language::Jsx => &javascript::JS_BUILTINS,
        Language::TypeScript | Language::Tsx => &javascript::TS_BUILTINS,
        Language::Go => &go::BUILTINS,
        Language::Ruby => &ruby::BUILTINS,
        _ => &EMPTY_BUILTINS,
    }
}

use once_cell::sync::Lazy;
static EMPTY_BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(HashSet::new);

// ---------------------------------------------------------------------------
// Shared helpers

pub(super) fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

pub(super) fn position(node: Node<'_>) -> (u32, u32) {
    let p = node.start_position();
    (p.row as u32, p.column as u32)
}

/// Recursive depth-first walk over every child (named and anonymous).
/// Threads the tree's lifetime through `F` so closures can capture nodes
/// into outside containers (the `for<'r>` HRTB form would require any
/// lifetime, breaking that).
pub(super) fn walk<'tree, F>(node: Node<'tree>, visit: &mut F)
where
    F: FnMut(Node<'tree>),
{
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

/// First identifier found in a depth-first walk of `node`.
pub(super) fn first_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_identifier(child) {
            return Some(found);
        }
    }
    None
}

/// Field name this node occupies in its parent, if any.
pub(super) fn field_name(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    for (idx, child) in parent.children(&mut cursor).enumerate() {
        if child.id() == node.id() {
            return parent.field_name_for_child(idx as u32);
        }
    }
    None
}

fn add_reference(refs: &mut Vec<UndefinedName>, node: Node<'_>, source: &str, kind: &'static str) {
    let (row, col) = position(node);
    refs.push(UndefinedName {
        name: node_text(node, source).to_string(),
        kind,
        row,
        column: col,
    });
}

// ---------------------------------------------------------------------------
// Profiles

mod python {
    use super::*;
    use once_cell::sync::Lazy;

    pub(super) static BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
        [
            "__name__",
            "__main__",
            "__file__",
            "__doc__",
            "__builtins__",
            "__dict__",
            "__init__",
            "__class__",
            "__all__",
            "__author__",
            "__version__",
            "True",
            "False",
            "None",
            "NotImplemented",
            "Ellipsis",
            "self",
            "cls",
            "super",
            "abs",
            "all",
            "any",
            "ascii",
            "bin",
            "bool",
            "breakpoint",
            "bytearray",
            "bytes",
            "callable",
            "chr",
            "classmethod",
            "compile",
            "complex",
            "delattr",
            "dict",
            "dir",
            "divmod",
            "enumerate",
            "eval",
            "exec",
            "exit",
            "filter",
            "float",
            "format",
            "frozenset",
            "getattr",
            "globals",
            "hasattr",
            "hash",
            "help",
            "hex",
            "id",
            "input",
            "int",
            "isinstance",
            "issubclass",
            "iter",
            "len",
            "list",
            "locals",
            "map",
            "max",
            "memoryview",
            "min",
            "next",
            "object",
            "oct",
            "open",
            "ord",
            "pow",
            "print",
            "property",
            "range",
            "repr",
            "reversed",
            "round",
            "set",
            "setattr",
            "slice",
            "sorted",
            "staticmethod",
            "str",
            "sum",
            "tuple",
            "type",
            "vars",
            "zip",
            "Exception",
            "BaseException",
            "ValueError",
            "TypeError",
            "KeyError",
            "IndexError",
            "AttributeError",
            "RuntimeError",
            "StopIteration",
            "NotImplementedError",
            "FileNotFoundError",
            "IOError",
            "OSError",
            "ArithmeticError",
            "ZeroDivisionError",
            "OverflowError",
            "NameError",
            "UnicodeDecodeError",
            "UnicodeEncodeError",
            "ImportError",
            "ModuleNotFoundError",
            "GeneratorExit",
            "KeyboardInterrupt",
            "SystemExit",
            "match",
            "case",
        ]
        .into_iter()
        .collect()
    });

    pub(super) fn collect(
        root: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        visit(root, source, defined, refs);
    }

    fn visit(
        node: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        match node.kind() {
            "function_definition" | "lambda" => {
                if node.kind() == "function_definition" {
                    if let Some(name) = node.child_by_field_name("name") {
                        defined.insert(node_text(name, source).to_string());
                    }
                }
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_python_parameters(params, source, defined);
                    // Defaults are *references*: walk the parameter list
                    // and visit any default value node we find.
                    let mut visit_defaults = |child: Node<'_>| {
                        let t = child.kind();
                        let has_default =
                            t == "default_parameter" || t == "typed_default_parameter";
                        if has_default {
                            if let Some(value) = child.child_by_field_name("value") {
                                visit(value, source, defined, refs);
                            }
                        }
                    };
                    walk(params, &mut visit_defaults);
                }
                if let Some(rt) = node.child_by_field_name("return_type") {
                    visit(rt, source, defined, refs);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "class_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(bases) = node.child_by_field_name("superclasses") {
                    visit(bases, source, defined, refs);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "import_statement" | "import_from_statement" => {
                collect_python_imports(node, source, defined);
            }
            "for_statement" | "for_in_clause" => {
                if let Some(target) = node.child_by_field_name("left") {
                    collect_python_targets(target, source, defined);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "assignment" => {
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    collect_python_targets(left, source, defined);
                }
            }
            "named_expression" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
            }
            "global_statement" | "nonlocal_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    }
                }
            }
            "with_statement" => {
                let mut cursor = node.walk();
                for item in node.named_children(&mut cursor) {
                    let mut vis = |child: Node<'_>| {
                        if child.kind() == "as_pattern_target" {
                            if let Some(ident) = first_identifier(child) {
                                defined.insert(node_text(ident, source).to_string());
                            }
                        }
                    };
                    walk(item, &mut vis);
                    if let Some(value) = item.child_by_field_name("value") {
                        visit(value, source, defined, refs);
                    }
                }
            }
            "except_clause" => {
                let mut saw_as = false;
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "as" {
                        saw_as = true;
                        continue;
                    }
                    if saw_as && child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                        saw_as = false;
                    } else if child.is_named() {
                        visit(child, source, defined, refs);
                    }
                }
            }
            "attribute" => {
                if let Some(object) = node.child_by_field_name("object") {
                    visit(object, source, defined, refs);
                }
            }
            "keyword_argument" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs);
                }
            }
            "identifier" => {
                add_reference(refs, node, source, "identifier");
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    visit(child, source, defined, refs);
                }
            }
        }
    }

    fn collect_python_parameters(params: Node<'_>, source: &str, defined: &mut HashSet<String>) {
        let mut cursor = params.walk();
        for child in params.named_children(&mut cursor) {
            match child.kind() {
                "identifier" => {
                    defined.insert(node_text(child, source).to_string());
                }
                "typed_parameter"
                | "default_parameter"
                | "typed_default_parameter"
                | "list_splat_pattern"
                | "dictionary_splat_pattern" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        defined.insert(node_text(name, source).to_string());
                    } else if let Some(ident) = first_identifier(child) {
                        defined.insert(node_text(ident, source).to_string());
                    }
                }
                _ => {
                    if let Some(ident) = first_identifier(child) {
                        defined.insert(node_text(ident, source).to_string());
                    }
                }
            }
        }
    }

    fn collect_python_imports(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
        // Bind aliases and bare module names from anywhere in the import.
        let mut vis = |child: Node<'_>| match child.kind() {
            "aliased_import" => {
                if let Some(alias) = child.child_by_field_name("alias") {
                    defined.insert(node_text(alias, source).to_string());
                }
            }
            "dotted_name" => {
                if let Some(first) = child.named_child(0) {
                    defined.insert(node_text(first, source).to_string());
                }
            }
            _ => {}
        };
        walk(node, &mut vis);

        // `from x import y, z` — direct identifiers after `import`.
        if node.kind() == "import_from_statement" {
            let mut saw_import = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "import" {
                    saw_import = true;
                    continue;
                }
                if saw_import {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    } else if child.kind() == "dotted_name" {
                        let count = child.named_child_count();
                        if count > 0 {
                            if let Some(last) = child.named_child((count - 1) as u32) {
                                defined.insert(node_text(last, source).to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_python_targets(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
        match node.kind() {
            "identifier" => {
                defined.insert(node_text(node, source).to_string());
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    collect_python_targets(child, source, defined);
                }
            }
            "list_splat_pattern" => {
                if let Some(ident) = first_identifier(node) {
                    defined.insert(node_text(ident, source).to_string());
                }
            }
            // Assignment to `a.b` / `a[b]` doesn't bind a new local.
            "attribute" | "subscript" => {}
            _ => {}
        }
    }
}

mod javascript {
    use super::*;
    use once_cell::sync::Lazy;

    pub(super) static JS_BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(base_builtins);
    pub(super) static TS_BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
        let mut set = base_builtins();
        // TS-only builtin type names so references don't fire on every
        // `string` / `number`.
        for extra in [
            "string",
            "number",
            "boolean",
            "any",
            "unknown",
            "never",
            "void",
            "object",
            "bigint",
            "symbol",
            "Record",
            "Partial",
            "Required",
            "Readonly",
            "Pick",
            "Omit",
            "Exclude",
            "Extract",
            "NonNullable",
            "Parameters",
            "ReturnType",
            "InstanceType",
            "ThisParameterType",
            "OmitThisParameter",
            "ThisType",
            "Awaited",
            "Array",
        ] {
            set.insert(extra);
        }
        set
    });

    fn base_builtins() -> HashSet<&'static str> {
        [
            "undefined",
            "null",
            "true",
            "false",
            "this",
            "arguments",
            "globalThis",
            "console",
            "process",
            "window",
            "document",
            "navigator",
            "setTimeout",
            "clearTimeout",
            "setInterval",
            "clearInterval",
            "setImmediate",
            "queueMicrotask",
            "requestAnimationFrame",
            "Promise",
            "Error",
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "URIError",
            "EvalError",
            "Array",
            "Object",
            "String",
            "Number",
            "Boolean",
            "Symbol",
            "BigInt",
            "RegExp",
            "Date",
            "Math",
            "JSON",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Int8Array",
            "Int16Array",
            "Int32Array",
            "Uint8Array",
            "Uint16Array",
            "Uint32Array",
            "Uint8ClampedArray",
            "Float32Array",
            "Float64Array",
            "ArrayBuffer",
            "DataView",
            "SharedArrayBuffer",
            "Atomics",
            "parseInt",
            "parseFloat",
            "isNaN",
            "isFinite",
            "encodeURI",
            "decodeURI",
            "encodeURIComponent",
            "decodeURIComponent",
            "escape",
            "unescape",
            "NaN",
            "Infinity",
            "require",
            "module",
            "exports",
            "__dirname",
            "__filename",
            "Buffer",
            "React",
            "JSX",
            "async",
            "await",
        ]
        .into_iter()
        .collect()
    }

    pub(super) fn collect(
        root: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
        typescript: bool,
    ) {
        visit(root, source, defined, refs, typescript);
    }

    fn bind_pattern(
        node: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
        typescript: bool,
    ) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                defined.insert(node_text(node, source).to_string());
            }
            "array_pattern" | "object_pattern" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    bind_pattern(child, source, defined, refs, typescript);
                }
            }
            "assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    bind_pattern(left, source, defined, refs, typescript);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs, typescript);
                }
            }
            "rest_pattern" => {
                if let Some(ident) = first_identifier(node) {
                    defined.insert(node_text(ident, source).to_string());
                }
            }
            "pair_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    bind_pattern(value, source, defined, refs, typescript);
                }
            }
            _ => {
                if let Some(ident) = first_identifier(node) {
                    defined.insert(node_text(ident, source).to_string());
                }
            }
        }
    }

    fn visit(
        node: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
        typescript: bool,
    ) {
        match node.kind() {
            "import_statement" => {
                let mut vis = |child: Node<'_>| match child.kind() {
                    "identifier" if field_name(child) != Some("source") => {
                        defined.insert(node_text(child, source).to_string());
                    }
                    "namespace_import" => {
                        if let Some(ident) = first_identifier(child) {
                            defined.insert(node_text(ident, source).to_string());
                        }
                    }
                    "import_specifier" => {
                        if let Some(alias) = child.child_by_field_name("alias") {
                            defined.insert(node_text(alias, source).to_string());
                        } else if let Some(name) = child.child_by_field_name("name") {
                            defined.insert(node_text(name, source).to_string());
                        }
                    }
                    _ => {}
                };
                walk(node, &mut vis);
            }
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_js_parameters(params, source, defined);
                    visit_param_annotations(params, source, defined, refs, typescript);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs, typescript);
                }
            }
            "class_declaration" | "class" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(superclass) = node.child_by_field_name("superclass") {
                    visit(superclass, source, defined, refs, typescript);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs, typescript);
                }
            }
            "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                let name_node = node.child_by_field_name("name");
                if let Some(name) = name_node {
                    defined.insert(node_text(name, source).to_string());
                }
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if name_node.map(|n| n.id()) != Some(child.id()) {
                        visit(child, source, defined, refs, typescript);
                    }
                }
            }
            "variable_declarator" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs, typescript);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    bind_pattern(name, source, defined, refs, typescript);
                }
            }
            "arrow_function"
            | "function"
            | "method_definition"
            | "function_expression"
            | "generator_function" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_js_parameters(params, source, defined);
                    visit_param_annotations(params, source, defined, refs, typescript);
                } else if let Some(param) = node.child_by_field_name("parameter") {
                    bind_pattern(param, source, defined, refs, typescript);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs, typescript);
                }
            }
            "catch_clause" => {
                if let Some(param) = node.child_by_field_name("parameter") {
                    bind_pattern(param, source, defined, refs, typescript);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs, typescript);
                }
            }
            "for_in_statement" | "for_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    if matches!(left.kind(), "variable_declaration" | "lexical_declaration") {
                        let mut vis = |child: Node<'_>| {
                            if child.kind() == "variable_declarator" {
                                if let Some(name) = child.child_by_field_name("name") {
                                    bind_pattern(name, source, defined, refs, typescript);
                                }
                            }
                        };
                        walk(left, &mut vis);
                    } else {
                        bind_pattern(left, source, defined, refs, typescript);
                    }
                }
                for fname in [
                    "right",
                    "condition",
                    "update",
                    "initializer",
                    "increment",
                    "body",
                ] {
                    if let Some(child) = node.child_by_field_name(fname) {
                        visit(child, source, defined, refs, typescript);
                    }
                }
            }
            "member_expression" => {
                if let Some(object) = node.child_by_field_name("object") {
                    visit(object, source, defined, refs, typescript);
                }
            }
            "subscript_expression" => {
                if let Some(object) = node.child_by_field_name("object") {
                    visit(object, source, defined, refs, typescript);
                }
                if let Some(index) = node.child_by_field_name("index") {
                    visit(index, source, defined, refs, typescript);
                }
            }
            "property_identifier"
            | "shorthand_property_identifier"
            | "statement_identifier"
            | "label_identifier" => {}
            "pair" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs, typescript);
                }
            }
            "jsx_attribute" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs, typescript);
                }
            }
            "type_identifier" => {
                if typescript {
                    add_reference(refs, node, source, "type");
                }
            }
            "identifier" => {
                add_reference(refs, node, source, "identifier");
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    visit(child, source, defined, refs, typescript);
                }
            }
        }
    }

    fn collect_js_parameters(params: Node<'_>, source: &str, defined: &mut HashSet<String>) {
        let mut vis = |child: Node<'_>| match child.kind() {
            "identifier" if !is_inside_type_annotation(child) => {
                defined.insert(node_text(child, source).to_string());
            }
            "shorthand_property_identifier_pattern" => {
                defined.insert(node_text(child, source).to_string());
            }
            _ => {}
        };
        walk(params, &mut vis);
    }

    fn is_inside_type_annotation(node: Node<'_>) -> bool {
        let mut current = node.parent();
        while let Some(n) = current {
            match n.kind() {
                "type_annotation" | "type_parameters" | "generic_type" | "predefined_type"
                | "type_arguments" => return true,
                _ => {}
            }
            current = n.parent();
        }
        false
    }

    fn visit_param_annotations<'tree>(
        params: Node<'tree>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
        typescript: bool,
    ) {
        // Collect candidate annotation/default nodes first so we don't
        // pull `defined`/`refs` aliases through the closure during the
        // walk (which would clash with the recursive `visit` borrows).
        let mut to_visit: Vec<Node<'tree>> = Vec::new();
        {
            let mut collect = |child: Node<'tree>| {
                let t = child.kind();
                if t == "type_annotation" {
                    let mut cur = child.walk();
                    for c in child.named_children(&mut cur) {
                        to_visit.push(c);
                    }
                } else if t == "assignment_pattern" {
                    if let Some(right) = child.child_by_field_name("right") {
                        to_visit.push(right);
                    }
                }
            };
            walk(params, &mut collect);
        }

        for n in to_visit {
            visit(n, source, defined, refs, typescript);
        }
    }
}

mod go {
    use super::*;
    use once_cell::sync::Lazy;

    pub(super) static BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
        [
            "true",
            "false",
            "nil",
            "iota",
            "append",
            "cap",
            "close",
            "complex",
            "copy",
            "delete",
            "imag",
            "len",
            "make",
            "new",
            "panic",
            "print",
            "println",
            "real",
            "recover",
            "any",
            "bool",
            "byte",
            "comparable",
            "complex64",
            "complex128",
            "error",
            "float32",
            "float64",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "rune",
            "string",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
            "_",
        ]
        .into_iter()
        .collect()
    });

    pub(super) fn collect(
        root: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        visit(root, source, defined, refs);
    }

    fn visit(
        node: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        match node.kind() {
            "import_spec" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                } else if let Some(path) = node.child_by_field_name("path") {
                    let raw = node_text(path, source).trim_matches('"');
                    if let Some(last) = raw.rsplit('/').next() {
                        defined.insert(last.to_string());
                    }
                }
            }
            "function_declaration" | "method_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    collect_go_field_idents(receiver, source, defined);
                }
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_go_field_idents(params, source, defined);
                }
                if let Some(result) = node.child_by_field_name("result") {
                    visit(result, source, defined, refs);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "type_declaration" => {
                let mut vis = |child: Node<'_>| {
                    if child.kind() == "type_spec" {
                        if let Some(name) = child.child_by_field_name("name") {
                            defined.insert(node_text(name, source).to_string());
                        }
                    }
                };
                walk(node, &mut vis);
                let mut cursor = node.walk();
                for spec in node.named_children(&mut cursor) {
                    if let Some(t) = spec.child_by_field_name("type") {
                        visit(t, source, defined, refs);
                    }
                }
            }
            "short_var_declaration" | "var_spec" | "const_spec" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut cursor = left.walk();
                    for child in left.named_children(&mut cursor) {
                        if child.kind() == "identifier" {
                            defined.insert(node_text(child, source).to_string());
                        }
                    }
                } else {
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() == "identifier" {
                            defined.insert(node_text(child, source).to_string());
                        } else {
                            break;
                        }
                    }
                }
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs);
                }
                if let Some(t) = node.child_by_field_name("type") {
                    visit(t, source, defined, refs);
                }
            }
            "range_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let mut cursor = left.walk();
                    for child in left.named_children(&mut cursor) {
                        if child.kind() == "identifier" {
                            defined.insert(node_text(child, source).to_string());
                        }
                    }
                }
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs);
                }
            }
            "selector_expression" => {
                if let Some(operand) = node.child_by_field_name("operand") {
                    visit(operand, source, defined, refs);
                }
            }
            "field_identifier" | "type_identifier" => {}
            "keyed_element" => {
                let count = node.named_child_count();
                for i in 1..count {
                    if let Some(child) = node.named_child(i as u32) {
                        visit(child, source, defined, refs);
                    }
                }
            }
            "identifier" => {
                add_reference(refs, node, source, "identifier");
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    visit(child, source, defined, refs);
                }
            }
        }
    }

    fn collect_go_field_idents(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
        let mut vis = |child: Node<'_>| {
            if child.kind() == "parameter_declaration" {
                let mut cursor = child.walk();
                for c in child.named_children(&mut cursor) {
                    if c.kind() == "identifier" {
                        defined.insert(node_text(c, source).to_string());
                    }
                }
            }
        };
        walk(node, &mut vis);
    }
}

mod ruby {
    use super::*;
    use once_cell::sync::Lazy;

    pub(super) static BUILTINS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
        [
            "self",
            "super",
            "nil",
            "true",
            "false",
            "__method__",
            "__callee__",
            "Array",
            "Hash",
            "String",
            "Integer",
            "Float",
            "Symbol",
            "Range",
            "Regexp",
            "Proc",
            "Lambda",
            "Module",
            "Class",
            "Object",
            "Kernel",
            "Comparable",
            "Enumerable",
            "Exception",
            "StandardError",
            "RuntimeError",
            "ArgumentError",
            "TypeError",
            "NameError",
            "NoMethodError",
            "IOError",
            "p",
            "pp",
            "print",
            "puts",
            "gets",
            "require",
            "require_relative",
            "load",
            "raise",
            "throw",
            "catch",
            "lambda",
            "proc",
            "yield",
            "attr_reader",
            "attr_writer",
            "attr_accessor",
            "initialize",
            "include",
            "extend",
            "prepend",
            "private",
            "public",
            "protected",
        ]
        .into_iter()
        .collect()
    });

    pub(super) fn collect(
        root: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        visit(root, source, defined, refs);
    }

    fn visit(
        node: Node<'_>,
        source: &str,
        defined: &mut HashSet<String>,
        refs: &mut Vec<UndefinedName>,
    ) {
        match node.kind() {
            "method" | "singleton_method" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(params) = node.child_by_field_name("parameters") {
                    let mut vis = |child: Node<'_>| {
                        if child.kind() == "identifier" || child.kind() == "simple_symbol" {
                            defined.insert(node_text(child, source).to_string());
                        }
                    };
                    walk(params, &mut vis);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "class" | "module" => {
                if let Some(name) = node.child_by_field_name("name") {
                    defined.insert(node_text(name, source).to_string());
                }
                if let Some(body) = node.child_by_field_name("body") {
                    visit(body, source, defined, refs);
                }
            }
            "assignment" | "operator_assignment" => {
                if let Some(right) = node.child_by_field_name("right") {
                    visit(right, source, defined, refs);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "identifier" || left.kind() == "constant" {
                        defined.insert(node_text(left, source).to_string());
                    } else {
                        visit(left, source, defined, refs);
                    }
                }
            }
            "block_parameters" | "method_parameters" => {
                let mut vis = |child: Node<'_>| {
                    if child.kind() == "identifier" {
                        defined.insert(node_text(child, source).to_string());
                    }
                };
                walk(node, &mut vis);
            }
            "call" => {
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    visit(receiver, source, defined, refs);
                } else if let Some(method) = node.child_by_field_name("method") {
                    visit(method, source, defined, refs);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    visit(args, source, defined, refs);
                }
                if let Some(block) = node.child_by_field_name("block") {
                    visit(block, source, defined, refs);
                }
            }
            "hash_key_symbol" | "simple_symbol" => {}
            "pair" => {
                if let Some(value) = node.child_by_field_name("value") {
                    visit(value, source, defined, refs);
                }
            }
            "identifier" | "constant" => {
                add_reference(refs, node, source, "identifier");
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    visit(child, source, defined, refs);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn run_with(content: &str, language: &str) -> VmValue {
        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        dict.insert("content".into(), VmValue::String(Rc::from(content)));
        dict.insert("language".into(), VmValue::String(Rc::from(language)));
        run(&[VmValue::Dict(Rc::new(dict))]).expect("undefined_names run")
    }

    fn names(result: &VmValue) -> Vec<String> {
        let diagnostics = match result {
            VmValue::Dict(d) => match d.get("diagnostics") {
                Some(VmValue::List(l)) => l.clone(),
                _ => panic!("missing diagnostics"),
            },
            _ => panic!("expected dict"),
        };
        diagnostics
            .iter()
            .map(|d| match d {
                VmValue::Dict(dict) => match dict.get("name") {
                    Some(VmValue::String(s)) => s.to_string(),
                    _ => panic!("missing name"),
                },
                _ => panic!("expected dict"),
            })
            .collect()
    }

    fn supported(result: &VmValue) -> bool {
        match result {
            VmValue::Dict(d) => match d.get("supported") {
                Some(VmValue::Bool(b)) => *b,
                _ => panic!("missing supported"),
            },
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn python_flags_undefined_call() {
        let src = "def foo():\n    bar()\n";
        let result = run_with(src, "python");
        let n = names(&result);
        assert_eq!(n, vec!["bar".to_string()]);
    }

    #[test]
    fn python_imports_satisfy_references() {
        let src = "import os\nfrom collections import OrderedDict as OD\nos.path\nOD()\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert!(n.is_empty(), "expected no undefined, got {n:?}");
    }

    #[test]
    fn python_skips_attribute_rhs() {
        let src = "import os\nos.path.join('a', 'b')\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert!(n.is_empty(), "got {n:?}");
    }

    #[test]
    fn javascript_flags_typo() {
        let src = "import { foo } from './m';\nfoo(); baz();\n";
        let result = run_with(src, "js");
        let n = names(&result);
        assert_eq!(n, vec!["baz".to_string()]);
    }

    #[test]
    fn typescript_flags_unknown_type_reference() {
        let src = "function f(x: SomeType) { return x; }\n";
        let result = run_with(src, "ts");
        let n = names(&result);
        assert!(n.contains(&"SomeType".to_string()), "got {n:?}");
    }

    #[test]
    fn go_resolves_imports_and_decls() {
        let src = "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n";
        let result = run_with(src, "go");
        let n = names(&result);
        assert!(n.is_empty(), "got {n:?}");
    }

    #[test]
    fn go_flags_unknown_call() {
        let src = "package main\nfunc main() { mystery() }\n";
        let result = run_with(src, "go");
        let n = names(&result);
        assert_eq!(n, vec!["mystery".to_string()]);
    }

    #[test]
    fn ruby_flags_unknown_call() {
        let src = "def greet(name)\n  hello(name)\nend\n";
        let result = run_with(src, "rb");
        let n = names(&result);
        assert_eq!(n, vec!["hello".to_string()]);
    }

    #[test]
    fn unsupported_language_returns_supported_false() {
        let src = "fn main() {}\n";
        let result = run_with(src, "rust");
        assert!(!supported(&result));
        let n = names(&result);
        assert!(n.is_empty());
    }

    #[test]
    fn missing_payload_is_rejected() {
        let dict: std::collections::BTreeMap<String, VmValue> = std::collections::BTreeMap::new();
        let err = run(&[VmValue::Dict(Rc::new(dict))]).expect_err("must reject");
        match err {
            HostlibError::MissingParameter { builtin, .. } => assert_eq!(builtin, BUILTIN),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn deduplicates_repeated_references() {
        let src = "missing()\nmissing()\nmissing()\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert_eq!(n, vec!["missing".to_string()]);
    }
}
