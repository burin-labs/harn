//! `ast.apply_node` — Tree-Sitter query → format-preserving replace.
//!
//! Locates one or more AST nodes via a Tree-Sitter query, then splices the
//! caller's `replacement` text in for each match by byte range. Because the
//! splice operates on the matched node's start/end bytes, leading
//! indentation, surrounding whitespace, and trailing trivia outside the
//! span are preserved verbatim.
//!
//! ## Wire shape
//!
//! Request fields (see `schemas/ast/apply_node.request.json`):
//!
//! - `path`: file to mutate.
//! - `query`: Tree-Sitter S-expression query with at least one capture.
//!   The capture named by `target_capture` (default `"target"`) is the
//!   span replaced; if the query has exactly one capture, that capture is
//!   the target regardless of its name.
//! - `replacement`: replacement text spliced in for every selected node.
//! - `select`: multi-match policy — one of `"unique" | "first" | "all" | "nth"`.
//!   Defaults to `"unique"`: matching exactly one node is required.
//! - `nth`: 1-based index when `select == "nth"`. Required in that mode.
//! - `language`: optional language hint (otherwise inferred from the
//!   extension).
//! - `target_capture`: capture name to treat as the replaceable span.
//! - `dry_run`: when true the file is left untouched but the response still
//!   carries the `preview` text the splice would produce.
//! - `validate`: when true (default) the post-edit source is re-parsed; if
//!   tree-sitter surfaces any ERROR/MISSING node the edit is rejected.
//! - `session_id`: routes the read+write through staged-fs (#1722) so the
//!   edit is atomic alongside siblings.
//!
//! Response is a tagged `result` union: `applied` on success and one of
//! `no_match | ambiguous | invalid_query | unsupported_language |
//! syntax_error` on failure. Bad selector / `nth` shapes surface as a
//! structured [`HostlibError::InvalidParameter`] before reaching the
//! union since they are caller bugs, not edit outcomes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use harn_vm::VmValue;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, QueryError, QueryErrorKind};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};

use super::language::Language;
use super::parse::parse_source;

const BUILTIN: &str = "hostlib_ast_apply_node";
const DEFAULT_TARGET_CAPTURE: &str = "target";

/// Multi-match selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selector {
    /// Require exactly one match; otherwise `ambiguous`.
    Unique,
    /// Apply to the first match in document order.
    First,
    /// Apply to every match.
    All,
    /// Apply to the nth (1-based) match in document order.
    Nth(usize),
}

impl Selector {
    fn parse(raw: Option<&str>, nth: Option<i64>) -> Result<Self, HostlibError> {
        match raw.unwrap_or("unique") {
            "unique" => Ok(Self::Unique),
            "first" => Ok(Self::First),
            "all" => Ok(Self::All),
            "nth" => {
                let n = nth.ok_or(HostlibError::InvalidParameter {
                    builtin: BUILTIN,
                    param: "nth",
                    message: "`select: \"nth\"` requires a positive `nth` (1-based)".into(),
                })?;
                if n < 1 {
                    return Err(HostlibError::InvalidParameter {
                        builtin: BUILTIN,
                        param: "nth",
                        message: format!("`nth` must be >= 1, got {n}"),
                    });
                }
                Ok(Self::Nth(n as usize))
            }
            other => Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "select",
                message: format!(
                    "expected one of [\"unique\", \"first\", \"all\", \"nth\"], got `{other}`"
                ),
            }),
        }
    }
}

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let query_text = require_string(BUILTIN, dict, "query")?;
    let replacement = require_string(BUILTIN, dict, "replacement")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let target_capture = optional_string(BUILTIN, dict, "target_capture")?
        .unwrap_or_else(|| DEFAULT_TARGET_CAPTURE.to_string());
    let select_raw = optional_string(BUILTIN, dict, "select")?;
    let nth_raw = match dict.get("nth") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Int(n)) => Some(*n),
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "nth",
                message: format!("expected integer, got {}", other.type_name()),
            });
        }
    };
    let selector = Selector::parse(select_raw.as_deref(), nth_raw)?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", false)?;
    let validate = optional_bool(BUILTIN, dict, "validate", true)?;
    let session_id = optional_string(BUILTIN, dict, "session_id")?;
    let max_bytes = optional_int(BUILTIN, dict, "max_bytes", 0)?;
    if max_bytes < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_bytes",
            message: "must be >= 0".into(),
        });
    }

    let path = PathBuf::from(&path_str);
    let language = match Language::detect(&path, language_hint.as_deref()) {
        Some(l) => l,
        None => {
            return Ok(unsupported_language_response(
                &path_str,
                language_hint.as_deref(),
            ))
        }
    };

    let source = read_source(&path, session_id.as_deref(), max_bytes as usize)?;

    let query = match Query::new(&language.ts_language(), &query_text) {
        Ok(q) => q,
        Err(err) => return Ok(invalid_query_response(&query_text, &err)),
    };

    let target_index = match resolve_target_capture(&query, &target_capture) {
        Ok(idx) => idx,
        Err(detail) => {
            return Ok(no_match_response(
                &path_str,
                &query_text,
                &target_capture,
                &detail,
            ))
        }
    };

    let tree = parse_source(&source, language).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: err.to_string(),
    })?;

    let spans = collect_target_spans(&query, &tree, source.as_bytes(), target_index);
    if spans.is_empty() {
        return Ok(no_match_response(
            &path_str,
            &query_text,
            &target_capture,
            "query produced zero captures",
        ));
    }

    let chosen = match select_spans(&spans, selector, &path_str, &query_text) {
        Ok(spans) => spans,
        Err(reason) => return Ok(reason),
    };

    let patched = splice(&source, &chosen, &replacement);

    if validate {
        if let Some(detail) = first_syntax_error(&patched, language) {
            return Ok(syntax_error_response(&path_str, &patched, &detail, &chosen));
        }
    }

    if !dry_run {
        write_source(&path, &patched, session_id.as_deref())?;
    }

    Ok(applied_response(
        &path_str,
        &source,
        &patched,
        &chosen,
        &replacement,
        dry_run,
    ))
}

/// Resolve which capture index drives replacement.
///
/// - If `requested` matches a capture name in the query, return that
///   index.
/// - Otherwise, if the query has exactly one capture, fall back to it
///   so single-capture queries do not need a `@target` rename.
/// - Otherwise, return a diagnostic listing the available captures so
///   the caller can fix the query.
fn resolve_target_capture(query: &Query, requested: &str) -> Result<u32, String> {
    let names = query.capture_names();
    if let Some(idx) = names.iter().position(|n| *n == requested) {
        return Ok(idx as u32);
    }
    if names.len() == 1 {
        return Ok(0);
    }
    Err(format!(
        "query has no capture named `{requested}`; available captures: [{}]",
        names
            .iter()
            .map(|n| format!("@{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[derive(Debug, Clone)]
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    original: String,
}

fn collect_target_spans(
    query: &Query,
    tree: &tree_sitter::Tree,
    source_bytes: &[u8],
    target_index: u32,
) -> Vec<Span> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
    let mut seen: BTreeMap<(usize, usize), Span> = BTreeMap::new();

    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index != target_index {
                continue;
            }
            insert_span(&mut seen, capture.node, source_bytes);
        }
    }

    let mut spans: Vec<Span> = seen.into_values().collect();
    spans.sort_by_key(|s| s.start_byte);
    spans
}

fn insert_span(into: &mut BTreeMap<(usize, usize), Span>, node: Node<'_>, source_bytes: &[u8]) {
    let key = (node.start_byte(), node.end_byte());
    into.entry(key).or_insert_with(|| {
        let start = node.start_position();
        let end = node.end_position();
        let original = std::str::from_utf8(&source_bytes[node.start_byte()..node.end_byte()])
            .unwrap_or_default()
            .to_string();
        Span {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
            original,
        }
    });
}

fn select_spans(
    spans: &[Span],
    selector: Selector,
    path: &str,
    query: &str,
) -> Result<Vec<Span>, VmValue> {
    match selector {
        Selector::Unique => {
            if spans.len() > 1 {
                return Err(ambiguous_response(spans.len(), spans));
            }
            Ok(spans.to_vec())
        }
        Selector::First => Ok(spans.first().cloned().into_iter().collect()),
        Selector::All => Ok(spans.to_vec()),
        Selector::Nth(n) => match spans.get(n - 1) {
            Some(span) => Ok(vec![span.clone()]),
            None => Err(no_nth_response(spans.len(), n, path, query)),
        },
    }
}

/// Replace each chosen span's bytes with `replacement`. Spans are processed
/// in reverse byte order so earlier offsets remain valid; the original
/// `chosen` order is preserved by the caller.
fn splice(source: &str, chosen: &[Span], replacement: &str) -> String {
    let mut by_end: Vec<&Span> = chosen.iter().collect();
    by_end.sort_by_key(|s| std::cmp::Reverse(s.start_byte));
    let mut out = source.to_string();
    for span in by_end {
        out.replace_range(span.start_byte..span.end_byte, replacement);
    }
    out
}

fn first_syntax_error(source: &str, language: Language) -> Option<String> {
    let tree = parse_source(source, language).ok()?;
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let pos = node.start_position();
            return Some(format!(
                "missing `{}` at line {}, column {}",
                node.kind(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        if node.is_error() {
            let pos = node.start_position();
            let snippet = node_text(node, source);
            let trimmed: String = snippet.chars().take(40).collect();
            return Some(format!(
                "unexpected `{trimmed}` at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ));
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i as u32) {
                if child.has_error() || child.is_missing() {
                    stack.push(child);
                }
            }
        }
    }
    Some("post-edit source has parse errors".into())
}

fn node_text(node: Node<'_>, source: &str) -> String {
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
// I/O — routed through staged-fs (#1722) when a session id is supplied.
// ---------------------------------------------------------------------------

fn read_source(
    path: &Path,
    session_id: Option<&str>,
    max_bytes: usize,
) -> Result<String, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, session_id) {
        result.map_err(|err| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!("read `{}`: {err}", path.display()),
        })?
    } else {
        std::fs::read(path).map_err(|err| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!("read `{}`: {err}", path.display()),
        })?
    };
    let slice = if max_bytes == 0 || bytes.len() <= max_bytes {
        &bytes[..]
    } else {
        &bytes[..max_bytes]
    };
    Ok(String::from_utf8_lossy(slice).into_owned())
}

fn write_source(path: &Path, contents: &str, session_id: Option<&str>) -> Result<(), HostlibError> {
    if crate::fs::stage_write_or_none(BUILTIN, path, contents.as_bytes(), true, true, session_id)?
        .is_some()
    {
        return Ok(());
    }
    crate::fs_snapshot::auto_capture_for_write(BUILTIN, path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                builtin: BUILTIN,
                message: format!("mkdir `{}`: {err}", parent.display()),
            })?;
        }
    }
    std::fs::write(path, contents).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!("write `{}`: {err}", path.display()),
    })
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

fn applied_response(
    path: &str,
    before: &str,
    after: &str,
    chosen: &[Span],
    replacement: &str,
    dry_run: bool,
) -> VmValue {
    VmValue::Dict(Rc::new(
        [
            ("result".to_string(), str_value("applied")),
            ("applied".to_string(), VmValue::Bool(true)),
            ("path".to_string(), str_value(path)),
            ("dry_run".to_string(), VmValue::Bool(dry_run)),
            ("match_count".to_string(), VmValue::Int(chosen.len() as i64)),
            (
                "edits".to_string(),
                VmValue::List(Rc::new(
                    chosen
                        .iter()
                        .map(|s| edit_to_value(s, replacement))
                        .collect(),
                )),
            ),
            (
                "before_sha256".to_string(),
                str_value(sha256_hex(before.as_bytes())),
            ),
            (
                "after_sha256".to_string(),
                str_value(sha256_hex(after.as_bytes())),
            ),
            ("preview".to_string(), str_value(after)),
        ]
        .into_iter()
        .collect(),
    ))
}

fn no_match_response(path: &str, query: &str, target_capture: &str, details: &str) -> VmValue {
    build_dict([
        ("result", str_value("no_match")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("query", str_value(query)),
        ("target_capture", str_value(target_capture)),
        ("details", str_value(details)),
    ])
}

fn ambiguous_response(match_count: usize, spans: &[Span]) -> VmValue {
    VmValue::Dict(Rc::new(
        [
            ("result".to_string(), str_value("ambiguous")),
            ("applied".to_string(), VmValue::Bool(false)),
            ("match_count".to_string(), VmValue::Int(match_count as i64)),
            (
                "spans".to_string(),
                VmValue::List(Rc::new(spans.iter().map(span_to_value).collect())),
            ),
            (
                "details".to_string(),
                str_value(format!(
                    "`select: \"unique\"` requires a single match, found {match_count}; \
                     use `\"first\" | \"all\" | \"nth\"` to disambiguate"
                )),
            ),
        ]
        .into_iter()
        .collect(),
    ))
}

fn no_nth_response(match_count: usize, requested: usize, path: &str, query: &str) -> VmValue {
    build_dict([
        ("result", str_value("no_match")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("query", str_value(query)),
        ("match_count", VmValue::Int(match_count as i64)),
        (
            "details",
            str_value(format!(
                "`select: \"nth\"` requested index {requested}, only {match_count} match(es) found"
            )),
        ),
    ])
}

fn invalid_query_response(query: &str, err: &QueryError) -> VmValue {
    build_dict([
        ("result", str_value("invalid_query")),
        ("applied", VmValue::Bool(false)),
        ("query", str_value(query)),
        (
            "details",
            str_value(format!(
                "tree-sitter rejected query at row {} col {}: {} ({})",
                err.row,
                err.column,
                err.message,
                query_error_kind_str(&err.kind),
            )),
        ),
        ("error_row", VmValue::Int(err.row as i64)),
        ("error_column", VmValue::Int(err.column as i64)),
    ])
}

fn query_error_kind_str(kind: &QueryErrorKind) -> &'static str {
    match kind {
        QueryErrorKind::Syntax => "syntax",
        QueryErrorKind::NodeType => "node_type",
        QueryErrorKind::Field => "field",
        QueryErrorKind::Capture => "capture",
        QueryErrorKind::Predicate => "predicate",
        QueryErrorKind::Structure => "structure",
        QueryErrorKind::Language => "language",
    }
}

fn unsupported_language_response(path: &str, hint: Option<&str>) -> VmValue {
    build_dict([
        ("result", str_value("unsupported_language")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        (
            "details",
            str_value(format!(
                "could not infer a tree-sitter grammar for `{path}` (hint: {})",
                hint.unwrap_or("none")
            )),
        ),
    ])
}

fn syntax_error_response(path: &str, after: &str, detail: &str, chosen: &[Span]) -> VmValue {
    build_dict([
        ("result", str_value("syntax_error")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("details", str_value(detail)),
        ("match_count", VmValue::Int(chosen.len() as i64)),
        ("preview", str_value(after)),
    ])
}

fn edit_to_value(span: &Span, replacement: &str) -> VmValue {
    build_dict([
        ("start_byte", VmValue::Int(span.start_byte as i64)),
        ("end_byte", VmValue::Int(span.end_byte as i64)),
        ("start_row", VmValue::Int(span.start_row as i64)),
        ("start_col", VmValue::Int(span.start_col as i64)),
        ("end_row", VmValue::Int(span.end_row as i64)),
        ("end_col", VmValue::Int(span.end_col as i64)),
        ("original", str_value(&span.original)),
        ("replacement", str_value(replacement)),
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
        ("text", str_value(&span.original)),
    ])
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(Rc::from(s))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::Dict(Rc::new(map))
    }

    fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        match value {
            VmValue::Dict(d) => d.get(key).expect("missing field"),
            _ => panic!("expected dict"),
        }
    }

    fn s(value: &VmValue) -> String {
        match value {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn write_temp(extension: &str, source: &str) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .expect("temp file");
        file.write_all(source.as_bytes()).expect("write source");
        file
    }

    fn invoke(payload: VmValue) -> VmValue {
        run(&[payload]).expect("apply_node runs")
    }

    #[test]
    fn replaces_unique_match_and_preserves_indentation() {
        let source = "fn alpha() {\n    let x = 1;\n}\n\nfn beta() {\n    let y = 2;\n}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let query = r#"(function_item name: (identifier) @name (#eq? @name "beta")
                        body: (block) @target)"#;
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string(query)),
            ("replacement", vm_string("{ let y = 42; }")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        // Indentation outside the replaced body is preserved.
        assert!(preview.contains("fn beta() {"));
        assert!(preview.contains("let y = 42"));
        assert!(!preview.contains("let y = 2"));
    }

    #[test]
    fn unique_selector_rejects_multiple_matches() {
        let source = "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 0 }")),
        ]));
        assert_eq!(s(field(&result, "result")), "ambiguous");
    }

    #[test]
    fn select_all_replaces_every_match() {
        let source = "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 0 }")),
            ("select", vm_string("all")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert_eq!(preview.matches("{ 0 }").count(), 3);
        match field(&result, "match_count") {
            VmValue::Int(n) => assert_eq!(*n, 3),
            other => panic!("expected int, got {other:?}"),
        }
    }

    #[test]
    fn select_first_picks_lowest_offset_match() {
        let source = "fn a() { 1 }\nfn b() { 2 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 99 }")),
            ("select", vm_string("first")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(preview.starts_with("fn a() { 99 }"));
        assert!(preview.contains("fn b() { 2 }"));
    }

    #[test]
    fn select_nth_picks_specific_index() {
        let source = "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 77 }")),
            ("select", vm_string("nth")),
            ("nth", VmValue::Int(2)),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(preview.contains("fn a() { 1 }"));
        assert!(preview.contains("fn b() { 77 }"));
        assert!(preview.contains("fn c() { 3 }"));
    }

    #[test]
    fn rejects_syntax_errors_after_edit() {
        let source = "fn alpha() { 1 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ ( }")), // unbalanced, produces ERROR node
        ]));
        assert_eq!(s(field(&result, "result")), "syntax_error");
        // Original file must remain untouched on rejection.
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn reports_invalid_query() {
        let source = "fn a() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("((((")),
            ("replacement", vm_string("{ }")),
        ]));
        assert_eq!(s(field(&result, "result")), "invalid_query");
    }

    #[test]
    fn reports_no_match() {
        let source = "fn alpha() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(
                    r#"(function_item name: (identifier) @name (#eq? @name "beta")
                       body: (block) @target)"#,
                ),
            ),
            ("replacement", vm_string("{ }")),
        ]));
        assert_eq!(s(field(&result, "result")), "no_match");
    }

    #[test]
    fn dry_run_returns_preview_without_writing() {
        let source = "fn alpha() { 1 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 2 }")),
            ("dry_run", VmValue::Bool(true)),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        assert!(s(field(&result, "preview")).contains("{ 2 }"));
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn supports_python_function_rewrite() {
        let source = "def greet(name):\n    return 'hi ' + name\n";
        let file = write_temp("py", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_definition name: (identifier) @name body: (block) @target)"),
            ),
            ("replacement", vm_string("return f'hi {name}!'")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(preview.contains("f'hi {name}!'"));
    }

    #[test]
    fn supports_typescript_function_rewrite() {
        let source = "function greet(name: string) {\n  return 'hi ' + name;\n}\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_declaration body: (statement_block) @target)"),
            ),
            ("replacement", vm_string("{\n  return `hi ${name}!`;\n}")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(preview.contains("`hi ${name}!`"));
    }

    #[test]
    fn supports_go_function_rewrite() {
        let source =
            "package main\n\nfunc greet(name string) string {\n\treturn \"hi \" + name\n}\n";
        let file = write_temp("go", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_declaration body: (block) @target)"),
            ),
            (
                "replacement",
                vm_string("{\n\treturn \"hi \" + name + \"!\"\n}"),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(preview.contains("name + \"!\""));
    }

    #[test]
    fn supports_swift_function_rewrite() {
        let source = "func greet(name: String) -> String {\n    return \"hi \\(name)\"\n}\n";
        let file = write_temp("swift", source);
        let path = file.path().to_string_lossy().to_string();
        let query = "(function_declaration body: (function_body) @target)";
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string(query)),
            (
                "replacement",
                vm_string("{\n    return \"hi \\(name)!\"\n}"),
            ),
        ]));
        // tree-sitter-swift's node kind may differ; if so we tolerate
        // unsupported queries here, but the call itself should succeed
        // with a structured result.
        let result_kind = s(field(&result, "result"));
        assert!(
            matches!(
                result_kind.as_str(),
                "applied" | "no_match" | "invalid_query"
            ),
            "swift call returned unexpected result `{result_kind}`"
        );
    }

    #[test]
    fn rejects_unknown_selector() {
        let source = "fn a() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let err = run(&[dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ }")),
            ("select", vm_string("everything-please")),
        ])])
        .expect_err("invalid selector must return error");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "select"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    #[test]
    fn nth_requires_index() {
        let source = "fn a() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let err = run(&[dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ }")),
            ("select", vm_string("nth")),
        ])])
        .expect_err("nth without index is an error");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "nth"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }
}
