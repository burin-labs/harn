//! `ast.search` — read-only structural search with full capture bindings.
//!
//! Runs a Tree-Sitter query against a file (or inline `source`) and returns
//! **every** match with **every** capture (`@name`) bound: the capture's
//! text plus its byte/row/col range. This is the structural complement to
//! `fs.find_text` (#2818, regex only) and the foundational read primitive of
//! the rule engine (harn#2827) — matching, `where`-predicates, `transform`,
//! and `fix` interpolation all need the full capture map.
//!
//! `apply_node` already runs the same query machinery but discards every
//! capture except the one span it splices; `search` exposes that machinery
//! read-only, so it is registered without the deterministic-tools write gate.
//!
//! ## Wire shape
//!
//! Request fields (see `schemas/ast/search.request.json`):
//!
//! - `path` **or** `source`: exactly one supplies the bytes to search.
//!   `path` is read from disk (staged-fs aware) and its extension drives
//!   grammar detection; `source` is inline text and therefore *requires*
//!   `language`.
//! - `query`: Tree-Sitter S-expression query with at least one capture.
//! - `language`: optional grammar override (required with `source`).
//! - `max_matches`: optional cap on the number of matches returned; `0`
//!   (default) is unlimited. When the cap truncates results the response
//!   sets `truncated: true` so coverage is never silently bounded.
//!
//! Response is a tagged `result` union: `ok` (matches may be empty),
//! `invalid_query`, or `unsupported_language`. Every match carries a `range`
//! (the bounding span of its captures) and a `captures` map keyed by bare
//! capture name (no leading `@`, so `.harn` callers can dot-access
//! `m.captures.target.text`). Unlike the edit primitives, search never fails
//! on syntax errors — it queries whatever (possibly partial) tree it gets and
//! reports `had_errors` so the caller can decide.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, QueryError, Tree};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};
use crate::tools::permissions::enforce_path_scope;

use super::edit_common::{format_query_error, lossy_str, node_text, read_source};
use super::language::{Language, TEXT_PATCH_FALLBACK};
use super::parse::parse_source;

const BUILTIN: &str = "hostlib_ast_search";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let query_text = require_string(BUILTIN, dict, "query")?;
    let path = optional_string(BUILTIN, dict, "path")?;
    let inline_source = optional_string(BUILTIN, dict, "source")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_matches = optional_int(BUILTIN, dict, "max_matches", 0)?;
    if max_matches < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_matches",
            message: "must be >= 0".into(),
        });
    }

    // Exactly one of `path` / `source` supplies the bytes to search.
    let (source, language) = match (path.as_deref(), inline_source.as_deref()) {
        (Some(path_str), None) => {
            let path = PathBuf::from(path_str);
            enforce_path_scope(BUILTIN, &path, FsAccess::Read)?;
            let language = match Language::detect(&path, language_hint.as_deref()) {
                Some(l) => l,
                None => {
                    return Ok(unsupported_language_response(
                        Some(path_str),
                        language_hint.as_deref(),
                    ))
                }
            };
            // Read-only search: lossy decode is fine, no bytes are written back.
            (lossy_str(&read_source(BUILTIN, &path, None, 0)?), language)
        }
        (None, Some(src)) => {
            // Inline source carries no extension, so a language is required.
            let language = match language_hint.as_deref().and_then(Language::from_name) {
                Some(l) => l,
                None => {
                    return Ok(unsupported_language_response(
                        None,
                        language_hint.as_deref(),
                    ))
                }
            };
            (src.to_string(), language)
        }
        (Some(_), Some(_)) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "source",
                message: "pass exactly one of `path` or `source`, not both".into(),
            });
        }
        (None, None) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "path",
                message: "one of `path` or `source` is required".into(),
            });
        }
    };

    // A recognized language whose grammar family was not compiled into this
    // build degrades to the same graceful response as an unrecognized one.
    let ts_language = match language.ts_language() {
        Some(l) => l,
        None => {
            return Ok(unsupported_language_response(
                path.as_deref(),
                language_hint.as_deref(),
            ))
        }
    };

    let query = match Query::new(&ts_language, &query_text) {
        Ok(q) => q,
        Err(err) => {
            return Ok(invalid_query_response(
                &query_text,
                format_query_error(&err),
                &err,
            ))
        }
    };
    if query.capture_names().is_empty() {
        return Ok(no_capture_response(&query_text));
    }

    let tree = parse_source(&source, language).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: err.to_string(),
    })?;
    let had_errors = tree.root_node().has_error();

    let mut matches = collect_matches(&query, &tree, &source);
    // Document order keeps results stable regardless of the cursor's internal
    // match ordering, so `max_matches` truncation and golden tests are
    // deterministic.
    matches.sort_by_key(|m| (m.range.start_byte, m.range.end_byte));

    let truncated = max_matches > 0 && matches.len() > max_matches as usize;
    if truncated {
        matches.truncate(max_matches as usize);
    }

    Ok(ok_response(language, &matches, truncated, had_errors))
}

/// A captured span: byte offsets plus 0-based row/col, mirroring the
/// coordinate convention every other AST builtin emits.
#[derive(Clone)]
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
}

impl Span {
    fn of(node: Node<'_>) -> Self {
        let start = node.start_position();
        let end = node.end_position();
        Self {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
        }
    }
}

/// One `@name` → node binding within a match.
struct CaptureBinding {
    name: String,
    text: String,
    span: Span,
}

/// One query match: the bounding range of all its captures plus the bound
/// captures themselves.
struct SearchMatch {
    range: Span,
    text: String,
    captures: Vec<CaptureBinding>,
}

fn collect_matches(query: &Query, tree: &Tree, source: &str) -> Vec<SearchMatch> {
    let capture_names = query.capture_names();
    let source_bytes = source.as_bytes();
    let mut cursor = QueryCursor::new();
    let mut iter = cursor.matches(query, tree.root_node(), source_bytes);
    let mut out: Vec<SearchMatch> = Vec::new();

    while let Some(m) = iter.next() {
        if m.captures.is_empty() {
            // A capture-less pattern in a multi-pattern query yields no
            // bindings and no range — nothing for a caller to act on.
            continue;
        }
        let mut captures: Vec<CaptureBinding> = Vec::new();
        let mut start = Span::of(m.captures[0].node);
        let mut end = start.clone();
        for capture in m.captures {
            let span = Span::of(capture.node);
            if span.start_byte < start.start_byte {
                start = span.clone();
            }
            if span.end_byte > end.end_byte {
                end = span.clone();
            }
            let name = capture_names[capture.index as usize];
            // Keep the first node bound to each name. Quantified captures
            // (`(_)+  @x`) can bind a name more than once per match; the
            // single-object wire shape reports the leading binding.
            if !captures.iter().any(|c| c.name == name) {
                captures.push(CaptureBinding {
                    name: name.to_string(),
                    text: node_text(capture.node, source_bytes),
                    span,
                });
            }
        }
        let range = Span {
            start_byte: start.start_byte,
            end_byte: end.end_byte,
            start_row: start.start_row,
            start_col: start.start_col,
            end_row: end.end_row,
            end_col: end.end_col,
        };
        let text = source
            .get(range.start_byte..range.end_byte)
            .unwrap_or_default()
            .to_string();
        out.push(SearchMatch {
            range,
            text,
            captures,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

fn ok_response(
    language: Language,
    matches: &[SearchMatch],
    truncated: bool,
    had_errors: bool,
) -> VmValue {
    build_dict([
        ("result", str_value("ok")),
        ("language", str_value(language.name())),
        ("match_count", VmValue::Int(matches.len() as i64)),
        ("truncated", VmValue::Bool(truncated)),
        ("had_errors", VmValue::Bool(had_errors)),
        (
            "matches",
            VmValue::List(Arc::new(matches.iter().map(match_to_value).collect())),
        ),
    ])
}

fn match_to_value(m: &SearchMatch) -> VmValue {
    let captures: BTreeMap<String, VmValue> = m
        .captures
        .iter()
        .map(|c| {
            (
                c.name.clone(),
                build_dict([
                    ("text", str_value(&c.text)),
                    ("range", span_to_value(&c.span)),
                ]),
            )
        })
        .collect();
    build_dict([
        ("range", span_to_value(&m.range)),
        ("text", str_value(&m.text)),
        ("captures", VmValue::Dict(Arc::new(captures))),
    ])
}

fn span_to_value(span: &Span) -> VmValue {
    build_dict([
        ("start_byte", VmValue::Int(span.start_byte as i64)),
        ("end_byte", VmValue::Int(span.end_byte as i64)),
        ("start_row", VmValue::Int(span.start_row as i64)),
        ("start_col", VmValue::Int(span.start_col as i64)),
        ("end_row", VmValue::Int(span.end_row as i64)),
        ("end_col", VmValue::Int(span.end_col as i64)),
    ])
}

/// Shared skeleton for the non-`ok` variants: every response carries the same
/// core fields so `.harn` consumers never branch on field presence.
fn error_response(
    result: &str,
    extra: impl IntoIterator<Item = (&'static str, VmValue)>,
) -> VmValue {
    let mut entries: Vec<(&'static str, VmValue)> = vec![
        ("result", str_value(result)),
        ("language", VmValue::Nil),
        ("match_count", VmValue::Int(0)),
        ("truncated", VmValue::Bool(false)),
        ("had_errors", VmValue::Bool(false)),
        ("matches", VmValue::List(Arc::new(Vec::new()))),
    ];
    entries.extend(extra);
    build_dict(entries)
}

fn invalid_query_response(query: &str, details: String, err: &QueryError) -> VmValue {
    error_response(
        "invalid_query",
        [
            ("query", str_value(query)),
            ("details", str_value(details)),
            ("error_row", VmValue::Int(err.row as i64)),
            ("error_column", VmValue::Int(err.column as i64)),
        ],
    )
}

fn no_capture_response(query: &str) -> VmValue {
    error_response(
        "invalid_query",
        [
            ("query", str_value(query)),
            (
                "details",
                str_value(
                    "query declares no captures; add at least one `@name` so search can bind results",
                ),
            ),
            ("error_row", VmValue::Int(0)),
            ("error_column", VmValue::Int(0)),
        ],
    )
}

fn unsupported_language_response(path: Option<&str>, hint: Option<&str>) -> VmValue {
    let target = path.unwrap_or("<inline source>");
    error_response(
        "unsupported_language",
        [
            (
                "details",
                str_value(format!(
                    "could not infer a tree-sitter grammar for `{target}` (hint: {})",
                    hint.unwrap_or("none")
                )),
            ),
            ("fallback_suggestion", str_value(TEXT_PATCH_FALLBACK)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(Arc::from(s))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::Dict(Arc::new(map))
    }

    fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        match value {
            VmValue::Dict(d) => d
                .get(key)
                .unwrap_or_else(|| panic!("missing field `{key}`")),
            _ => panic!("expected dict"),
        }
    }

    fn s(value: &VmValue) -> String {
        match value {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn int(value: &VmValue) -> i64 {
        match value {
            VmValue::Int(n) => *n,
            other => panic!("expected int, got {other:?}"),
        }
    }

    fn list(value: &VmValue) -> Vec<VmValue> {
        match value {
            VmValue::List(items) => items.as_ref().clone(),
            other => panic!("expected list, got {other:?}"),
        }
    }

    fn invoke(payload: VmValue) -> VmValue {
        run(&[payload]).expect("search runs")
    }

    /// The `{text, range}` binding for `name` within a single match value.
    fn capture<'a>(m: &'a VmValue, name: &str) -> &'a VmValue {
        field(field(m, "captures"), name)
    }

    /// Capture text by name out of a single match value.
    fn capture_text(m: &VmValue, name: &str) -> String {
        s(field(capture(m, name), "text"))
    }

    #[test]
    fn binds_every_capture_by_name_across_matches() {
        let source = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        let result = invoke(dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            (
                "query",
                vm_string(
                    "(let_declaration pattern: (identifier) @name value: (integer_literal) @value)",
                ),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(s(field(&result, "language")), "rust");
        assert_eq!(int(field(&result, "match_count")), 2);
        let matches = list(field(&result, "matches"));
        assert_eq!(capture_text(&matches[0], "name"), "x");
        assert_eq!(capture_text(&matches[0], "value"), "1");
        assert_eq!(capture_text(&matches[1], "name"), "y");
        assert_eq!(capture_text(&matches[1], "value"), "2");
        // The match range is the bounding span of its captures, so the
        // leading `let` keyword (outside both `@name` and `@value`) is not
        // covered. Capture the enclosing node explicitly to widen the range.
        assert_eq!(s(field(&matches[0], "text")), "x = 1");
    }

    #[test]
    fn capturing_the_enclosing_node_widens_the_match_range() {
        let source = "fn main() {\n    let x = 1;\n}\n";
        let result = invoke(dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("query", vm_string("(let_declaration) @decl")),
        ]));
        let matches = list(field(&result, "matches"));
        assert_eq!(s(field(&matches[0], "text")), "let x = 1;");
    }

    #[test]
    fn match_range_carries_byte_and_rowcol_coordinates() {
        let source = "let a = 7;\n";
        let result = invoke(dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("query", vm_string("(integer_literal) @lit")),
        ]));
        let matches = list(field(&result, "matches"));
        let range = field(capture(&matches[0], "lit"), "range");
        assert_eq!(int(field(range, "start_row")), 0);
        assert_eq!(int(field(range, "start_col")), 8);
        assert_eq!(int(field(range, "start_byte")), 8);
        assert_eq!(int(field(range, "end_byte")), 9);
    }

    #[test]
    fn max_matches_truncates_in_document_order_and_flags_it() {
        let source = "let a = 1; let b = 2; let c = 3;\n";
        let result = invoke(dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("rust")),
            ("query", vm_string("(integer_literal) @lit")),
            ("max_matches", VmValue::Int(2)),
        ]));
        assert_eq!(int(field(&result, "match_count")), 2);
        assert!(matches!(field(&result, "truncated"), VmValue::Bool(true)));
        let matches = list(field(&result, "matches"));
        assert_eq!(capture_text(&matches[0], "lit"), "1");
        assert_eq!(capture_text(&matches[1], "lit"), "2");
    }

    #[test]
    fn finds_optional_chain_nullish_default_in_typescript() {
        // The first rule-engine customer (#2824 / burin-code#1629) folds
        // `obj?.prop ?? default` shapes; the same syntax is valid TypeScript.
        let source = "const v = config?.timeout ?? 30;\n";
        let result = invoke(dict(&[
            ("source", vm_string(source)),
            ("language", vm_string("typescript")),
            (
                "query",
                vm_string(
                    "(binary_expression \
                       left: (member_expression object: (_) @obj (optional_chain) property: (property_identifier) @prop) \
                       right: (_) @default) @match",
                ),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(int(field(&result, "match_count")), 1);
        let m = &list(field(&result, "matches"))[0];
        assert_eq!(capture_text(m, "obj"), "config");
        assert_eq!(capture_text(m, "prop"), "timeout");
        assert_eq!(capture_text(m, "default"), "30");
        assert_eq!(s(field(m, "text")), "config?.timeout ?? 30");
    }

    #[test]
    fn searches_data_grammars_not_just_general_purpose_languages() {
        // Data/markup grammars (JSON here) carry no symbol projection but
        // parse and query like any other — search is grammar-agnostic.
        let result = invoke(dict(&[
            ("source", vm_string("{\"a\": 1, \"b\": 2}\n")),
            ("language", vm_string("json")),
            (
                "query",
                vm_string("(pair key: (string) @k value: (number) @v)"),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(int(field(&result, "match_count")), 2);
        let matches = list(field(&result, "matches"));
        assert_eq!(capture_text(&matches[0], "k"), "\"a\"");
        assert_eq!(capture_text(&matches[0], "v"), "1");
        assert_eq!(capture_text(&matches[1], "k"), "\"b\"");
    }

    #[test]
    fn empty_result_set_is_ok_not_an_error() {
        let result = invoke(dict(&[
            ("source", vm_string("fn main() {}\n")),
            ("language", vm_string("rust")),
            ("query", vm_string("(integer_literal) @lit")),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(int(field(&result, "match_count")), 0);
        assert!(list(field(&result, "matches")).is_empty());
    }

    #[test]
    fn reports_invalid_query() {
        let result = invoke(dict(&[
            ("source", vm_string("fn main() {}\n")),
            ("language", vm_string("rust")),
            ("query", vm_string("((((")),
        ]));
        assert_eq!(s(field(&result, "result")), "invalid_query");
        assert_eq!(int(field(&result, "match_count")), 0);
    }

    #[test]
    fn rejects_query_without_captures() {
        let result = invoke(dict(&[
            ("source", vm_string("fn main() {}\n")),
            ("language", vm_string("rust")),
            ("query", vm_string("(integer_literal)")),
        ]));
        assert_eq!(s(field(&result, "result")), "invalid_query");
        assert!(s(field(&result, "details")).contains("no captures"));
    }

    #[test]
    fn unsupported_language_for_inline_source_without_hint() {
        let result = invoke(dict(&[
            ("source", vm_string("whatever")),
            ("query", vm_string("(x) @y")),
        ]));
        assert_eq!(s(field(&result, "result")), "unsupported_language");
        assert!(!s(field(&result, "fallback_suggestion")).is_empty());
    }

    #[test]
    fn requires_one_of_path_or_source() {
        let err = run(&[dict(&[("query", vm_string("(x) @y"))])])
            .expect_err("missing path/source is an error");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "path"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    #[test]
    fn rejects_both_path_and_source() {
        let err = run(&[dict(&[
            ("query", vm_string("(x) @y")),
            ("path", vm_string("/tmp/x.rs")),
            ("source", vm_string("fn main() {}")),
        ])])
        .expect_err("both path and source is an error");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "source"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    #[test]
    fn reads_from_path_and_detects_language_by_extension() {
        use std::io::Write;
        let mut file = tempfile::Builder::new()
            .suffix(".py")
            .tempfile()
            .expect("temp file");
        file.write_all(b"def greet(name):\n    return name\n")
            .expect("write");
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_definition name: (identifier) @fn)"),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert_eq!(s(field(&result, "language")), "python");
        let m = &list(field(&result, "matches"))[0];
        assert_eq!(capture_text(m, "fn"), "greet");
    }
}
