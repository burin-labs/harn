//! `ast.insert_at_anchor` — anchor a node via Tree-Sitter query, then
//! insert sibling-or-child text relative to it.
//!
//! Companion to [`super::apply_node`]: where `apply_node` *replaces* a
//! span, this primitive *adds* a sibling or child node. It is the right
//! tool for "append a new test to this mod", "add this import after the
//! last one", or "register a new variant before the trailing brace".
//!
//! ## Wire shape
//!
//! Request fields (see `schemas/ast/insert_at_anchor.request.json`):
//!
//! - `path`: file to mutate.
//! - `query`: Tree-Sitter S-expression query with at least one capture.
//!   The capture named by `target_capture` (default `"anchor"`) names
//!   the anchor; single-capture queries are accepted without rename.
//! - `position`: one of `"before" | "after" | "first_child" | "last_child"`.
//! - `content`: text to insert. Re-indented to the inferred target depth
//!   unless `reindent: false` is supplied.
//! - `indent`: optional indent unit override (e.g. `"  "` or `"\t"`).
//!   When unset, the unit is inferred from the file (per [`detect_indent_unit`]).
//! - `language`: optional language hint (else inferred from extension).
//! - `target_capture`: capture name to treat as the anchor.
//! - `reindent`: when true (default) each line of `content` is prefixed
//!   with the inferred target indentation. Set false to splice the text
//!   verbatim.
//! - `dry_run`: when true the file is left untouched, but the response
//!   carries the `preview` the splice would produce.
//! - `validate`: when true (default) the post-edit source is re-parsed;
//!   tree-sitter ERROR/MISSING aborts the edit.
//! - `session_id`: routes read+write through staged-fs (#1722) so the
//!   insert is atomic alongside siblings.
//! - `max_bytes`: optional read cap (`0` is unlimited, the default).
//!
//! Response is a tagged `result` union: `applied` on success and one of
//! `no_match | ambiguous | invalid_query | invalid_position |
//! invalid_anchor | unsupported_language | syntax_error` on failure.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, QueryError};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};
use crate::tools::permissions::enforce_path_scope;

use super::edit_common::{
    first_syntax_error, format_query_error, lossy_str, read_source, resolve_target_capture,
    sha256_hex, write_source,
};
use super::language::{Language, TEXT_PATCH_FALLBACK};
use super::parse::parse_bytes;

const BUILTIN: &str = "hostlib_ast_insert_at_anchor";
const DEFAULT_TARGET_CAPTURE: &str = "anchor";

/// Insertion position relative to the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// On a new line immediately above the anchor, at the anchor's indent.
    Before,
    /// On a new line immediately below the anchor, at the anchor's indent.
    After,
    /// As the first child of the anchor (just inside the opening delimiter).
    FirstChild,
    /// As the last child of the anchor (just inside the closing delimiter).
    LastChild,
}

impl Position {
    fn parse(raw: &str) -> Result<Self, HostlibError> {
        Ok(match raw {
            "before" => Self::Before,
            "after" => Self::After,
            "first_child" => Self::FirstChild,
            "last_child" => Self::LastChild,
            other => {
                return Err(HostlibError::InvalidParameter {
                    builtin: BUILTIN,
                    param: "position",
                    message: format!(
                        "expected one of [\"before\", \"after\", \"first_child\", \"last_child\"], \
                         got `{other}`"
                    ),
                });
            }
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::FirstChild => "first_child",
            Self::LastChild => "last_child",
        }
    }
}

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let query_text = require_string(BUILTIN, dict, "query")?;
    let position_raw = require_string(BUILTIN, dict, "position")?;
    let position = Position::parse(&position_raw)?;
    let content = require_string(BUILTIN, dict, "content")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let target_capture = optional_string(BUILTIN, dict, "target_capture")?
        .unwrap_or_else(|| DEFAULT_TARGET_CAPTURE.to_string());
    let indent_override = optional_string(BUILTIN, dict, "indent")?;
    let reindent = optional_bool(BUILTIN, dict, "reindent", true)?;
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
    // Guards both the read below and the write-back; one scope check at the
    // strongest access the builtin can perform.
    enforce_path_scope(BUILTIN, &path, FsAccess::Write)?;
    let language = match Language::detect(&path, language_hint.as_deref()) {
        Some(l) => l,
        None => {
            return Ok(unsupported_language_response(
                &path_str,
                language_hint.as_deref(),
            ));
        }
    };

    // A recognized language whose grammar family was not compiled into this
    // build degrades to the same graceful text-fallback response as an
    // unrecognized one.
    let ts_language = match language.ts_language() {
        Some(l) => l,
        None => {
            return Ok(unsupported_language_response(
                &path_str,
                language_hint.as_deref(),
            ));
        }
    };

    let source = read_source(BUILTIN, &path, session_id.as_deref(), max_bytes as usize)?;

    let query = match Query::new(&ts_language, &query_text) {
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
            ));
        }
    };

    let tree = parse_bytes(&source, language).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: err.to_string(),
    })?;

    let anchors = collect_anchors(&query, &tree, &source, target_index);
    if anchors.is_empty() {
        return Ok(no_match_response(
            &path_str,
            &query_text,
            &target_capture,
            "query produced zero anchors",
        ));
    }
    if anchors.len() > 1 {
        return Ok(ambiguous_response(&anchors));
    }

    // Re-borrow the matched node so we can inspect its children — the
    // span we collected only retains byte/row data.
    let anchor_node = locate_node(tree.root_node(), anchors[0].start_byte, anchors[0].end_byte)
        .expect("anchor node must still be reachable in the parse tree");

    let plan = match build_insertion(
        &source,
        anchor_node,
        position,
        language,
        indent_override.as_deref(),
    ) {
        Ok(p) => p,
        Err(invalid) => {
            return Ok(invalid_anchor_response(
                &path_str,
                position,
                &anchors[0],
                &invalid,
            ));
        }
    };

    let body = if reindent {
        reindent_lines(&content, &plan.target_indent)
    } else {
        content
    };
    let insertion_text = format!("{}{body}{}", plan.prefix, plan.suffix);
    let patched = splice_insert(&source, plan.insertion_byte, &insertion_text);

    if validate {
        if let Some(detail) = first_syntax_error(&patched, language) {
            return Ok(syntax_error_response(
                &path_str,
                &lossy_str(&patched),
                &detail,
            ));
        }
    }

    if !dry_run {
        write_source(BUILTIN, &path, &patched, session_id.as_deref())?;
    }

    Ok(applied_response(
        &path_str,
        &source,
        &patched,
        &anchors[0],
        plan.insertion_byte,
        &insertion_text,
        position,
        &plan.target_indent,
        dry_run,
    ))
}

#[derive(Debug, Clone)]
struct AnchorSpan {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    text: String,
}

fn collect_anchors(
    query: &Query,
    tree: &tree_sitter::Tree,
    source_bytes: &[u8],
    target_index: u32,
) -> Vec<AnchorSpan> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
    let mut seen: BTreeMap<(usize, usize), AnchorSpan> = BTreeMap::new();

    while let Some(m) = matches.next() {
        for capture in m.captures {
            if capture.index != target_index {
                continue;
            }
            let node = capture.node;
            let key = (node.start_byte(), node.end_byte());
            seen.entry(key).or_insert_with(|| {
                let start = node.start_position();
                let end = node.end_position();
                let text = std::str::from_utf8(&source_bytes[node.start_byte()..node.end_byte()])
                    .unwrap_or_default()
                    .to_string();
                AnchorSpan {
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    start_row: start.row,
                    start_col: start.column,
                    end_row: end.row,
                    end_col: end.column,
                    text,
                }
            });
        }
    }

    let mut spans: Vec<AnchorSpan> = seen.into_values().collect();
    spans.sort_by_key(|s| s.start_byte);
    spans
}

/// Walk the tree to find the node with exactly the given byte span.
/// We re-locate (rather than store the [`Node`] from the query loop)
/// because tree-sitter's `Node` borrows the parse cursor.
fn locate_node<'a>(root: Node<'a>, start: usize, end: usize) -> Option<Node<'a>> {
    if root.start_byte() == start && root.end_byte() == end {
        return Some(root);
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() == start && node.end_byte() == end {
            return Some(node);
        }
        // Only descend into nodes that could still contain the span.
        if node.start_byte() <= start && node.end_byte() >= end {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    stack.push(child);
                }
            }
        }
    }
    None
}

/// Plan for a single insertion: where to put it, what to wrap it with,
/// and the indent to apply to the user's content.
#[derive(Debug)]
struct InsertionPlan {
    insertion_byte: usize,
    target_indent: String,
    prefix: String,
    suffix: String,
}

fn build_insertion(
    source: &[u8],
    anchor: Node<'_>,
    position: Position,
    language: Language,
    indent_override: Option<&str>,
) -> Result<InsertionPlan, String> {
    let bytes = source;
    let anchor_indent = line_indent(bytes, anchor.start_byte());

    match position {
        Position::Before => {
            // Splice in front of the anchor, at the same depth. The
            // anchor's own leading indent on the line stays put.
            Ok(InsertionPlan {
                insertion_byte: anchor.start_byte(),
                target_indent: anchor_indent.clone(),
                prefix: String::new(),
                suffix: format!("\n{anchor_indent}"),
            })
        }
        Position::After => {
            // Splice immediately after the anchor's last byte. The
            // anchor's trailing newline (if any) keeps the new line
            // self-contained.
            Ok(InsertionPlan {
                insertion_byte: anchor.end_byte(),
                target_indent: anchor_indent.clone(),
                prefix: format!("\n{anchor_indent}"),
                suffix: String::new(),
            })
        }
        Position::FirstChild | Position::LastChild => {
            if anchor.child_count() == 0 {
                return Err(format!(
                    "anchor `{}` has no children; cannot insert as {}",
                    anchor.kind(),
                    position.as_str(),
                ));
            }
            let unit = indent_unit(source, language, indent_override);
            let child_indent = first_named_child_indent(bytes, anchor)
                .unwrap_or_else(|| anchor_indent.clone() + &unit);
            match position {
                Position::FirstChild => first_child_plan(bytes, anchor, child_indent),
                Position::LastChild => last_child_plan(bytes, anchor, child_indent, anchor_indent),
                _ => unreachable!(),
            }
        }
    }
}

fn first_child_plan(
    bytes: &[u8],
    anchor: Node<'_>,
    child_indent: String,
) -> Result<InsertionPlan, String> {
    // Prefer placing the insertion right before the first existing
    // named child so it lands as a true sibling of the others.
    if let Some(first_named) = first_named_child(anchor) {
        // Re-emit the named child's existing leading indent so it
        // stays right-aligned after the splice.
        let suffix = format!("\n{}", line_indent(bytes, first_named.start_byte()));
        return Ok(InsertionPlan {
            insertion_byte: first_named.start_byte(),
            target_indent: child_indent,
            prefix: String::new(),
            suffix,
        });
    }
    // No named children — fall back to inserting just inside the
    // opening delimiter (the anchor's first child, named or not).
    let opener = anchor.child(0).ok_or_else(|| {
        format!(
            "anchor `{}` has no children to anchor against",
            anchor.kind()
        )
    })?;
    let prefix = format!("\n{child_indent}");
    Ok(InsertionPlan {
        insertion_byte: opener.end_byte(),
        target_indent: child_indent,
        prefix,
        suffix: String::new(),
    })
}

fn last_child_plan(
    bytes: &[u8],
    anchor: Node<'_>,
    child_indent: String,
    anchor_indent: String,
) -> Result<InsertionPlan, String> {
    if let Some(last_named) = last_named_child(anchor) {
        let prefix = format!("\n{child_indent}");
        return Ok(InsertionPlan {
            insertion_byte: last_named.end_byte(),
            target_indent: child_indent,
            prefix,
            suffix: String::new(),
        });
    }
    // Empty container — insert just before the closing delimiter (the
    // anchor's last child) and re-emit the anchor's own indent for the
    // delimiter line.
    let closer = anchor
        .child((anchor.child_count() - 1) as u32)
        .ok_or_else(|| {
            format!(
                "anchor `{}` has no children to anchor against",
                anchor.kind()
            )
        })?;
    // Walk back over the closer's leading whitespace on its own line
    // so we splice in *before* that indent.
    let closer_line_start = closer.start_byte() - line_indent(bytes, closer.start_byte()).len();
    let prefix = format!("\n{child_indent}");
    Ok(InsertionPlan {
        insertion_byte: closer_line_start,
        target_indent: child_indent,
        prefix,
        suffix: format!("\n{anchor_indent}"),
    })
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.is_named() {
                return Some(child);
            }
        }
    }
    None
}

fn last_named_child(node: Node<'_>) -> Option<Node<'_>> {
    for i in (0..node.child_count()).rev() {
        if let Some(child) = node.child(i as u32) {
            if child.is_named() {
                return Some(child);
            }
        }
    }
    None
}

fn first_named_child_indent(bytes: &[u8], anchor: Node<'_>) -> Option<String> {
    let child = first_named_child(anchor)?;
    Some(line_indent(bytes, child.start_byte()))
}

/// Return the run of [` `, `\t`] characters immediately preceding `byte`
/// on the same line. Returns an empty string if `byte` is at column 0.
fn line_indent(bytes: &[u8], byte: usize) -> String {
    let upper = byte.min(bytes.len());
    let mut start = upper;
    while start > 0 {
        let prev = bytes[start - 1];
        if prev == b' ' || prev == b'\t' {
            start -= 1;
            continue;
        }
        break;
    }
    // Walk back only as far as the start of the line so we don't pick
    // up indented previous-line content if there are no newlines yet.
    let line_start = bytes[..upper]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let begin = start.max(line_start);
    std::str::from_utf8(&bytes[begin..upper])
        .unwrap_or("")
        .to_string()
}

/// Detect the file's prevailing indent unit so child insertions feel
/// native. Override > language default > heuristic scan.
fn indent_unit(source: &[u8], language: Language, override_unit: Option<&str>) -> String {
    if let Some(unit) = override_unit {
        if !unit.is_empty() {
            return unit.to_string();
        }
    }
    // Indent detection is a display/formatting heuristic, so a lossy view
    // of the raw bytes is fine here — it never feeds back into byte offsets
    // or the write path.
    if let Some(unit) = scan_indent_unit(&lossy_str(source)) {
        return unit;
    }
    language_default_indent(language).to_string()
}

/// Walk the source looking for the smallest non-zero leading-whitespace
/// run on a non-blank line. Tab indentation wins outright when any line
/// starts with a tab (consistent with most editors' "indent type"
/// detection). Returns `None` if no indented lines exist.
fn scan_indent_unit(source: &str) -> Option<String> {
    let mut tab_lines = 0usize;
    let mut min_spaces = usize::MAX;
    for line in source.lines() {
        let mut iter = line.bytes();
        match iter.next() {
            Some(b'\t') => tab_lines += 1,
            Some(b' ') => {
                let spaces = 1 + iter.take_while(|&b| b == b' ').count();
                if line.len() == spaces {
                    // Blank-but-indented line; ignore.
                    continue;
                }
                if spaces < min_spaces {
                    min_spaces = spaces;
                }
            }
            _ => {}
        }
    }
    if tab_lines > 0 {
        return Some("\t".to_string());
    }
    if min_spaces == usize::MAX {
        return None;
    }
    Some(" ".repeat(min_spaces))
}

fn language_default_indent(language: Language) -> &'static str {
    match language {
        Language::Go => "\t",
        Language::JavaScript | Language::Jsx | Language::TypeScript | Language::Tsx => "  ",
        _ => "    ",
    }
}

/// Re-indent each line of `body` so subsequent lines share the target's
/// indentation. The first line is left as-is so the splice can be
/// joined cleanly to whatever prefix the [`InsertionPlan`] supplies.
fn reindent_lines(body: &str, indent: &str) -> String {
    if indent.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len() + indent.len() * 2);
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
    }
    out
}

/// Insert `text` at byte offset `byte` in the raw `source` bytes. Operates
/// on bytes so any non-UTF-8 bytes elsewhere in the file pass through the
/// insertion untouched (`byte` is a tree-sitter offset over these same raw
/// bytes via [`parse_bytes`]).
fn splice_insert(source: &[u8], byte: usize, text: &str) -> Vec<u8> {
    let cut = byte.min(source.len());
    let mut out = Vec::with_capacity(source.len() + text.len());
    out.extend_from_slice(&source[..cut]);
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(&source[cut..]);
    out
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn applied_response(
    path: &str,
    before: &[u8],
    after: &[u8],
    anchor: &AnchorSpan,
    insertion_byte: usize,
    inserted_text: &str,
    position: Position,
    target_indent: &str,
    dry_run: bool,
) -> VmValue {
    build_dict([
        ("result", str_value("applied")),
        ("applied", VmValue::Bool(true)),
        ("path", str_value(path)),
        ("dry_run", VmValue::Bool(dry_run)),
        ("position", str_value(position.as_str())),
        ("insertion_byte", VmValue::Int(insertion_byte as i64)),
        ("indent", str_value(target_indent)),
        ("inserted_text", str_value(inserted_text)),
        ("anchor", anchor_to_value(anchor)),
        ("before_sha256", str_value(sha256_hex(before))),
        ("after_sha256", str_value(sha256_hex(after))),
        ("preview", str_value(lossy_str(after))),
    ])
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

fn ambiguous_response(anchors: &[AnchorSpan]) -> VmValue {
    build_dict([
        ("result", str_value("ambiguous")),
        ("applied", VmValue::Bool(false)),
        ("match_count", VmValue::Int(anchors.len() as i64)),
        (
            "anchors",
            VmValue::List(Arc::new(anchors.iter().map(anchor_to_value).collect())),
        ),
        (
            "details",
            str_value(format!(
                "anchor query must match exactly one node, found {}; \
                 tighten the query (e.g. add a `(#eq? @name \"...\")` predicate) \
                 to disambiguate",
                anchors.len()
            )),
        ),
    ])
}

fn invalid_query_response(query: &str, err: &QueryError) -> VmValue {
    build_dict([
        ("result", str_value("invalid_query")),
        ("applied", VmValue::Bool(false)),
        ("query", str_value(query)),
        ("details", str_value(format_query_error(err))),
        ("error_row", VmValue::Int(err.row as i64)),
        ("error_column", VmValue::Int(err.column as i64)),
    ])
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
        ("fallback_suggestion", str_value(TEXT_PATCH_FALLBACK)),
    ])
}

fn invalid_anchor_response(
    path: &str,
    position: Position,
    anchor: &AnchorSpan,
    detail: &str,
) -> VmValue {
    build_dict([
        ("result", str_value("invalid_anchor")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("position", str_value(position.as_str())),
        ("anchor", anchor_to_value(anchor)),
        ("details", str_value(detail)),
    ])
}

fn syntax_error_response(path: &str, after: &str, detail: &str) -> VmValue {
    build_dict([
        ("result", str_value("syntax_error")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("details", str_value(detail)),
        ("preview", str_value(after)),
    ])
}

fn anchor_to_value(span: &AnchorSpan) -> VmValue {
    build_dict([
        ("start_byte", VmValue::Int(span.start_byte as i64)),
        ("end_byte", VmValue::Int(span.end_byte as i64)),
        ("start_row", VmValue::Int(span.start_row as i64)),
        ("start_col", VmValue::Int(span.start_col as i64)),
        ("end_row", VmValue::Int(span.end_row as i64)),
        ("end_col", VmValue::Int(span.end_col as i64)),
        ("text", str_value(&span.text)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert((*k).to_string(), v.clone());
        }
        VmValue::dict(map)
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
        run(&[payload]).expect("insert_at_anchor runs")
    }

    #[test]
    fn after_inserts_sibling_function_at_anchor_depth() {
        let source = "fn alpha() {\n    1\n}\n\nfn gamma() {\n    3\n}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(
                    "(function_item name: (identifier) @name (#eq? @name \"alpha\")) @anchor",
                ),
            ),
            ("position", vm_string("after")),
            ("content", vm_string("fn beta() {\n    2\n}")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(
            preview.contains("fn alpha()")
                && preview.contains("fn beta()")
                && preview.contains("fn gamma()"),
            "all three functions must be present:\n{preview}"
        );
        let alpha = preview.find("fn alpha()").unwrap();
        let beta = preview.find("fn beta()").unwrap();
        let gamma = preview.find("fn gamma()").unwrap();
        assert!(
            alpha < beta && beta < gamma,
            "beta must land between alpha and gamma:\n{preview}"
        );
    }

    fn write_temp_bytes(extension: &str, source: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .expect("temp file");
        file.write_all(source).expect("write source");
        file
    }

    #[test]
    fn preserves_invalid_utf8_bytes_outside_insertion() {
        // An invalid byte (0x80) lives in a comment far from the insertion
        // anchor. Reading the file lossily and writing the decoded buffer
        // back would rewrite 0x80 to U+FFFD even though the insert never
        // touches that region.
        let mut source: Vec<u8> = b"// header byte: ".to_vec();
        source.push(0x80);
        source.extend_from_slice(b"\nfn alpha() {\n    1\n}\n");
        assert!(
            std::str::from_utf8(&source).is_err(),
            "fixture must be non-UTF8"
        );

        let file = write_temp_bytes("rs", &source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(
                    "(function_item name: (identifier) @name (#eq? @name \"alpha\")) @anchor",
                ),
            ),
            ("position", vm_string("after")),
            ("content", vm_string("fn beta() {\n    2\n}")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");

        let on_disk = std::fs::read(file.path()).expect("read raw bytes");
        assert!(
            String::from_utf8_lossy(&on_disk).contains("fn beta()"),
            "insert should apply"
        );
        assert!(
            on_disk.contains(&0x80),
            "invalid byte must be preserved, not rewritten to U+FFFD"
        );
        assert!(
            !on_disk.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
            "no U+FFFD replacement bytes should be introduced"
        );
    }

    #[test]
    fn before_inserts_sibling_import_above_anchor() {
        let source = "import { a } from \"./a\";\nimport { b } from \"./b\";\n\nconst x = 1;\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(
                    "(import_statement source: (string (string_fragment) @src) (#eq? @src \"./b\")) @anchor",
                ),
            ),
            ("position", vm_string("before")),
            ("content", vm_string("import { aa } from \"./aa\";")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        let aa_pos = preview.find("./aa").expect("./aa present");
        let b_pos = preview.find("./b\"").expect("./b present");
        assert!(aa_pos < b_pos, "aa must land before b:\n{preview}");
    }

    #[test]
    fn last_child_appends_test_to_rust_mod() {
        let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(mod_item name: (identifier) @name (#eq? @name \"tests\") body: (declaration_list) @anchor)"),
            ),
            ("position", vm_string("last_child")),
            (
                "content",
                vm_string("#[test]\nfn two() {}"),
            ),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        let one = preview.find("fn one()").unwrap();
        let two = preview.find("fn two()").unwrap();
        assert!(one < two, "two must follow one inside the mod:\n{preview}");
        // The new function should be indented to the mod's body depth.
        assert!(
            preview.contains("    #[test]\n    fn two() {}"),
            "expected 4-space indented test, got:\n{preview}"
        );
    }

    #[test]
    fn first_child_prepends_to_empty_typescript_block() {
        let source = "function noop() {}\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_declaration body: (statement_block) @anchor)"),
            ),
            ("position", vm_string("first_child")),
            ("content", vm_string("return 1;")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        let preview = s(field(&result, "preview"));
        assert!(
            preview.contains("return 1;"),
            "preview missing body:\n{preview}"
        );
    }

    #[test]
    fn ambiguous_anchor_rejects_multi_match() {
        let source = "fn a() { 1 }\nfn b() { 2 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item) @anchor")),
            ("position", vm_string("after")),
            ("content", vm_string("fn c() {}")),
        ]));
        assert_eq!(s(field(&result, "result")), "ambiguous");
        match field(&result, "match_count") {
            VmValue::Int(n) => assert_eq!(*n, 2),
            other => panic!("expected match_count int, got {other:?}"),
        }
    }

    #[test]
    fn syntax_error_is_rejected_and_file_untouched() {
        let source = "fn one() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_item name: (identifier) @name (#eq? @name \"one\")) @anchor"),
            ),
            ("position", vm_string("after")),
            // Missing closing brace -> tree-sitter ERROR.
            ("content", vm_string("fn broken( {")),
        ]));
        assert_eq!(s(field(&result, "result")), "syntax_error");
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn dry_run_returns_preview_without_writing() {
        let source = "fn alpha() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(
                    "(function_item name: (identifier) @name (#eq? @name \"alpha\")) @anchor",
                ),
            ),
            ("position", vm_string("after")),
            ("content", vm_string("fn beta() {}")),
            ("dry_run", VmValue::Bool(true)),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");
        assert!(s(field(&result, "preview")).contains("fn beta()"));
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn invalid_position_returns_structured_error() {
        let source = "fn one() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let err = run(&[dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item) @anchor")),
            ("position", vm_string("middle")),
            ("content", vm_string("fn x() {}")),
        ])])
        .expect_err("unknown position must error");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "position"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    #[test]
    fn invalid_query_returns_invalid_query() {
        let source = "fn one() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("((((")),
            ("position", vm_string("after")),
            ("content", vm_string("fn x() {}")),
        ]));
        assert_eq!(s(field(&result, "result")), "invalid_query");
    }

    #[test]
    fn no_match_when_query_misses() {
        let source = "fn alpha() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(function_item name: (identifier) @name (#eq? @name \"beta\")) @anchor"),
            ),
            ("position", vm_string("after")),
            ("content", vm_string("fn beta() {}")),
        ]));
        assert_eq!(s(field(&result, "result")), "no_match");
    }

    #[test]
    fn invalid_anchor_when_first_child_requested_on_leaf() {
        // `(identifier)` is a leaf with no children; first_child must
        // surface a structured failure rather than panicking.
        let source = "let x = 1;\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            (
                "query",
                vm_string("(variable_declarator name: (identifier) @anchor (#eq? @anchor \"x\"))"),
            ),
            ("position", vm_string("first_child")),
            ("content", vm_string("// noop")),
        ]));
        assert_eq!(s(field(&result, "result")), "invalid_anchor");
    }

    #[test]
    fn reindent_lines_preserves_first_line_and_indents_rest() {
        let body = "fn a() {\n    1\n}";
        let indented = reindent_lines(body, "    ");
        assert_eq!(indented, "fn a() {\n        1\n    }");
    }

    #[test]
    fn scan_indent_unit_prefers_tabs_when_present() {
        let src = "fn a() {\n\treturn 1;\n}\n";
        assert_eq!(scan_indent_unit(src), Some("\t".to_string()));
    }

    #[test]
    fn scan_indent_unit_finds_smallest_space_run() {
        let src = "fn a() {\n  return 1;\n}\n";
        assert_eq!(scan_indent_unit(src), Some("  ".to_string()));
    }

    #[test]
    fn unsupported_language_when_extension_unknown() {
        let file = write_temp("xyz", "noop\n");
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(any) @anchor")),
            ("position", vm_string("after")),
            ("content", vm_string("noop")),
        ]));
        assert_eq!(s(field(&result, "result")), "unsupported_language");
    }
}
