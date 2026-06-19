//! `ast.batch_apply` — multi-file, query-driven codemod runner.
//!
//! Runs one Tree-Sitter `query` → `replacement` over a list of `paths`,
//! returning a per-file preview plus a roll-up summary. It is the
//! multi-file sibling of [`super::apply_node`]: each file goes through the
//! same `collect_target_spans` → `select_spans` → `splice` → validate
//! pipeline (shared via [`super::edit_common`]), so the format-preserving
//! and post-edit-validation guarantees are identical.
//!
//! ## Dry-run is the default
//!
//! `dry_run` defaults to **true**: a plain call previews edits across every
//! file without touching disk. Writing requires an explicit `dry_run:
//! false`. This makes "see what this codemod would do" the zero-argument
//! path and "actually apply it" the deliberate one.
//!
//! ## Per-file isolation
//!
//! A failure on one file (unparseable, no match, a query that is invalid
//! for that grammar, a post-edit syntax error) is recorded as that file's
//! `result` and does not abort the batch — the remaining files still run.
//! Path-scope violations are the exception: every path is scope-checked up
//! front, so an out-of-scope file aborts the whole call *before* anything
//! is written (no partial application).
//!
//! ## Wire shape
//!
//! Request (see `schemas/ast/batch_apply.request.json`):
//!
//! - `paths`: non-empty list of files to run the codemod over.
//! - `query`: Tree-Sitter query with at least one capture.
//! - `replacement` (alias `fix`): text spliced in for every selected node.
//! - `select`: multi-match policy `"unique" | "first" | "all" | "nth"`
//!   (default `"unique"`), with `nth` for the `"nth"` mode.
//! - `target_capture`: capture treated as the replaceable span (default
//!   `"target"`; single-capture queries use their only capture).
//! - `language`: optional grammar hint applied to every path (otherwise
//!   inferred per file from its extension).
//! - `dry_run` (default `true`), `validate` (default `true`),
//!   `max_bytes`, `session_id` — as in `apply_node`.
//!
//! Response: `{ result: "ok", dry_run, summary, files: [...] }` where each
//! file entry carries `{ path, result, match_count, before_sha256,
//! after_sha256, changed, preview, details? }`.

use std::path::PathBuf;
use std::sync::Arc;

use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;
use tree_sitter::Query;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, optional_string_list,
    require_string, str_value,
};
use crate::tools::permissions::enforce_path_scope;

use super::edit_common::{
    collect_target_spans, first_syntax_error, format_query_error, lossy_str, read_source,
    resolve_target_capture, select_spans, sha256_hex, splice, write_source, SelectFailure,
    Selector,
};
use super::language::Language;
use super::parse::parse_bytes;

const BUILTIN: &str = "hostlib_ast_batch_apply";
const DEFAULT_TARGET_CAPTURE: &str = "target";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    // `paths` is required and must be non-empty. An empty/missing fileset is
    // a caller bug, so it surfaces as `MissingParameter` (matching the
    // registration contract every AST builtin honors).
    let raw_paths = optional_string_list(BUILTIN, dict, "paths")?;
    if raw_paths.is_empty() {
        return Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "paths",
        });
    }
    // De-duplicate while preserving order so a path listed twice is not
    // applied twice (the second pass would re-read the just-written file).
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<String> = raw_paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect();

    let query_text = require_string(BUILTIN, dict, "query")?;
    let replacement = resolve_replacement(dict)?;
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
    let selector = Selector::parse(BUILTIN, select_raw.as_deref(), nth_raw)?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", true)?;
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

    // Scope-check every path up front at the access the call will actually
    // perform (read-only in dry-run, write otherwise). Doing this before any
    // write means an out-of-scope file aborts the batch with nothing
    // half-applied.
    let access = if dry_run {
        FsAccess::Read
    } else {
        FsAccess::Write
    };
    for path in &paths {
        enforce_path_scope(BUILTIN, &PathBuf::from(path), access)?;
    }

    let cfg = FileJob {
        query_text: &query_text,
        replacement: &replacement,
        language_hint: language_hint.as_deref(),
        target_capture: &target_capture,
        selector,
        validate,
        dry_run,
        max_bytes: max_bytes as usize,
        session_id: session_id.as_deref(),
    };

    let mut file_values = Vec::with_capacity(paths.len());
    let mut summary = Summary::default();
    for path in &paths {
        let outcome = apply_one(path, &cfg)?;
        summary.tally(&outcome);
        file_values.push(outcome.into_value(path));
    }
    summary.finalize_writes(dry_run);

    Ok(build_dict([
        ("result", str_value("ok")),
        ("dry_run", VmValue::Bool(dry_run)),
        ("summary", summary.into_value()),
        ("files", VmValue::List(Arc::new(file_values))),
    ]))
}

/// Accept the replacement under either `replacement` or its `fix` alias
/// (the rule model, Epic A, will speak `fix`; the raw primitive accepts
/// both today). Exactly one must be present.
fn resolve_replacement(dict: &harn_vm::value::DictMap) -> Result<String, HostlibError> {
    let replacement = optional_string(BUILTIN, dict, "replacement")?;
    let fix = optional_string(BUILTIN, dict, "fix")?;
    match (replacement, fix) {
        (Some(_), Some(_)) => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "replacement",
            message: "pass exactly one of `replacement` or `fix`, not both".into(),
        }),
        (Some(value), None) | (None, Some(value)) => Ok(value),
        (None, None) => Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "replacement",
        }),
    }
}

/// Shared per-call configuration threaded into each file's run.
struct FileJob<'a> {
    query_text: &'a str,
    replacement: &'a str,
    language_hint: Option<&'a str>,
    target_capture: &'a str,
    selector: Selector,
    validate: bool,
    dry_run: bool,
    max_bytes: usize,
    session_id: Option<&'a str>,
}

/// The result of running the codemod against a single file. Mirrors
/// `apply_node`'s tagged `result` vocabulary, plus `read_error` /
/// `parse_error` for the failure modes only a multi-file runner has to
/// recover from rather than abort on.
struct FileOutcome {
    result: &'static str,
    match_count: usize,
    before_sha: Option<String>,
    after_sha: Option<String>,
    changed: bool,
    preview: Option<String>,
    details: Option<String>,
}

impl FileOutcome {
    fn failure(result: &'static str, details: impl Into<String>) -> Self {
        FileOutcome {
            result,
            match_count: 0,
            before_sha: None,
            after_sha: None,
            changed: false,
            preview: None,
            details: Some(details.into()),
        }
    }

    /// True for any result that prevented the codemod from applying.
    fn is_failure(&self) -> bool {
        !matches!(self.result, "applied" | "no_match")
    }

    fn into_value(self, path: &str) -> VmValue {
        let mut entries: Vec<(&'static str, VmValue)> = vec![
            ("path", str_value(path)),
            ("result", str_value(self.result)),
            ("match_count", VmValue::Int(self.match_count as i64)),
            ("changed", VmValue::Bool(self.changed)),
        ];
        if let Some(before) = self.before_sha {
            entries.push(("before_sha256", str_value(before)));
        }
        if let Some(after) = self.after_sha {
            entries.push(("after_sha256", str_value(after)));
        }
        if let Some(preview) = self.preview {
            entries.push(("preview", str_value(preview)));
        }
        if let Some(details) = self.details {
            entries.push(("details", str_value(details)));
        }
        build_dict(entries)
    }
}

/// Run the codemod against one file. Returns `Err` only for genuinely
/// fatal host errors (a write that fails mid-batch); every *edit* outcome —
/// including failures — is encoded in the returned [`FileOutcome`] so the
/// batch keeps going.
fn apply_one(path_str: &str, cfg: &FileJob<'_>) -> Result<FileOutcome, HostlibError> {
    let path = PathBuf::from(path_str);

    let language = match Language::detect(&path, cfg.language_hint) {
        Some(l) => l,
        None => {
            return Ok(FileOutcome::failure(
                "unsupported_language",
                format!(
                    "could not infer a tree-sitter grammar for `{path_str}` (hint: {})",
                    cfg.language_hint.unwrap_or("none")
                ),
            ))
        }
    };
    let ts_language = match language.ts_language() {
        Some(l) => l,
        None => {
            return Ok(FileOutcome::failure(
                "unsupported_language",
                format!(
                    "grammar for `{}` is not compiled into this build",
                    language.name()
                ),
            ))
        }
    };

    let source = match read_source(BUILTIN, &path, cfg.session_id, cfg.max_bytes) {
        Ok(source) => source,
        Err(err) => return Ok(FileOutcome::failure("read_error", err.to_string())),
    };

    let query = match Query::new(&ts_language, cfg.query_text) {
        Ok(q) => q,
        Err(err) => {
            return Ok(FileOutcome::failure(
                "invalid_query",
                format_query_error(&err),
            ))
        }
    };

    let target_index = match resolve_target_capture(&query, cfg.target_capture) {
        Ok(idx) => idx,
        Err(detail) => return Ok(FileOutcome::failure("no_match", detail)),
    };

    let tree = match parse_bytes(&source, language) {
        Ok(tree) => tree,
        Err(err) => return Ok(FileOutcome::failure("parse_error", err.to_string())),
    };

    let spans = collect_target_spans(&query, &tree, &source, target_index);
    if spans.is_empty() {
        return Ok(unchanged(&source, "no_match", 0, None));
    }

    let chosen = match select_spans(&spans, cfg.selector) {
        Ok(chosen) => chosen,
        Err(SelectFailure::Ambiguous) => {
            return Ok(unchanged(
                &source,
                "ambiguous",
                spans.len(),
                Some(format!(
                    "`select: \"unique\"` requires a single match, found {}; \
                     use `\"first\" | \"all\" | \"nth\"` to disambiguate",
                    spans.len()
                )),
            ))
        }
        Err(SelectFailure::NthOutOfRange { requested }) => {
            return Ok(unchanged(
                &source,
                "no_match",
                spans.len(),
                Some(format!(
                    "`select: \"nth\"` requested index {requested}, only {} match(es) found",
                    spans.len()
                )),
            ))
        }
    };

    let patched = splice(&source, &chosen, cfg.replacement);

    if cfg.validate {
        if let Some(detail) = first_syntax_error(&patched, language) {
            return Ok(FileOutcome {
                result: "syntax_error",
                match_count: chosen.len(),
                before_sha: Some(sha256_hex(&source)),
                after_sha: Some(sha256_hex(&patched)),
                changed: false,
                preview: Some(lossy_str(&patched)),
                details: Some(detail),
            });
        }
    }

    let changed = patched != source;
    // Only touch disk when applying *and* the splice actually altered the
    // file. A no-op edit (replacement equals the original span) is what
    // makes a second run of an applied codemod report zero changes.
    if !cfg.dry_run && changed {
        write_source(BUILTIN, &path, &patched, cfg.session_id)?;
    }

    Ok(FileOutcome {
        result: "applied",
        match_count: chosen.len(),
        before_sha: Some(sha256_hex(&source)),
        after_sha: Some(sha256_hex(&patched)),
        changed,
        preview: Some(lossy_str(&patched)),
        details: None,
    })
}

/// A file whose source was read but left untouched (no match / ambiguous).
/// Equal `before`/`after` fingerprints let callers detect "no change"
/// uniformly. There is deliberately no `preview`: the file is unchanged,
/// so echoing its full source back for every non-matching file would bloat
/// a large batch's response for nothing.
fn unchanged(
    source: &[u8],
    result: &'static str,
    match_count: usize,
    details: Option<String>,
) -> FileOutcome {
    let sha = sha256_hex(source);
    FileOutcome {
        result,
        match_count,
        before_sha: Some(sha.clone()),
        after_sha: Some(sha),
        changed: false,
        preview: None,
        details,
    }
}

#[derive(Default)]
struct Summary {
    files_total: usize,
    files_matched: usize,
    files_changed: usize,
    files_written: usize,
    files_failed: usize,
    match_count_total: usize,
}

impl Summary {
    fn tally(&mut self, outcome: &FileOutcome) {
        self.files_total += 1;
        self.match_count_total += outcome.match_count;
        if outcome.match_count > 0 {
            self.files_matched += 1;
        }
        if outcome.changed {
            self.files_changed += 1;
        }
        if outcome.is_failure() {
            self.files_failed += 1;
        }
    }

    /// Record how many of the changed files were actually persisted. In
    /// dry-run nothing is written, so this stays zero regardless of how many
    /// files *would* change.
    fn finalize_writes(&mut self, dry_run: bool) {
        self.files_written = if dry_run { 0 } else { self.files_changed };
    }

    fn into_value(self) -> VmValue {
        build_dict([
            ("files_total", VmValue::Int(self.files_total as i64)),
            ("files_matched", VmValue::Int(self.files_matched as i64)),
            ("files_changed", VmValue::Int(self.files_changed as i64)),
            ("files_written", VmValue::Int(self.files_written as i64)),
            ("files_failed", VmValue::Int(self.files_failed as i64)),
            (
                "match_count_total",
                VmValue::Int(self.match_count_total as i64),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;
    use tempfile::NamedTempFile;

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn vm_list(items: &[&str]) -> VmValue {
        VmValue::List(Arc::new(items.iter().map(|s| vm_string(s)).collect()))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert(harn_vm::value::intern_key(k), v.clone());
        }
        VmValue::dict(map)
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

    fn n(value: &VmValue) -> i64 {
        match value {
            VmValue::Int(i) => *i,
            other => panic!("expected int, got {other:?}"),
        }
    }

    fn b(value: &VmValue) -> bool {
        match value {
            VmValue::Bool(v) => *v,
            other => panic!("expected bool, got {other:?}"),
        }
    }

    fn files(result: &VmValue) -> Vec<VmValue> {
        match field(result, "files") {
            VmValue::List(items) => items.as_ref().clone(),
            other => panic!("expected list, got {other:?}"),
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
        run(&[payload]).expect("batch_apply runs")
    }

    const BODY_QUERY: &str = "(function_item body: (block) @target)";

    #[test]
    fn previews_every_file_without_writing_by_default() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let b_file = write_temp("rs", "fn b() { 2 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let pb = b_file.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa, &pb])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        assert_eq!(s(field(&result, "result")), "ok");
        assert!(b(field(&result, "dry_run")));
        let entries = files(&result);
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(s(field(entry, "result")), "applied");
            assert!(b(field(entry, "changed")));
            assert!(s(field(entry, "preview")).contains("{ 0 }"));
        }
        let summary = field(&result, "summary");
        assert_eq!(n(field(summary, "files_changed")), 2);
        assert_eq!(n(field(summary, "files_written")), 0);
        // Dry-run leaves disk untouched.
        assert_eq!(std::fs::read_to_string(a.path()).unwrap(), "fn a() { 1 }\n");
        assert_eq!(
            std::fs::read_to_string(b_file.path()).unwrap(),
            "fn b() { 2 }\n"
        );
    }

    #[test]
    fn apply_writes_files_when_dry_run_false() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
            ("dry_run", VmValue::Bool(false)),
        ]));
        assert_eq!(n(field(field(&result, "summary"), "files_written")), 1);
        assert_eq!(std::fs::read_to_string(a.path()).unwrap(), "fn a() { 0 }\n");
    }

    #[test]
    fn reapplying_an_applied_codemod_reports_no_further_changes() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let payload = dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
            ("dry_run", VmValue::Bool(false)),
        ]);
        invoke(payload.clone());
        // Second run: the body is now `{ 0 }`; replacing with `{ 0 }` again
        // still matches but produces identical bytes -> no change written.
        let again = invoke(payload);
        let summary = field(&again, "summary");
        assert_eq!(n(field(summary, "files_changed")), 0);
        assert_eq!(n(field(summary, "files_written")), 0);
        let entry = &files(&again)[0];
        assert_eq!(s(field(entry, "result")), "applied");
        assert!(!b(field(entry, "changed")));
        assert_eq!(
            s(field(entry, "before_sha256")),
            s(field(entry, "after_sha256"))
        );
    }

    #[test]
    fn one_unsupported_file_does_not_abort_the_batch() {
        let good = write_temp("rs", "fn a() { 1 }\n");
        let bad = write_temp("xyzlang", "whatever\n");
        let pg = good.path().to_string_lossy().to_string();
        let pb = bad.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pg, &pb])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        let entries = files(&result);
        assert_eq!(s(field(&entries[0], "result")), "applied");
        assert_eq!(s(field(&entries[1], "result")), "unsupported_language");
        assert_eq!(n(field(field(&result, "summary"), "files_failed")), 1);
    }

    #[test]
    fn missing_file_is_a_per_file_read_error() {
        let good = write_temp("rs", "fn a() { 1 }\n");
        let pg = good.path().to_string_lossy().to_string();
        let missing = format!("{pg}.does-not-exist.rs");
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pg, &missing])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        let entries = files(&result);
        assert_eq!(s(field(&entries[0], "result")), "applied");
        assert_eq!(s(field(&entries[1], "result")), "read_error");
        assert!(matches!(field(&entries[1], "details"), VmValue::String(_)));
    }

    #[test]
    fn invalid_query_is_reported_per_file() {
        let a = write_temp("rs", "fn a() {}\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string("((((")),
            ("replacement", vm_string("{}")),
        ]));
        assert_eq!(s(field(&files(&result)[0], "result")), "invalid_query");
    }

    #[test]
    fn no_match_leaves_file_unchanged_and_fingerprinted() {
        let a = write_temp("rs", "fn a() {}\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            (
                "query",
                vm_string(
                    r#"(function_item name: (identifier) @name (#eq? @name "zzz")
                       body: (block) @target)"#,
                ),
            ),
            ("replacement", vm_string("{ 0 }")),
        ]));
        let entry = &files(&result)[0];
        assert_eq!(s(field(entry, "result")), "no_match");
        assert!(!b(field(entry, "changed")));
        assert_eq!(
            s(field(entry, "before_sha256")),
            s(field(entry, "after_sha256"))
        );
    }

    #[test]
    fn unique_selector_reports_ambiguous_per_file() {
        let a = write_temp("rs", "fn a() { 1 }\nfn b() { 2 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        let entry = &files(&result)[0];
        assert_eq!(s(field(entry, "result")), "ambiguous");
        assert_eq!(n(field(entry, "match_count")), 2);
        assert!(!b(field(entry, "changed")));
    }

    #[test]
    fn select_all_rewrites_every_match() {
        let a = write_temp("rs", "fn a() { 1 }\nfn b() { 2 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
            ("select", vm_string("all")),
        ]));
        let entry = &files(&result)[0];
        assert_eq!(s(field(entry, "result")), "applied");
        assert_eq!(n(field(entry, "match_count")), 2);
        assert_eq!(s(field(entry, "preview")).matches("{ 0 }").count(), 2);
    }

    #[test]
    fn validate_rejects_post_edit_syntax_error_and_skips_write() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ ( }")),
            ("dry_run", VmValue::Bool(false)),
        ]));
        let entry = &files(&result)[0];
        assert_eq!(s(field(entry, "result")), "syntax_error");
        // The file must be left untouched on rejection.
        assert_eq!(std::fs::read_to_string(a.path()).unwrap(), "fn a() { 1 }\n");
    }

    #[test]
    fn preserves_formatting_outside_the_matched_span() {
        let a = write_temp("rs", "fn a() {\n    let x = 1;\n}\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ let x = 42; }")),
        ]));
        let preview = s(field(&files(&result)[0], "preview"));
        assert!(preview.starts_with("fn a() {"));
        assert!(preview.contains("let x = 42"));
    }

    #[test]
    fn fix_is_an_alias_for_replacement() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("fix", vm_string("{ 9 }")),
        ]));
        assert!(s(field(&files(&result)[0], "preview")).contains("{ 9 }"));
    }

    #[test]
    fn replacement_and_fix_together_is_rejected() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let err = run(&[dict(&[
            ("paths", vm_list(&[&pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
            ("fix", vm_string("{ 1 }")),
        ])])
        .expect_err("both replacement and fix must be rejected");
        match err {
            HostlibError::InvalidParameter { param, .. } => assert_eq!(param, "replacement"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    #[test]
    fn empty_paths_is_missing_parameter() {
        let err = run(&[dict(&[
            ("paths", vm_list(&[])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ])])
        .expect_err("empty paths is a caller bug");
        match err {
            HostlibError::MissingParameter { param, .. } => assert_eq!(param, "paths"),
            other => panic!("expected MissingParameter, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_paths_are_processed_once() {
        let a = write_temp("rs", "fn a() { 1 }\n");
        let pa = a.path().to_string_lossy().to_string();
        let result = invoke(dict(&[
            ("paths", vm_list(&[&pa, &pa])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        assert_eq!(files(&result).len(), 1);
        assert_eq!(n(field(field(&result, "summary"), "files_total")), 1);
    }

    #[test]
    fn runs_across_mixed_grammars() {
        let rs = write_temp("rs", "fn a() { 1 }\n");
        let py = write_temp("py", "def greet():\n    return 1\n");
        let prs = rs.path().to_string_lossy().to_string();
        let ppy = py.path().to_string_lossy().to_string();
        // A query valid for Rust is invalid for Python's grammar, so each
        // file is judged against its own grammar independently.
        let result = invoke(dict(&[
            ("paths", vm_list(&[&prs, &ppy])),
            ("query", vm_string(BODY_QUERY)),
            ("replacement", vm_string("{ 0 }")),
        ]));
        let entries = files(&result);
        assert_eq!(s(field(&entries[0], "result")), "applied");
        // Python has no `function_item`/`block` node kinds -> invalid_query.
        assert_eq!(s(field(&entries[1], "result")), "invalid_query");
    }
}
