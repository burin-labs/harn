//! Symbol-targeted source mutation: `extract`, `delete`, `replace`.
//!
//! These builtins provide symbol-scoped read and edit helpers for hosts
//! that operate on line ranges.
//!
//! ## Wire shape
//!
//! Every handler accepts `{ source, language, symbol_name, ... }` and
//! returns a tagged-union dict with a `result` field driving the variant:
//!
//! - `extracted` → `{ text, start_line, end_line }` (1-based, inclusive)
//! - `removed` / `replaced` → `{ source }` (rewritten text)
//! - `not_found` → `{ available, suggestions }`
//! - `ambiguous` → `{ match_count }`
//! - `unsupported_language` → no extra fields
//! - `syntax_error_after_edit` (delete/replace only) → `{ details }`
//! - `parse_failure` (delete/replace only) → `{ details }`
//!
//! Coordinates are 1-based, while the existing `ast.symbols` /
//! `ast.outline` builtins stay 0-based per their tree-sitter native shape.
//! The two conventions live side by side because they target different
//! consumers: tree-sitter-oriented editor features and line-based file
//! editing helpers.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_string, require_string, str_value};

use super::fuzzy;
use super::health::{ParserHealth, ParserOperation};
use super::language::Language;
use super::parse::parse_source;
use super::symbols::extract;
use super::types::Symbol;

const EXTRACT_BUILTIN: &str = "hostlib_ast_symbol_extract";
const DELETE_BUILTIN: &str = "hostlib_ast_symbol_delete";
const REPLACE_BUILTIN: &str = "hostlib_ast_symbol_replace";

/// 1-based, inclusive line range for a located symbol (after preamble
/// expansion).
#[derive(Debug, Clone, Copy)]
struct SymbolRange {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
}

/// Outcome of resolving a `symbol_name` against a parsed source.
enum LocateOutcome {
    Found(SymbolRange),
    NotFound {
        available: Vec<String>,
        suggestions: Vec<String>,
    },
    Ambiguous {
        match_count: usize,
    },
}

pub(super) fn run_extract(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(EXTRACT_BUILTIN, args)?;
    let dict = raw.as_ref();
    let source = require_string(EXTRACT_BUILTIN, dict, "source")?;
    let language_name = require_string(EXTRACT_BUILTIN, dict, "language")?;
    let symbol_name = require_string(EXTRACT_BUILTIN, dict, "symbol_name")?;

    let Some(language) = Language::from_name(&language_name) else {
        let health = ParserHealth::unavailable(None, ParserOperation::Search);
        return Ok(health.attach_to(unsupported_language_response(&language_name)));
    };
    let (tree, health) = source_health(&source, language, ParserOperation::Search)?;

    let lines = source.split('\n').collect::<Vec<&str>>();
    let outcome = locate_symbol(&tree, &source, language, &symbol_name, &lines);
    let response = match outcome {
        LocateOutcome::Found(range) => {
            let text = slice_lines(&lines, range.start_line, range.end_line);
            build_dict([
                ("result", str_value("extracted")),
                ("text", str_value(text)),
                ("start_line", VmValue::Int(range.start_line as i64)),
                ("end_line", VmValue::Int(range.end_line as i64)),
            ])
        }
        LocateOutcome::NotFound {
            available,
            suggestions,
        } => not_found_response(available, suggestions),
        LocateOutcome::Ambiguous { match_count } => ambiguous_response(match_count),
    };
    Ok(health.attach_to(response))
}

pub(super) fn run_delete(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(DELETE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let source = require_string(DELETE_BUILTIN, dict, "source")?;
    let language_name = require_string(DELETE_BUILTIN, dict, "language")?;
    let symbol_name = require_string(DELETE_BUILTIN, dict, "symbol_name")?;

    let Some(language) = Language::from_name(&language_name) else {
        let health = ParserHealth::unavailable(None, ParserOperation::SafeEdit);
        return Ok(health.attach_to(unsupported_language_response(&language_name)));
    };
    let (tree, health) = source_health(&source, language, ParserOperation::SafeEdit)?;
    if !health.is_verified() {
        return Ok(health.attach_to(parser_not_verified_response()));
    }

    let lines = source.split('\n').collect::<Vec<&str>>();
    let outcome = locate_symbol(&tree, &source, language, &symbol_name, &lines);
    match outcome {
        LocateOutcome::Found(range) => {
            let new_lines = remove_range(&lines, range.start_line, range.end_line);
            let collapsed = collapse_blank_lines(new_lines);
            let new_source = collapsed.join("\n");
            match validate_syntax(&new_source, language) {
                Ok(()) => Ok(
                    ParserHealth::observed(language, ParserOperation::SafeEdit, false).attach_to(
                        build_dict([
                            ("result", str_value("removed")),
                            ("source", str_value(&new_source)),
                        ]),
                    ),
                ),
                Err(details) => {
                    Ok(
                        ParserHealth::observed(language, ParserOperation::SafeEdit, true)
                            .attach_to(build_dict([
                                ("result", str_value("syntax_error_after_edit")),
                                ("details", str_value(&details)),
                            ])),
                    )
                }
            }
        }
        LocateOutcome::NotFound {
            available,
            suggestions,
        } => Ok(health.attach_to(not_found_response(available, suggestions))),
        LocateOutcome::Ambiguous { match_count } => {
            Ok(health.attach_to(ambiguous_response(match_count)))
        }
    }
}

pub(super) fn run_replace(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(REPLACE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let source = require_string(REPLACE_BUILTIN, dict, "source")?;
    let language_name = require_string(REPLACE_BUILTIN, dict, "language")?;
    let symbol_name = require_string(REPLACE_BUILTIN, dict, "symbol_name")?;
    let new_text = require_string(REPLACE_BUILTIN, dict, "new_text")?;
    let part =
        optional_string(REPLACE_BUILTIN, dict, "part")?.unwrap_or_else(|| "declaration".into());
    if part != "declaration" && part != "body" {
        return Err(HostlibError::InvalidParameter {
            builtin: REPLACE_BUILTIN,
            param: "part",
            message: "expected declaration or body".into(),
        });
    }

    let Some(language) = Language::from_name(&language_name) else {
        let health = ParserHealth::unavailable(None, ParserOperation::SafeEdit);
        return Ok(health.attach_to(unsupported_language_response(&language_name)));
    };
    let (tree, health) = source_health(&source, language, ParserOperation::SafeEdit)?;
    if !health.is_verified() {
        return Ok(health.attach_to(parser_not_verified_response()));
    }

    let lines = source.split('\n').collect::<Vec<&str>>();
    let outcome = locate_symbol(&tree, &source, language, &symbol_name, &lines);
    match outcome {
        LocateOutcome::Found(range) => {
            let new_source = if part == "body" {
                let Some(candidate) = super::replace_body::replacement(
                    &tree,
                    &source,
                    language,
                    &symbol_name,
                    &new_text,
                ) else {
                    return Ok(health.attach_to(build_dict([
                        ("result", str_value("body_unavailable")),
                        ("details", str_value("body cannot be isolated or reindented without changing token bytes; use an anchored text patch over current source")),
                    ])));
                };
                candidate
            } else {
                super::replace_declaration::replacement(
                    &source,
                    range.start_byte,
                    range.end_byte,
                    language,
                    &new_text,
                )
            };
            match validate_syntax(&new_source, language) {
                Ok(()) => Ok(
                    ParserHealth::observed(language, ParserOperation::SafeEdit, false).attach_to(
                        build_dict([
                            ("result", str_value("replaced")),
                            ("source", str_value(&new_source)),
                        ]),
                    ),
                ),
                Err(details) => {
                    Ok(
                        ParserHealth::observed(language, ParserOperation::SafeEdit, true)
                            .attach_to(build_dict([
                                ("result", str_value("syntax_error_after_edit")),
                                ("details", str_value(&details)),
                                ("candidate", str_value(&new_source)),
                            ])),
                    )
                }
            }
        }
        LocateOutcome::NotFound {
            available,
            suggestions,
        } => Ok(health.attach_to(not_found_response(available, suggestions))),
        LocateOutcome::Ambiguous { match_count } => {
            Ok(health.attach_to(ambiguous_response(match_count)))
        }
    }
}

fn source_health(
    source: &str,
    language: Language,
    operation: ParserOperation,
) -> Result<(tree_sitter::Tree, ParserHealth), HostlibError> {
    let tree = parse_source(source, language)?;
    let health = ParserHealth::observed(language, operation, tree.root_node().has_error());
    Ok((tree, health))
}

fn parser_not_verified_response() -> VmValue {
    build_dict([
        ("result", str_value("parser_not_verified")),
        (
            "details",
            str_value("source structure is not verified; refusing structural mutation"),
        ),
    ])
}

/// Parse `source`, run the existing symbol extractor, then resolve
/// `symbol_name` (optionally `Container.member`) against the extracted
/// list. Returns ranges in 1-based, inclusive form, with the start
/// extended upward to capture decorators / doc comments / attributes
/// that share the symbol's preamble.
fn locate_symbol(
    tree: &tree_sitter::Tree,
    source: &str,
    language: Language,
    symbol_name: &str,
    lines: &[&str],
) -> LocateOutcome {
    let symbols = extract(tree, source, language);

    let parts: Vec<&str> = symbol_name.splitn(2, '.').collect();
    let (qualifier, base_name) = if parts.len() == 2 {
        (Some(parts[0]), parts[1])
    } else {
        (None, parts[0])
    };

    let matches: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| {
            s.name == base_name
                && match qualifier {
                    Some(q) => s.container.as_deref() == Some(q),
                    None => true,
                }
        })
        .collect();

    if matches.len() > 1 {
        return LocateOutcome::Ambiguous {
            match_count: matches.len(),
        };
    }
    let Some(matched) = matches.first().copied() else {
        let available: Vec<String> = symbols.iter().map(|s| s.name.clone()).collect();
        let suggestions = fuzzy::best_matches(symbol_name, &available, 3);
        return LocateOutcome::NotFound {
            available,
            suggestions,
        };
    };

    let decl_line_zero = matched.start_row as usize;
    let starts_after_indentation = lines
        .get(decl_line_zero)
        .and_then(|line| line.get(..matched.start_col as usize))
        .is_some_and(|prefix| prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')));
    let preamble_start_zero = if starts_after_indentation {
        find_preamble_start(lines, decl_line_zero, language)
    } else {
        decl_line_zero
    };
    let start_line = preamble_start_zero + 1;
    let end_line = (matched.end_row as usize)
        .saturating_add(1)
        .min(lines.len());
    let start_col = if preamble_start_zero == decl_line_zero {
        matched.start_col as usize
    } else {
        lines[preamble_start_zero].len()
            - lines[preamble_start_zero]
                .trim_start_matches([' ', '\t'])
                .len()
    };
    let line_offset = |row: usize| {
        lines[..row]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
    };

    LocateOutcome::Found(SymbolRange {
        start_line,
        end_line,
        start_byte: line_offset(preamble_start_zero) + start_col,
        end_byte: line_offset(matched.end_row as usize) + matched.end_col as usize,
    })
}

/// Walk upward from a declaration line and return the 0-based line index
/// where the symbol's preamble starts (decorators, doc comments,
/// attributes).
fn find_preamble_start(lines: &[&str], decl_line_zero: usize, language: Language) -> usize {
    if decl_line_zero == 0 {
        return 0;
    }
    let mut start = decl_line_zero;
    let mut i = decl_line_zero as isize - 1;
    while i >= 0 {
        let trimmed = lines[i as usize].trim();
        if trimmed.is_empty() {
            break;
        }
        if !is_preamble_line(trimmed, language) {
            break;
        }
        start = i as usize;
        i -= 1;
    }
    start
}

fn is_preamble_line(trimmed: &str, language: Language) -> bool {
    match language {
        Language::Python => trimmed.starts_with('@'),
        Language::Rust => {
            trimmed.starts_with("#[") || trimmed.starts_with("///") || trimmed.starts_with("//!")
        }
        Language::Go => trimmed.starts_with("//"),
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            trimmed.starts_with("/**")
                || trimmed.starts_with('*')
                || trimmed.starts_with("*/")
                || trimmed.starts_with('@')
                || trimmed.starts_with("//")
        }
        Language::Swift => trimmed.starts_with("///") || trimmed.starts_with('@'),
        Language::Java | Language::Kotlin => {
            trimmed.starts_with("/**")
                || trimmed.starts_with('*')
                || trimmed.starts_with("*/")
                || trimmed.starts_with('@')
                || trimmed.starts_with("//")
        }
        _ => {
            trimmed.starts_with("///")
                || trimmed.starts_with("//")
                || trimmed.starts_with('@')
                || trimmed.starts_with("#[")
                || trimmed.starts_with("/**")
        }
    }
}

/// Re-parse `source` and surface the first tree-sitter `ERROR` /
/// `MISSING` node as a single-line diagnostic.
fn validate_syntax(source: &str, language: Language) -> Result<(), String> {
    let tree = match parse_source(source, language) {
        Ok(tree) => tree,
        Err(_) => return Err("Failed to parse modified source".into()),
    };
    let root = tree.root_node();
    if !root.has_error() {
        return Ok(());
    }
    let errors = collect_errors(root, source);
    if let Some(first) = errors.into_iter().next() {
        return Err(first);
    }
    Err("post-edit source has parse errors".into())
}

fn collect_errors(root: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "ERROR" || kind.starts_with("MISSING") || node.is_missing() {
            let line = node.start_position().row + 1;
            let snippet = node_text(node, source);
            let trimmed: String = snippet.chars().take(40).collect();
            errors.push(format!("line {line}: unexpected '{trimmed}'"));
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    errors
}

fn node_text(node: tree_sitter::Node<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    if start >= end {
        return String::new();
    }
    std::str::from_utf8(&bytes[start..end])
        .map(|s| s.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Line-range helpers
// ---------------------------------------------------------------------------

/// Slice `lines[start_line - 1 ..= end_line - 1]` (1-based inclusive)
/// and re-join with newlines. Tolerates `end_line` past `lines.len()`.
fn slice_lines(lines: &[&str], start_line: usize, end_line: usize) -> String {
    if start_line == 0 || start_line > lines.len() {
        return String::new();
    }
    let start = start_line - 1;
    let end = end_line.min(lines.len());
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// Remove `lines[start_line - 1 ..= end_line - 1]` (1-based inclusive)
/// and return the surviving lines in document order.
fn remove_range(lines: &[&str], start_line: usize, end_line: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let start = start_line.saturating_sub(1);
    let end = end_line.min(lines.len());
    out.extend(lines[..start].iter().map(|s| (*s).to_string()));
    if end < lines.len() {
        out.extend(lines[end..].iter().map(|s| (*s).to_string()));
    }
    out
}

/// Collapse runs of 3+ blank lines to 2.
fn collapse_blank_lines(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut blank_count: usize = 0;
    for line in lines {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                out.push(line);
            }
        } else {
            blank_count = 0;
            out.push(line);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

fn unsupported_language_response(name: &str) -> VmValue {
    build_dict([
        ("result", str_value("unsupported_language")),
        ("language", str_value(name)),
    ])
}

fn not_found_response(available: Vec<String>, suggestions: Vec<String>) -> VmValue {
    let available_list = VmValue::list(available.into_iter().map(|s| str_value(&s)).collect());
    let suggestions_list = VmValue::list(suggestions.into_iter().map(|s| str_value(&s)).collect());
    let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
    dict.insert("result".into(), str_value("not_found"));
    dict.insert("available".into(), available_list);
    dict.insert("suggestions".into(), suggestions_list);
    VmValue::dict(dict)
}

fn ambiguous_response(match_count: usize) -> VmValue {
    build_dict([
        ("result", str_value("ambiguous")),
        ("match_count", VmValue::Int(match_count as i64)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn dict_field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        match value {
            VmValue::Dict(d) => d.get(key).expect("missing field"),
            _ => panic!("expected dict"),
        }
    }

    fn string_field(value: &VmValue, key: &str) -> String {
        match dict_field(value, key) {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string for {key}, got {other:?}"),
        }
    }

    fn int_field(value: &VmValue, key: &str) -> i64 {
        match dict_field(value, key) {
            VmValue::Int(n) => *n,
            other => panic!("expected int for {key}, got {other:?}"),
        }
    }

    #[test]
    fn collapse_drops_runs_of_three_or_more() {
        let lines = vec!["a", "", "", "", "b"]
            .into_iter()
            .map(String::from)
            .collect();
        let collapsed = collapse_blank_lines(lines);
        assert_eq!(collapsed, vec!["a", "", "", "b"]);
    }

    #[test]
    fn replace_swaps_in_new_text_and_validates() {
        let source = "fn alpha() {}\nfn beta() {}\n";
        let new_text = "fn beta() -> i32 { 42 }";
        let result = run_replace(&[dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("symbol_name", vm_string("beta")),
            ("new_text", vm_string(new_text)),
        ])])
        .expect("replace works");
        assert_eq!(string_field(&result, "result"), "replaced");
        let new_source = string_field(&result, "source");
        assert!(new_source.contains("-> i32 { 42 }"));
    }

    #[test]
    fn not_found_returns_available_and_suggestions() {
        let source = "fn parse_query() {}\nfn parse_other() {}\n";
        let result = run_extract(&[dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("symbol_name", vm_string("parse_qury")),
        ])])
        .expect("extract handler runs");
        assert_eq!(string_field(&result, "result"), "not_found");
        match dict_field(&result, "available") {
            VmValue::List(items) => assert!(!items.is_empty()),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn qualified_name_disambiguates() {
        let source = "class A:\n    def greet(self): pass\nclass B:\n    def greet(self): pass\n";
        let result = run_extract(&[dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("python")),
            ("symbol_name", vm_string("A.greet")),
        ])])
        .expect("extract handler runs");
        assert_eq!(string_field(&result, "result"), "extracted");
        assert_eq!(int_field(&result, "start_line"), 2);
    }

    #[test]
    fn extract_includes_python_decorator_preamble() {
        let source = "@dataclass\nclass Greeter:\n    name: str\n";
        let result = run_extract(&[dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("python")),
            ("symbol_name", vm_string("Greeter")),
        ])])
        .expect("extract handler runs");
        assert_eq!(string_field(&result, "result"), "extracted");
        assert_eq!(int_field(&result, "start_line"), 1);
        assert!(string_field(&result, "text").contains("@dataclass"));
    }

    #[test]
    fn extract_includes_rust_attribute_preamble() {
        let source = "#[test]\nfn it_works() {}\n";
        let result = run_extract(&[dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("symbol_name", vm_string("it_works")),
        ])])
        .expect("extract handler runs");
        assert_eq!(string_field(&result, "result"), "extracted");
        assert_eq!(int_field(&result, "start_line"), 1);
        assert!(string_field(&result, "text").starts_with("#[test]"));
    }
}
