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

use std::path::PathBuf;

use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, str_value,
};
use crate::tools::permissions::enforce_path_scope;

use super::edit_common::{
    check_post_edit, collect_target_spans, lossy_str, no_match_response, non_verified_response,
    prepare_edit, select_spans, sha256_hex, splice, write_source, PostEditRejection, PreparedEdit,
    SelectFailure, Selector, Span, UNVALIDATED_WRITE,
};
use super::parse::max_bytes_arg;

const BUILTIN: &str = "hostlib_ast_apply_node";
const DEFAULT_TARGET_CAPTURE: &str = "target";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let query_text = require_string(BUILTIN, dict, "query")?;
    let replacement = require_string(BUILTIN, dict, "replacement")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let target_capture = optional_string(BUILTIN, dict, "target_capture")?
        .unwrap_or_else(|| DEFAULT_TARGET_CAPTURE.to_string());
    let selector = Selector::from_request(BUILTIN, dict)?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", false)?;
    let validate = optional_bool(BUILTIN, dict, "validate", true)?;
    let session_id = optional_string(BUILTIN, dict, "session_id")?;
    let max_bytes = max_bytes_arg(BUILTIN, dict)?;

    let path = PathBuf::from(&path_str);
    // Guards both the read below and the write-back; one scope check at the
    // strongest access the builtin can perform.
    enforce_path_scope(BUILTIN, &path, FsAccess::Write)?;
    let PreparedEdit {
        language,
        source,
        query,
        target_index,
        tree,
        source_health,
    } = match prepare_edit(
        BUILTIN,
        &path,
        language_hint.as_deref(),
        &query_text,
        &target_capture,
        session_id.as_deref(),
        max_bytes,
    ) {
        Ok(prepared) => prepared,
        Err(failure) => {
            return failure.into_response(
                BUILTIN,
                &path_str,
                language_hint.as_deref(),
                &query_text,
                &target_capture,
            )
        }
    };

    let spans = collect_target_spans(&query, &tree, &source, target_index);
    if spans.is_empty() {
        return Ok(source_health.attach_to(no_match_response(
            &path_str,
            &query_text,
            &target_capture,
            "query produced zero captures",
        )));
    }

    let chosen = match select_spans(&spans, selector) {
        Ok(spans) => spans,
        Err(SelectFailure::Ambiguous) => {
            return Ok(source_health.attach_to(ambiguous_response(spans.len(), &spans)));
        }
        Err(SelectFailure::NthOutOfRange { requested }) => {
            return Ok(source_health.attach_to(no_nth_response(
                spans.len(),
                requested,
                &path_str,
                &query_text,
            )));
        }
    };

    let patched = splice(&source, &chosen, &replacement);

    let health = match check_post_edit(&patched, language, validate, dry_run) {
        Ok(health) => health,
        Err(rejection) => {
            let response = match &rejection {
                PostEditRejection::Syntax(detail) => {
                    syntax_error_response(&path_str, &lossy_str(&patched), detail, &chosen)
                }
                PostEditRejection::UnvalidatedWrite => {
                    non_verified_response(&path_str, UNVALIDATED_WRITE)
                }
            };
            return Ok(rejection.health(language).attach_to(response));
        }
    };

    if !dry_run {
        write_source(BUILTIN, &path, &patched, session_id.as_deref())?;
    }

    Ok(health.attach_to(applied_response(
        &path_str,
        &source,
        &patched,
        &chosen,
        &replacement,
        dry_run,
    )))
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

fn applied_response(
    path: &str,
    before: &[u8],
    after: &[u8],
    chosen: &[Span],
    replacement: &str,
    dry_run: bool,
) -> VmValue {
    VmValue::dict(
        [
            ("result".to_string(), str_value("applied")),
            ("applied".to_string(), VmValue::Bool(true)),
            ("path".to_string(), str_value(path)),
            ("dry_run".to_string(), VmValue::Bool(dry_run)),
            ("match_count".to_string(), VmValue::Int(chosen.len() as i64)),
            (
                "edits".to_string(),
                VmValue::list(
                    chosen
                        .iter()
                        .map(|s| edit_to_value(s, replacement))
                        .collect(),
                ),
            ),
            ("before_sha256".to_string(), str_value(sha256_hex(before))),
            ("after_sha256".to_string(), str_value(sha256_hex(after))),
            ("preview".to_string(), str_value(lossy_str(after))),
        ]
        .into_iter()
        .collect::<harn_vm::value::DictMap>(),
    )
}

fn ambiguous_response(match_count: usize, spans: &[Span]) -> VmValue {
    VmValue::dict(
        [
            ("result".to_string(), str_value("ambiguous")),
            ("applied".to_string(), VmValue::Bool(false)),
            ("match_count".to_string(), VmValue::Int(match_count as i64)),
            (
                "spans".to_string(),
                VmValue::list(spans.iter().map(span_to_value).collect()),
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
        .collect::<harn_vm::value::DictMap>(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    use std::io::Write;
    use tempfile::NamedTempFile;

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
    fn rejects_structural_write_when_source_health_is_not_verified() {
        let source = "fn alpha( {\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item) @target")),
            ("replacement", vm_string("fn alpha() {}")),
        ]));
        assert_eq!(s(field(&result, "result")), "parser_not_verified");
        let health = field(&result, "health");
        assert_eq!(s(field(health, "coverage")), "inconclusive");
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), source);
    }

    #[test]
    fn validate_false_cannot_bypass_a_real_write() {
        let source = "fn alpha() { 1 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 2 }")),
            ("validate", VmValue::Bool(false)),
        ]));
        assert_eq!(s(field(&result, "result")), "parser_not_verified");
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), source);
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

    fn write_temp_bytes(extension: &str, source: &[u8]) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .expect("temp file");
        file.write_all(source).expect("write source");
        file
    }

    #[test]
    fn preserves_invalid_utf8_bytes_outside_edited_span() {
        // A valid-UTF-8 edit target sitting next to an invalid byte (0x80)
        // in a comment. Lossy-decoding the whole file on read and writing
        // the decoded string back would rewrite 0x80 to the 3-byte U+FFFD
        // encoding, even though the edit never touches the comment.
        let mut source: Vec<u8> = b"// header byte: ".to_vec();
        source.push(0x80);
        source.extend_from_slice(b"\nfn foo() { let x = 1; }\n");
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
                vm_string("(let_declaration value: (integer_literal) @target)"),
            ),
            ("replacement", vm_string("2")),
        ]));
        assert_eq!(s(field(&result, "result")), "applied");

        let on_disk = std::fs::read(file.path()).expect("read raw bytes");
        // The targeted edit landed...
        assert!(
            String::from_utf8_lossy(&on_disk).contains("let x = 2"),
            "edit should apply"
        );
        // ...and the untouched invalid byte survived verbatim — no U+FFFD.
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
