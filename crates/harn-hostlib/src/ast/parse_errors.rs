//! `ast.parse_errors` — surface tree-sitter `ERROR` and `MISSING` nodes
//! plus the count of top-level declarations.
//!
//! This builtin accepts either an in-memory `content` string or a `path`
//! plus an optional `language` hint. Coordinates here are 0-based to match
//! the rest of the `ast::*` builtins.

use std::path::PathBuf;
use std::sync::Arc;

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
                ("errors", VmValue::List(Arc::new(Vec::new()))),
                ("top_level_decl_count", VmValue::Int(0)),
            ]))
        }
    };

    let mut errors: Vec<ParseError> = Vec::new();
    collect_errors(tree.root_node(), source.as_bytes(), &mut errors);
    let top_level = count_top_level_declarations(tree.root_node(), language);
    let had_errors = tree.root_node().has_error() || !errors.is_empty();

    // A grammar-limitation "cascade": tree-sitter could not parse a construct
    // the language genuinely supports (e.g. tree-sitter-scala 0.26 on Scala 3
    // indentation-based `match`/`case`), so it wraps essentially the whole file
    // in one root-level ERROR node spanning from the first line to the last.
    // That is NOT a localized, model-authored syntax mistake — the file is
    // well-formed source the grammar just can't model. Edit-validation gates
    // must not hard-reject a CORRECT create/replace on this signal, or they
    // false-fail correct edits (evidence: eval-scala-feat t2 reported
    // `syntax error: line 1: package rulekit...` on a valid Scala 3 file).
    // We surface this so consumers can downgrade the rejection instead of
    // blaming the model for the grammar's blind spot.
    let total_lines = source_line_count(&source);
    let cascade = errors
        .iter()
        .any(|e| error_spans_full_source(e, total_lines));
    let errors_list: Vec<VmValue> = errors
        .iter()
        .map(|e| e.to_vm_value_with_span(error_spans_full_source(e, total_lines)))
        .collect();

    Ok(build_dict([
        ("path", str_value(path_str.as_deref().unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("had_errors", VmValue::Bool(had_errors)),
        ("errors", VmValue::List(Arc::new(errors_list))),
        ("top_level_decl_count", VmValue::Int(top_level as i64)),
        ("cascade", VmValue::Bool(cascade)),
    ]))
}

/// Count of source lines (number of `\n`-separated segments). Used to decide
/// whether an ERROR node covers essentially the whole file.
fn source_line_count(source: &str) -> u32 {
    if source.is_empty() {
        return 0;
    }
    (source.bytes().filter(|b| *b == b'\n').count() as u32) + 1
}

/// True when an ERROR node starts at the top of the file and spans nearly all
/// of it — the fingerprint of a grammar-limitation cascade rather than a
/// localized syntax mistake. Requires the node to begin on the first line and
/// to cover at least 80% of the source lines (and at least a handful of lines,
/// so a tiny file that is genuinely broken end-to-end is not excused).
fn error_spans_full_source(error: &ParseError, total_lines: u32) -> bool {
    if total_lines < 5 {
        return false;
    }
    if error.start_row != 0 {
        return false;
    }
    let covered = error.end_row.saturating_sub(error.start_row) + 1;
    covered * 100 >= total_lines * 80
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
        Language::Harn => (
            &[
                "pipeline_declaration",
                "import_declaration",
                "fn_declaration",
                "tool_declaration",
                "skill_declaration",
                "eval_pack_declaration",
                "struct_declaration",
                "enum_declaration",
                "interface_declaration",
                "type_declaration",
                "impl_block",
                "attributed_declaration",
            ],
            &["attributed_declaration"],
        ),
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
        | Language::R
        | Language::Json
        | Language::Yaml
        | Language::Toml
        | Language::Css
        | Language::Html
        | Language::Sql
        | Language::Markdown => (&[], &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(content: &str, language: &str) -> VmValue {
        let mut dict: harn_vm::value::DictMap = Default::default();
        dict.insert(
            "content".into(),
            VmValue::String(arcstr::ArcStr::from(content)),
        );
        dict.insert(
            "language".into(),
            VmValue::String(arcstr::ArcStr::from(language)),
        );
        run(&[VmValue::dict(dict)]).expect("parse_errors run")
    }

    fn list_field(value: &VmValue, key: &str) -> Arc<Vec<VmValue>> {
        match value {
            VmValue::Dict(d) => match d.get(key) {
                Some(VmValue::List(l)) => l.clone(),
                _ => panic!("missing list field {key} on {value:?}"),
            },
            _ => panic!("expected dict"),
        }
    }

    fn bool_field(value: &VmValue, key: &str) -> bool {
        match value {
            VmValue::Dict(d) => match d.get(key) {
                Some(VmValue::Bool(b)) => *b,
                other => panic!("expected bool field {key}, got {other:?}"),
            },
            _ => panic!("expected dict"),
        }
    }

    fn err_bool(err: &VmValue, key: &str) -> bool {
        match err {
            VmValue::Dict(d) => matches!(d.get(key), Some(VmValue::Bool(true))),
            _ => false,
        }
    }

    // The decoded body the model authored for eval-scala-feat t2 (`edit`
    // create, `tool_format: "native"` — serde already turned the JSON `\n`
    // escapes into real newlines). It is valid Scala 3, but tree-sitter-scala
    // 0.26 cannot parse the indentation-based `match`/`case` arms, so it wraps
    // nearly the whole file in one root-spanning ERROR node and the gate
    // false-rejected the correct create with `syntax error: line 1: package
    // rulekit...`. The body must reach tree-sitter with REAL newlines (it does)
    // and the result must carry the `cascade` / `spans_full_source` signal so
    // the edit-validation gate can decline to hard-reject a grammar blind spot.
    #[test]
    fn scala3_indented_match_cascade_is_flagged_not_localized() {
        let body = include_str!("scala_repro_fixture.scala");
        // The authored body reaches us with real newlines, not literal `\n`
        // line breaks. (Literal backslash-n only survives inside Scala string /
        // char literals like `case '\n'`, which is correct source text.)
        assert!(
            body.starts_with("package rulekit\n\n"),
            "fixture must have real newlines after the package clause"
        );
        let result = run_with(body, "scala");
        let errors = list_field(&result, "errors");
        assert!(
            !errors.is_empty(),
            "tree-sitter-scala 0.26 should error here"
        );
        // The cascade signal must be set, and the first (root-spanning) error
        // must be marked so consumers can downgrade the rejection.
        assert!(
            bool_field(&result, "cascade"),
            "a root-spanning grammar cascade must set cascade=true"
        );
        assert!(
            errors.iter().any(|e| err_bool(e, "spans_full_source")),
            "the file-spanning ERROR node must be marked spans_full_source"
        );
    }

    // A normal one-line, localized syntax error must NOT be classified as a
    // cascade — only file-spanning grammar blind spots get the escape hatch.
    #[test]
    fn localized_python_error_is_not_a_cascade() {
        let src = "def a():\n    return 1\n\n\ndef b(\n    return 2\n\n\ndef c():\n    return 3\n";
        let result = run_with(src, "python");
        let errors = list_field(&result, "errors");
        assert!(!errors.is_empty());
        assert!(
            !bool_field(&result, "cascade"),
            "a localized error in an otherwise-parseable file is not a cascade"
        );
        assert!(
            !errors.iter().any(|e| err_bool(e, "spans_full_source")),
            "a localized error must not be marked spans_full_source"
        );
    }

    // Simple Scala 3 optional-braces (`object Foo:`) parses cleanly — proves the
    // cascade is specifically the indented `match`/`case` blind spot, and that
    // we are not blanket-excusing Scala.
    #[test]
    fn simple_scala3_optional_braces_has_no_errors() {
        let src = "package rulekit\n\nobject Foo:\n  val x: Int = 1\n";
        let result = run_with(src, "scala");
        let errors = list_field(&result, "errors");
        assert!(errors.is_empty(), "expected clean parse, got {errors:?}");
        assert!(!bool_field(&result, "cascade"));
    }

    // The Kotlin file the model authored for eval-kotlin-workflow t1 (`edit`
    // create, native tool_format) is valid and parses with ZERO errors when
    // checked standalone — proving the reported `line 150: }` rejection was a
    // a host-side `replace_body` brace-splice defect on a LATER edit, not a
    // newline-corruption or a grammar gap in the authored content. The body
    // carries real newlines and no literal `\n`.
    #[test]
    fn kotlin_authored_test_file_parses_clean() {
        let body = include_str!("kotlin_repro_fixture.kt");
        assert!(
            !body.contains("\\n"),
            "authored Kotlin body must not contain literal backslash-n"
        );
        let result = run_with(body, "kotlin");
        let errors = list_field(&result, "errors");
        assert!(errors.is_empty(), "expected clean parse, got {errors:?}");
        assert!(!bool_field(&result, "cascade"));
    }

    // Swift optional-chained subscripts are ordinary production syntax. The
    // 0.7.3 grammar release regressed this expression when it is followed by
    // a conditional cast and nil-coalescing fallback, which made valid Burin
    // sources look like model-authored syntax errors to AST consumers.
    #[test]
    fn swift_optional_chained_subscript_with_fallback_parses_clean() {
        let source = r#"
import Foundation

func message(from notification: Notification) -> String {
    notification.userInfo?["message"] as? String ?? "Collaboration error"
}
"#;
        let result = run_with(source, "swift");
        let errors = list_field(&result, "errors");
        assert!(
            errors.is_empty(),
            "valid Swift optional chaining must parse clean, got {errors:?}"
        );
        assert!(!bool_field(&result, "cascade"));
    }

    // Guard against over-unescaping: a JSON-arg body with `\\n` (an intended
    // literal backslash-n in authored code, e.g. a Go/Kotlin string literal)
    // must survive as the two characters `\` + `n` in the source we validate,
    // not collapse into a real newline. tree-sitter validation operates on the
    // already-decoded bytes, so this is a property of those bytes.
    #[test]
    fn literal_backslash_n_in_string_literal_survives() {
        // Represents the decoded bytes of a Go const: `const nl = "\n"` where
        // the `\n` is an escape INSIDE the Go string, not a line break.
        let src = "package main\n\nconst nl = \"\\n\"\n";
        // The validated source has a backslash followed by 'n' inside quotes,
        // and a real newline only between top-level lines.
        assert!(src.contains("\"\\n\""), "string literal escape preserved");
        let result = run_with(src, "go");
        let errors = list_field(&result, "errors");
        assert!(
            errors.is_empty(),
            "valid Go must parse clean, got {errors:?}"
        );
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
        let dict: harn_vm::value::DictMap = Default::default();
        let err = run(&[VmValue::dict(dict)]).expect_err("must reject empty payload");
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
