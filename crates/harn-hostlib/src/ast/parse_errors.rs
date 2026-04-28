//! `ast.parse_errors` — surface tree-sitter `ERROR` and `MISSING` nodes
//! plus the count of top-level declarations.
//!
//! This builtin accepts either an in-memory `content` string or a `path`
//! plus an optional `language` hint. Coordinates here are 0-based to match
//! the rest of the `ast::*` builtins.

use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;
use tree_sitter::Node;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, optional_string, str_value};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::types::ParseError;

const BUILTIN: &str = "hostlib_ast_parse_errors";

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
    let source = match (&content, &path_str) {
        (Some(text), _) => truncate_source(text, max_bytes as usize),
        (None, Some(path)) => read_source(path, max_bytes as usize)?,
        _ => unreachable!("guarded above"),
    };

    let tree = match parse_source(&source, language) {
        Ok(tree) => tree,
        Err(_) => {
            return Ok(build_dict([
                ("path", str_value(path_str.as_deref().unwrap_or(""))),
                ("language", str_value(language.name())),
                ("supported", VmValue::Bool(false)),
                ("had_errors", VmValue::Bool(false)),
                ("errors", VmValue::List(Rc::new(Vec::new()))),
                ("top_level_decl_count", VmValue::Int(0)),
            ]))
        }
    };

    let mut errors: Vec<ParseError> = Vec::new();
    collect_errors(tree.root_node(), source.as_bytes(), &mut errors);
    let top_level = count_top_level_declarations(tree.root_node(), language);
    let had_errors = tree.root_node().has_error() || !errors.is_empty();

    let errors_list: Vec<VmValue> = errors.iter().map(ParseError::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(path_str.as_deref().unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("had_errors", VmValue::Bool(had_errors)),
        ("errors", VmValue::List(Rc::new(errors_list))),
        ("top_level_decl_count", VmValue::Int(top_level as i64)),
    ]))
}

fn resolve_language(
    path: Option<&str>,
    language_hint: Option<&str>,
) -> Result<Language, HostlibError> {
    if let Some(name) = language_hint.filter(|s| !s.is_empty()) {
        // Accept either a canonical wire name or a bare extension here so
        // callers that only know the file extension don't need a separate
        // translation step.
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

fn truncate_source(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }
    // Trim to the last UTF-8 boundary at or below max_bytes so we never
    // hand tree-sitter a half-codepoint.
    let bytes = text.as_bytes();
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Depth-first walk; record any node that's flagged ERROR or MISSING.
fn collect_errors(root: Node<'_>, source: &[u8], out: &mut Vec<ParseError>) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "ERROR" || node.is_missing() {
            let start = node.start_position();
            let end = node.end_position();
            let raw = source
                .get(node.start_byte()..node.end_byte())
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("");
            let snippet_raw: String = raw.chars().take(60).collect();
            let snippet = snippet_raw.replace('\n', "\\n");
            let message = if node.is_missing() {
                // tree-sitter's `is_missing` marks the absence of a literal
                // grammar token. The node `kind` is the token that should
                // have been there.
                format!("missing '{kind}'")
            } else if snippet.is_empty() {
                "unexpected syntax".to_string()
            } else {
                format!("unexpected '{snippet}'")
            };
            out.push(ParseError {
                start_row: start.row as u32,
                start_col: start.column as u32,
                end_row: end.row as u32,
                end_col: end.column as u32,
                start_byte: node.start_byte() as u32,
                end_byte: node.end_byte() as u32,
                message,
                snippet,
                missing: node.is_missing(),
            });
        }
        // Visit children in reverse so the natural order is preserved
        // when popping off the stack.
        let mut cursor = node.walk();
        let mut children: Vec<Node<'_>> = Vec::new();
        for child in node.children(&mut cursor) {
            children.push(child);
        }
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    // Sort by start_byte so the wire order matches a left-to-right pass.
    out.sort_by_key(|e| e.start_byte);
}

/// Count top-level declarations in `root` for `language`. The (decls,
/// wrappers) pair determines what counts as a declaration and which
/// container kinds get expanded one level (e.g. TypeScript's
/// `export_statement` wrapping a `function_declaration`).
fn count_top_level_declarations(root: Node<'_>, language: Language) -> u32 {
    let (decls, wrappers) = declaration_kinds(language);
    if decls.is_empty() {
        return 0;
    }
    let mut count: u32 = 0;
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = child.kind();
        if decls.contains(&kind) {
            count += 1;
        }
        if wrappers.contains(&kind) {
            let mut inner = child.walk();
            for grandchild in child.children(&mut inner) {
                if decls.contains(&grandchild.kind()) {
                    count += 1;
                }
            }
        }
    }
    count
}

#[allow(clippy::type_complexity)]
fn declaration_kinds(language: Language) -> (&'static [&'static str], &'static [&'static str]) {
    match language {
        Language::TypeScript | Language::Tsx => (
            &[
                "function_declaration",
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "type_alias_declaration",
                "lexical_declaration",
                "export_statement",
            ],
            &["export_statement"],
        ),
        Language::JavaScript | Language::Jsx => (
            &[
                "function_declaration",
                "class_declaration",
                "lexical_declaration",
                "export_statement",
            ],
            &["export_statement"],
        ),
        Language::Go => (
            &[
                "function_declaration",
                "method_declaration",
                "type_declaration",
            ],
            &[],
        ),
        Language::Rust => (
            &[
                "function_item",
                "struct_item",
                "enum_item",
                "trait_item",
                "impl_item",
                "type_item",
            ],
            &["impl_item"],
        ),
        Language::Python => (
            &[
                "function_definition",
                "class_definition",
                "decorated_definition",
            ],
            &[],
        ),
        Language::Java => (
            &[
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "method_declaration",
            ],
            &[],
        ),
        Language::C => (
            &[
                "function_definition",
                "struct_specifier",
                "enum_specifier",
                "type_definition",
                "declaration",
            ],
            &[],
        ),
        Language::Cpp => (
            &[
                "function_definition",
                "class_specifier",
                "struct_specifier",
                "enum_specifier",
                "namespace_definition",
                "template_declaration",
            ],
            &["namespace_definition"],
        ),
        Language::Kotlin => (
            &[
                "function_declaration",
                "class_declaration",
                "object_declaration",
                "interface_declaration",
            ],
            &[],
        ),
        Language::Ruby => (
            &["class", "module", "method", "singleton_method"],
            &["module"],
        ),
        Language::CSharp => (
            &[
                "class_declaration",
                "struct_declaration",
                "interface_declaration",
                "enum_declaration",
                "method_declaration",
                "namespace_declaration",
            ],
            &["namespace_declaration"],
        ),
        Language::Php => (
            &[
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "function_definition",
                "method_declaration",
            ],
            &[],
        ),
        Language::Scala => (
            &[
                "class_definition",
                "trait_definition",
                "object_definition",
                "enum_definition",
                "function_definition",
                "type_definition",
            ],
            &["object_definition"],
        ),
        // Languages without an explicit profile contribute no top-level
        // count. The wire field stays present; consumers can ignore it.
        Language::Bash
        | Language::Swift
        | Language::Zig
        | Language::Elixir
        | Language::Lua
        | Language::Haskell
        | Language::R => (&[], &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(content: &str, language: &str) -> VmValue {
        use std::collections::BTreeMap;
        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        dict.insert("content".into(), VmValue::String(Rc::from(content)));
        dict.insert("language".into(), VmValue::String(Rc::from(language)));
        run(&[VmValue::Dict(Rc::new(dict))]).expect("parse_errors run")
    }

    fn list_field(value: &VmValue, key: &str) -> Rc<Vec<VmValue>> {
        match value {
            VmValue::Dict(d) => match d.get(key) {
                Some(VmValue::List(l)) => l.clone(),
                _ => panic!("missing list field {key} on {value:?}"),
            },
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn clean_python_source_has_no_errors() {
        let result = run_with("x = 1\n", "python");
        let errors = list_field(&result, "errors");
        assert!(errors.is_empty());
    }

    #[test]
    fn missing_close_paren_in_python_is_flagged() {
        let result = run_with("def foo(\n    pass\n", "py");
        let errors = list_field(&result, "errors");
        assert!(!errors.is_empty(), "expected errors, got {errors:?}");
        // At least one entry should be either ERROR or MISSING.
        let any_missing = errors.iter().any(|err| match err {
            VmValue::Dict(d) => matches!(d.get("missing"), Some(VmValue::Bool(true))),
            _ => false,
        });
        let any_error_msg = errors.iter().any(|err| match err {
            VmValue::Dict(d) => matches!(
                d.get("message"),
                Some(VmValue::String(s)) if !s.is_empty()
            ),
            _ => false,
        });
        assert!(any_missing || any_error_msg);
    }

    #[test]
    fn typescript_top_level_decl_count_includes_exports() {
        let source = "export function foo() {}\nexport const bar = 1;\n";
        let result = run_with(source, "typescript");
        let count = match &result {
            VmValue::Dict(d) => match d.get("top_level_decl_count") {
                Some(VmValue::Int(n)) => *n,
                _ => panic!("missing top_level_decl_count"),
            },
            _ => panic!("expected dict"),
        };
        assert!(count >= 2, "expected >= 2 top-level decls, got {count}");
    }

    #[test]
    fn rejects_when_no_content_or_path() {
        use std::collections::BTreeMap;
        let dict: BTreeMap<String, VmValue> = BTreeMap::new();
        let err = run(&[VmValue::Dict(Rc::new(dict))]).expect_err("must reject empty payload");
        match err {
            HostlibError::MissingParameter { builtin, param } => {
                assert_eq!(builtin, BUILTIN);
                assert_eq!(param, "content_or_path");
            }
            other => panic!("expected MissingParameter, got {other:?}"),
        }
    }

    #[test]
    fn extension_is_accepted_as_language_alias() {
        // Accept both a file extension (e.g. "py") and the canonical wire
        // name.
        let result = run_with("x = 1\n", "py");
        let language = match &result {
            VmValue::Dict(d) => match d.get("language") {
                Some(VmValue::String(s)) => s.to_string(),
                _ => panic!("missing language"),
            },
            _ => panic!("expected dict"),
        };
        assert_eq!(language, "python");
    }
}
