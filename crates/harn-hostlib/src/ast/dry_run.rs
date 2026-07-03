//! `ast.dry_run` — render a multi-op edit plan to a unified diff bundle
//! without committing anything to disk.
//!
//! The runner opens a *transient* staged-fs session (see
//! [`crate::fs`]), dispatches each plan op through its op-specific
//! handler with that session id wired in, then walks the resulting
//! overlay to produce a per-file unified diff plus a roll-up summary.
//! When every op has been processed the transient session is discarded,
//! so the on-disk tree is byte-identical before and after the call.
//!
//! ## Wire shape
//!
//! Request (see `schemas/ast/dry_run.request.json`):
//!
//! ```json
//! {
//!   "plan": [
//!     { "op": "apply_node",       "path": "...", "query": "...", "replacement": "...", "select": "first" },
//!     { "op": "insert_at_anchor", "path": "...", "query": "...", "position": "first_child", "content": "..." },
//!     { "op": "safe_text_patch",  "path": "...", "old_text": "...", "new_text": "..." },
//!     { "op": "rename_symbol",    "symbol_ref": "...", "new_name": "..." }
//!   ]
//! }
//! ```
//!
//! Response (see `schemas/ast/dry_run.response.json`):
//!
//! ```json
//! {
//!   "result": "ok" | "partial" | "no_ops_applied",
//!   "per_file_unified_diff": [{ "path": "...", "diff": "...", "lines_added": N, "lines_removed": N }],
//!   "summary": {
//!     "files_touched": N,
//!     "lines_added": N,
//!     "lines_removed": N,
//!     "ops_applied": N,
//!     "ops_rejected": N
//!   },
//!   "ops": [{ "op": "...", "applied": bool, "result": "applied"|"rejected"|..., "details": "..." }]
//! }
//! ```
//!
//! The diff format is standard unified diff, compatible with
//! `git apply --check`. New files use `--- /dev/null`; deleted files
//! use `+++ /dev/null`. Missing trailing newlines surface as the
//! conventional `\ No newline at end of file` markers.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::VmValue;

use crate::code_index::SharedIndex;
use crate::error::HostlibError;
use crate::fs::FsMode;
use crate::tools::args::{
    build_dict, dict_arg, optional_string, require_string, str_value, to_agent_path,
};

use super::unified_diff::{render as render_unified_diff, ChangeKind};

const BUILTIN: &str = "hostlib_ast_dry_run";
const TRANSIENT_PREFIX: &str = "__edit_dry_run__-";

/// Entry point registered as the `hostlib_ast_dry_run` builtin.
pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    run_with_code_index(None, args)
}

pub(super) fn run_with_code_index(
    code_index: Option<&SharedIndex>,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let plan = require_plan(dict)?;

    let session_id = transient_session_id();
    crate::fs::set_mode(&session_id, FsMode::Staged, None)?;

    let mut op_outcomes: Vec<OpOutcome> = Vec::with_capacity(plan.len());
    let mut touched: BTreeSet<PathBuf> = BTreeSet::new();
    let mut applied_count = 0usize;
    let mut rejected_count = 0usize;

    for (index, op_value) in plan.into_iter().enumerate() {
        let outcome = dispatch_op(code_index, &session_id, index, &op_value);
        match &outcome {
            OpOutcome::Applied { paths, .. } => {
                applied_count += 1;
                touched.extend(paths.iter().cloned());
            }
            OpOutcome::Rejected { .. } | OpOutcome::Error { .. } => {
                rejected_count += 1;
            }
        }
        op_outcomes.push(outcome);
    }

    let mut per_file_diffs: Vec<FileDiffEntry> = Vec::new();
    let mut lines_added_total = 0usize;
    let mut lines_removed_total = 0usize;
    let staged_paths: BTreeSet<PathBuf> = crate::fs::staged_status(&session_id)?
        .pending_writes
        .into_iter()
        .map(|write| PathBuf::from(write.path))
        .collect();
    let diff_targets = diff_targets(staged_paths, touched);

    for target in &diff_targets {
        let (before, before_existed) = read_before(&target.read_path);
        let after = read_after(&target.read_path, &session_id).unwrap_or_default();
        let kind = if !before_existed {
            ChangeKind::Create
        } else if after.is_empty() {
            // staged-fs deletes show up here too (delete entries return
            // a NotFound when read). For now, dry_run treats them as
            // file deletions.
            ChangeKind::Delete
        } else {
            ChangeKind::Modify
        };
        let display = diff_path_label(&target.display_path);
        let diff = render_unified_diff(&display, &before, &after, kind);
        lines_added_total += diff.lines_added;
        lines_removed_total += diff.lines_removed;
        per_file_diffs.push(FileDiffEntry {
            path: display,
            diff: diff.text,
            lines_added: diff.lines_added,
            lines_removed: diff.lines_removed,
        });
    }

    // Always discard and remove the transient session so no staged-fs metadata
    // remains after a preview.
    let _ = crate::fs::discard_staged(&session_id, &[]);
    let _ = crate::fs::remove_session_state(&session_id, None);

    let result_kind = if applied_count == 0 {
        "no_ops_applied"
    } else if rejected_count == 0 {
        "ok"
    } else {
        "partial"
    };

    Ok(build_response(
        result_kind,
        &per_file_diffs,
        SummaryCounts {
            files_touched: per_file_diffs.len(),
            lines_added: lines_added_total,
            lines_removed: lines_removed_total,
            ops_applied: applied_count,
            ops_rejected: rejected_count,
        },
        &op_outcomes,
    ))
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

fn require_plan(
    dict: &harn_vm::value::DictMap,
) -> Result<Vec<Arc<harn_vm::value::DictMap>>, HostlibError> {
    match dict.get("plan") {
        None | Some(VmValue::Nil) => Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "plan",
        }),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                match item {
                    VmValue::Dict(d) => out.push(d.clone()),
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin: BUILTIN,
                            param: "plan",
                            message: format!(
                                "plan[{idx}]: expected dict op, got {}",
                                other.type_name()
                            ),
                        });
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "plan",
            message: format!("expected list of op dicts, got {}", other.type_name()),
        }),
    }
}

fn transient_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{TRANSIENT_PREFIX}{pid}-{nanos}-{counter}")
}

// ---------------------------------------------------------------------------
// Op dispatch
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum OpOutcome {
    Applied {
        op: &'static str,
        paths: Vec<PathBuf>,
        details: String,
        match_count: usize,
    },
    Rejected {
        op: String,
        reason: &'static str,
        details: String,
        path: Option<String>,
    },
    Error {
        op: String,
        message: String,
    },
}

fn dispatch_op(
    code_index: Option<&SharedIndex>,
    session_id: &str,
    index: usize,
    raw: &Arc<harn_vm::value::DictMap>,
) -> OpOutcome {
    let dict = raw.as_ref();
    let op_name = match dict.get("op") {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return OpOutcome::Error {
                op: "<unknown>".into(),
                message: format!(
                    "plan[{index}].op: expected string, got {}",
                    other.type_name()
                ),
            };
        }
        None => {
            return OpOutcome::Error {
                op: "<unknown>".into(),
                message: format!("plan[{index}].op: missing"),
            };
        }
    };

    match op_name.as_str() {
        "apply_node" => run_apply_node(session_id, raw),
        "insert_at_anchor" => run_insert_at_anchor(session_id, raw),
        "safe_text_patch" => run_safe_text_patch(session_id, raw),
        "rename_symbol" => run_rename_symbol(code_index, session_id, raw),
        other => OpOutcome::Rejected {
            op: other.to_string(),
            reason: "unknown_op",
            details: format!(
                "unrecognized op `{other}`; expected one of \
                 [apply_node, insert_at_anchor, safe_text_patch, rename_symbol]"
            ),
            path: optional_string(BUILTIN, dict, "path").ok().flatten(),
        },
    }
}

fn run_apply_node(session_id: &str, raw: &Arc<harn_vm::value::DictMap>) -> OpOutcome {
    delegate_to_builtin(session_id, raw, "apply_node", super::apply_node::run)
}

fn run_insert_at_anchor(session_id: &str, raw: &Arc<harn_vm::value::DictMap>) -> OpOutcome {
    delegate_to_builtin(
        session_id,
        raw,
        "insert_at_anchor",
        super::insert_at_anchor::run,
    )
}

/// Forward an op dict to one of the AST builtin runners with our
/// transient session wired in, then parse its tagged-union response
/// back into an [`OpOutcome`]. The `dry_run` / `session_id` fields are
/// always overridden so the write lands in the overlay, never on disk —
/// even if the caller passed conflicting values.
fn delegate_to_builtin(
    session_id: &str,
    raw: &Arc<harn_vm::value::DictMap>,
    op_label: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) -> OpOutcome {
    let mut forwarded: harn_vm::value::DictMap = (**raw).clone();
    forwarded.remove("op");
    forwarded.insert(
        harn_vm::value::intern_key("session_id"),
        str_value(session_id),
    );
    forwarded.insert(harn_vm::value::intern_key("dry_run"), VmValue::Bool(false));
    let request = VmValue::dict(forwarded);
    match runner(&[request]) {
        Ok(VmValue::Dict(result)) => parse_builtin_outcome(&result, op_label),
        Ok(_) => OpOutcome::Error {
            op: op_label.into(),
            message: format!("{op_label} returned non-dict result"),
        },
        Err(err) => OpOutcome::Error {
            op: op_label.into(),
            message: err.to_string(),
        },
    }
}

fn parse_builtin_outcome(
    result: &Arc<harn_vm::value::DictMap>,
    op_label: &'static str,
) -> OpOutcome {
    let result_str = match result.get("result") {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return OpOutcome::Error {
                op: op_label.into(),
                message: format!("{op_label} response missing `result`"),
            }
        }
    };
    let path = match result.get("path") {
        Some(VmValue::String(s)) => Some(s.to_string()),
        _ => None,
    };
    if result_str == "applied" {
        let match_count = match result.get("match_count") {
            Some(VmValue::Int(n)) => *n as usize,
            _ => 0,
        };
        let path = path.as_ref().map(PathBuf::from).unwrap_or_default();
        OpOutcome::Applied {
            op: op_label,
            paths: vec![path],
            details: format!("{op_label}: applied with {match_count} match(es)"),
            match_count,
        }
    } else {
        OpOutcome::Rejected {
            op: op_label.into(),
            reason: classify_builtin_failure(&result_str),
            details: match result.get("details") {
                Some(VmValue::String(s)) => s.to_string(),
                _ => result_str.clone(),
            },
            path,
        }
    }
}

fn classify_builtin_failure(result: &str) -> &'static str {
    match result {
        "no_match" => "no_match",
        "ambiguous" => "ambiguous",
        "invalid_query" => "invalid_query",
        "invalid_anchor" => "invalid_anchor",
        "unsupported_language" => "unsupported_language",
        "syntax_error" => "syntax_error",
        _ => "rejected",
    }
}

fn run_rename_symbol(
    code_index: Option<&SharedIndex>,
    session_id: &str,
    raw: &Arc<harn_vm::value::DictMap>,
) -> OpOutcome {
    let Some(index) = code_index else {
        return OpOutcome::Rejected {
            op: "rename_symbol".into(),
            reason: "code_index_unavailable",
            details: "rename_symbol dry-run ops require hostlib_ast_dry_run to be registered with the shared code_index capability"
                .into(),
            path: None,
        };
    };
    let mut forwarded: harn_vm::value::DictMap = (**raw).clone();
    forwarded.remove("op");
    forwarded.insert(
        harn_vm::value::intern_key("session_id"),
        str_value(session_id),
    );
    forwarded.insert(harn_vm::value::intern_key("dry_run"), VmValue::Bool(false));
    forwarded
        .entry(harn_vm::value::intern_key("scope"))
        .or_insert_with(|| str_value("workspace"));

    let request = VmValue::dict(forwarded);
    match crate::code_index::run_rename_symbol(index, &[request]) {
        Ok(VmValue::Dict(result)) => parse_rename_outcome(&result),
        Ok(_) => OpOutcome::Error {
            op: "rename_symbol".into(),
            message: "rename_symbol returned non-dict result".into(),
        },
        Err(err) => OpOutcome::Error {
            op: "rename_symbol".into(),
            message: err.to_string(),
        },
    }
}

fn parse_rename_outcome(result: &Arc<harn_vm::value::DictMap>) -> OpOutcome {
    let result_str = match result.get("result") {
        Some(VmValue::String(s)) => s.to_string(),
        _ => {
            return OpOutcome::Error {
                op: "rename_symbol".into(),
                message: "rename_symbol response missing `result`".into(),
            }
        }
    };
    let details = match result.get("details") {
        Some(VmValue::String(s)) => s.to_string(),
        _ => result_str.clone(),
    };
    if result_str != "applied" {
        return OpOutcome::Rejected {
            op: "rename_symbol".into(),
            reason: classify_rename_failure(&result_str),
            details,
            path: None,
        };
    }

    let failed_count = match result.get("failed_paths_with_reasons") {
        Some(VmValue::List(items)) => items.len(),
        _ => 0,
    };
    if failed_count > 0 {
        return OpOutcome::Error {
            op: "rename_symbol".into(),
            message: "rename_symbol staged preview had failed paths; see standalone edit_rename_symbol for details".into(),
        };
    }

    OpOutcome::Applied {
        op: "rename_symbol",
        paths: rename_touched_paths(result),
        details,
        match_count: int_field(result, "match_count").unwrap_or(0) as usize,
    }
}

fn classify_rename_failure(result: &str) -> &'static str {
    match result {
        "conflict" => "conflict",
        "no_match" => "no_match",
        "ambiguous_symbol" => "ambiguous_symbol",
        "unsupported_language" => "unsupported_language",
        "syntax_error" => "syntax_error",
        "invalid_identifier" => "invalid_identifier",
        _ => "rejected",
    }
}

fn rename_touched_paths(result: &Arc<harn_vm::value::DictMap>) -> Vec<PathBuf> {
    match result.get("touched_files") {
        Some(VmValue::List(files)) => files
            .iter()
            .filter_map(|file| match file {
                VmValue::Dict(entry) => match entry.get("path") {
                    Some(VmValue::String(path)) => Some(PathBuf::from(path.to_string())),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn int_field(result: &Arc<harn_vm::value::DictMap>, key: &str) -> Option<i64> {
    match result.get(key) {
        Some(VmValue::Int(n)) => Some(*n),
        _ => None,
    }
}

fn run_safe_text_patch(session_id: &str, raw: &Arc<harn_vm::value::DictMap>) -> OpOutcome {
    let dict = raw.as_ref();
    let path_str = match require_string(BUILTIN, dict, "path") {
        Ok(s) => s,
        Err(err) => {
            return OpOutcome::Error {
                op: "safe_text_patch".into(),
                message: err.to_string(),
            };
        }
    };
    let old_text = match require_string(BUILTIN, dict, "old_text") {
        Ok(s) => s,
        Err(err) => {
            return OpOutcome::Error {
                op: "safe_text_patch".into(),
                message: err.to_string(),
            };
        }
    };
    let new_text = match require_string(BUILTIN, dict, "new_text") {
        Ok(s) => s,
        Err(err) => {
            return OpOutcome::Error {
                op: "safe_text_patch".into(),
                message: err.to_string(),
            };
        }
    };

    if old_text.is_empty() {
        return OpOutcome::Rejected {
            op: "safe_text_patch".into(),
            reason: "empty_old_text",
            details: "safe_text_patch requires non-empty `old_text`".into(),
            path: Some(path_str),
        };
    }
    if old_text == new_text {
        return OpOutcome::Rejected {
            op: "safe_text_patch".into(),
            reason: "no_op",
            details: "safe_text_patch is a no-op (old_text == new_text)".into(),
            path: Some(path_str),
        };
    }

    let path = PathBuf::from(&path_str);
    let source = match read_for_session(&path, session_id) {
        Ok(s) => s,
        Err(err) => {
            return OpOutcome::Rejected {
                op: "safe_text_patch".into(),
                reason: "read_failed",
                details: err.to_string(),
                path: Some(path_str),
            };
        }
    };
    let match_count = source.matches(&old_text).count();
    if match_count == 0 {
        return OpOutcome::Rejected {
            op: "safe_text_patch".into(),
            reason: "no_match",
            details: "old_text did not match exactly".into(),
            path: Some(path_str),
        };
    }
    if match_count > 1 {
        return OpOutcome::Rejected {
            op: "safe_text_patch".into(),
            reason: "ambiguous",
            details: format!("old_text matched {match_count} regions"),
            path: Some(path_str),
        };
    }
    let patched = source.replacen(&old_text, &new_text, 1);
    if let Err(err) = write_for_session(&path, &patched, session_id) {
        return OpOutcome::Error {
            op: "safe_text_patch".into(),
            message: err.to_string(),
        };
    }
    OpOutcome::Applied {
        op: "safe_text_patch",
        paths: vec![path],
        details: "safe_text_patch: replaced unique exact match".into(),
        match_count: 1,
    }
}

// ---------------------------------------------------------------------------
// I/O helpers — routed through staged-fs so reads see prior in-plan writes
// ---------------------------------------------------------------------------

fn read_for_session(path: &Path, session_id: &str) -> Result<String, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, Some(session_id)) {
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
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write_for_session(path: &Path, contents: &str, session_id: &str) -> Result<(), HostlibError> {
    let outcome = crate::fs::stage_write_or_none(
        BUILTIN,
        path,
        contents.as_bytes(),
        true,
        true,
        Some(session_id),
    )?;
    if outcome.is_some() {
        Ok(())
    } else {
        Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: "transient dry-run session is not in staged mode".into(),
        })
    }
}

fn read_before(path: &Path) -> (String, bool) {
    match std::fs::read(path) {
        Ok(bytes) => (String::from_utf8_lossy(&bytes).into_owned(), true),
        Err(_) => (String::new(), false),
    }
}

fn read_after(path: &Path, session_id: &str) -> Option<String> {
    match crate::fs::read(path, Some(session_id))? {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => Some(String::new()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffTarget {
    read_path: PathBuf,
    display_path: PathBuf,
}

fn diff_targets(staged_paths: BTreeSet<PathBuf>, touched: BTreeSet<PathBuf>) -> Vec<DiffTarget> {
    if staged_paths.is_empty() {
        return touched
            .into_iter()
            .map(|path| DiffTarget {
                read_path: path.clone(),
                display_path: path,
            })
            .collect();
    }

    let allow_single_path_fallback = staged_paths.len() == 1 && touched.len() == 1;
    let mut remaining_touched: Vec<PathBuf> = touched.into_iter().collect();
    staged_paths
        .into_iter()
        .map(|read_path| {
            let display_path = take_matching_display_path(
                &read_path,
                &mut remaining_touched,
                allow_single_path_fallback,
            )
            .unwrap_or_else(|| read_path.clone());
            DiffTarget {
                read_path,
                display_path,
            }
        })
        .collect()
}

fn take_matching_display_path(
    read_path: &Path,
    candidates: &mut Vec<PathBuf>,
    allow_single_path_fallback: bool,
) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
    }
    if allow_single_path_fallback && candidates.len() == 1 {
        return Some(candidates.remove(0));
    }

    let index = candidates
        .iter()
        .position(|candidate| paths_match_for_display(read_path, candidate))?;
    Some(candidates.remove(index))
}

fn paths_match_for_display(read_path: &Path, candidate: &Path) -> bool {
    if read_path == candidate {
        return true;
    }
    if let (Ok(read), Ok(candidate)) = (
        std::fs::canonicalize(read_path),
        std::fs::canonicalize(candidate),
    ) {
        if read == candidate {
            return true;
        }
    }

    let read = diff_path_label(read_path);
    let candidate = diff_path_label(candidate);
    read == candidate || read.ends_with(&format!("/{candidate}"))
}

fn diff_path_label(path: &Path) -> String {
    to_agent_path(path)
}

// ---------------------------------------------------------------------------
// Response shaping
// ---------------------------------------------------------------------------

struct FileDiffEntry {
    path: String,
    diff: String,
    lines_added: usize,
    lines_removed: usize,
}

struct SummaryCounts {
    files_touched: usize,
    lines_added: usize,
    lines_removed: usize,
    ops_applied: usize,
    ops_rejected: usize,
}

fn build_response(
    result: &str,
    per_file: &[FileDiffEntry],
    summary: SummaryCounts,
    op_outcomes: &[OpOutcome],
) -> VmValue {
    let per_file_value = VmValue::List(Arc::new(
        per_file
            .iter()
            .map(|entry| {
                build_dict([
                    ("path", str_value(&entry.path)),
                    ("diff", str_value(&entry.diff)),
                    ("lines_added", VmValue::Int(entry.lines_added as i64)),
                    ("lines_removed", VmValue::Int(entry.lines_removed as i64)),
                ])
            })
            .collect(),
    ));
    let summary_value = build_dict([
        ("files_touched", VmValue::Int(summary.files_touched as i64)),
        ("lines_added", VmValue::Int(summary.lines_added as i64)),
        ("lines_removed", VmValue::Int(summary.lines_removed as i64)),
        ("ops_applied", VmValue::Int(summary.ops_applied as i64)),
        ("ops_rejected", VmValue::Int(summary.ops_rejected as i64)),
    ]);
    let ops_value = VmValue::List(Arc::new(
        op_outcomes.iter().map(op_outcome_to_value).collect(),
    ));

    build_dict([
        ("result", str_value(result)),
        ("per_file_unified_diff", per_file_value),
        ("summary", summary_value),
        ("ops", ops_value),
    ])
}

fn op_outcome_to_value(outcome: &OpOutcome) -> VmValue {
    match outcome {
        OpOutcome::Applied {
            op,
            paths,
            details,
            match_count,
        } => {
            let path_values: Vec<VmValue> = paths
                .iter()
                .map(|path| str_value(diff_path_label(path)))
                .collect();
            let primary = paths.first().cloned().unwrap_or_default();
            build_dict([
                ("op", str_value(*op)),
                ("applied", VmValue::Bool(true)),
                ("result", str_value("applied")),
                ("path", str_value(diff_path_label(&primary))),
                ("paths", VmValue::List(Arc::new(path_values))),
                ("details", str_value(details)),
                ("match_count", VmValue::Int(*match_count as i64)),
            ])
        }
        OpOutcome::Rejected {
            op,
            reason,
            details,
            path,
        } => {
            let mut entries: Vec<(&'static str, VmValue)> = vec![
                ("op", str_value(op)),
                ("applied", VmValue::Bool(false)),
                ("result", str_value("rejected")),
                ("reason", str_value(*reason)),
                ("details", str_value(details)),
            ];
            if let Some(path) = path {
                entries.push(("path", str_value(path)));
            }
            build_dict(entries)
        }
        OpOutcome::Error { op, message } => build_dict([
            ("op", str_value(op)),
            ("applied", VmValue::Bool(false)),
            ("result", str_value("error")),
            ("details", str_value(message)),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::{CodeIndexCapability, IndexState};
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::{tempdir, NamedTempFile};

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn vm_dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert(harn_vm::value::intern_key(k), v.clone());
        }
        VmValue::dict(map)
    }

    fn vm_list(items: &[VmValue]) -> VmValue {
        VmValue::List(Arc::new(items.to_vec()))
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
        file.write_all(source.as_bytes()).expect("write");
        file
    }

    fn invoke(payload: VmValue) -> VmValue {
        run(&[payload]).expect("dry_run runs")
    }

    fn invoke_with_code_index(index: &SharedIndex, payload: VmValue) -> VmValue {
        run_with_code_index(Some(index), &[payload]).expect("dry_run runs")
    }

    fn build_index(root: &Path) -> CodeIndexCapability {
        let capability = CodeIndexCapability::new();
        let shared = capability.shared();
        let mut guard = shared.lock().expect("mutex");
        let (state, _outcome) = IndexState::build_from_root(root);
        *guard = Some(state);
        drop(guard);
        capability
    }

    #[test]
    fn diff_target_labels_keep_logical_single_op_path() {
        let staged = BTreeSet::from([PathBuf::from(r"C:\Temp\project\module.rs")]);
        let touched = BTreeSet::from([PathBuf::from("C:/Temp/project/module.rs")]);

        let targets = diff_targets(staged, touched);

        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].read_path,
            PathBuf::from(r"C:\Temp\project\module.rs")
        );
        assert_eq!(
            diff_path_label(&targets[0].display_path),
            "C:/Temp/project/module.rs"
        );
    }

    #[test]
    fn diff_target_labels_pair_relative_multi_file_paths() {
        let staged = BTreeSet::from([
            PathBuf::from("/tmp/workspace/src/a.rs"),
            PathBuf::from("/tmp/workspace/src/b.rs"),
        ]);
        let touched = BTreeSet::from([PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]);

        let labels: Vec<String> = diff_targets(staged, touched)
            .into_iter()
            .map(|target| diff_path_label(&target.display_path))
            .collect();

        assert_eq!(labels, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn diff_target_labels_do_not_guess_unmatched_multi_file_paths() {
        let staged = BTreeSet::from([
            PathBuf::from("/tmp/workspace/src/a.rs"),
            PathBuf::from("/tmp/workspace/src/b.rs"),
        ]);
        let touched = BTreeSet::from([PathBuf::from("elsewhere/c.rs")]);

        let labels: Vec<String> = diff_targets(staged, touched)
            .into_iter()
            .map(|target| diff_path_label(&target.display_path))
            .collect();

        assert_eq!(
            labels,
            vec!["/tmp/workspace/src/a.rs", "/tmp/workspace/src/b.rs"]
        );
    }

    #[test]
    fn empty_plan_emits_no_ops_applied() {
        let result = invoke(vm_dict(&[("plan", vm_list(&[]))]));
        assert_eq!(s(field(&result, "result")), "no_ops_applied");
        let summary = field(&result, "summary");
        assert_eq!(
            match field(summary, "files_touched") {
                VmValue::Int(n) => *n,
                _ => panic!(),
            },
            0
        );
    }

    #[test]
    fn apply_node_op_produces_diff_without_touching_disk() {
        let source = "fn alpha() { 1 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("apply_node")),
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 2 }")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(per_file.len(), 1);
        let entry = &per_file[0];
        let diff = s(field(entry, "diff"));
        assert!(diff.contains("--- a/"));
        assert!(diff.contains("+++ b/"));
        assert!(diff.contains("@@ -"));
        assert!(diff.contains("-fn alpha() { 1 }"));
        assert!(diff.contains("+fn alpha() { 2 }"));
        // Disk is untouched.
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn safe_text_patch_op_uses_exact_match() {
        let source = "const x = 1;\nconst y = 2;\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("safe_text_patch")),
            ("path", vm_string(&path)),
            ("old_text", vm_string("const y = 2;")),
            ("new_text", vm_string("const y = 42;")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        let entry = &per_file[0];
        let diff = s(field(entry, "diff"));
        assert!(diff.contains("-const y = 2;"));
        assert!(diff.contains("+const y = 42;"));
        let on_disk = std::fs::read_to_string(file.path()).expect("read");
        assert_eq!(on_disk, source);
    }

    #[test]
    fn safe_text_patch_rejects_ambiguous_match() {
        let source = "todo\ntodo\n";
        let file = write_temp("txt", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("safe_text_patch")),
            ("path", vm_string(&path)),
            ("old_text", vm_string("todo")),
            ("new_text", vm_string("done")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "no_ops_applied");
        let ops = match field(&result, "ops") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(s(field(&ops[0], "result")), "rejected");
        assert_eq!(s(field(&ops[0], "reason")), "ambiguous");
    }

    #[test]
    fn insert_at_anchor_first_child_prepends_to_block() {
        let source = "fn run() {\n    let x = 1;\n}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("insert_at_anchor")),
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @anchor)")),
            ("position", vm_string("first_child")),
            // first_child inserts at the start_byte of the first named
            // child — the source's indent is already in place, so the
            // content carries only the new statement plus a trailing
            // indent for the bumped-down original line.
            ("content", vm_string("let inserted = true;\n    ")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        let diff = s(field(&per_file[0], "diff"));
        assert!(diff.contains("+    let inserted = true;"));
    }

    #[test]
    fn insert_at_anchor_after_appends_after_anchor() {
        let source = "fn one() {}\nfn two() {}\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("insert_at_anchor")),
            ("path", vm_string(&path)),
            (
                "query",
                vm_string(r#"(function_item name: (identifier) @name (#eq? @name "one")) @anchor"#),
            ),
            ("position", vm_string("after")),
            ("content", vm_string("\nfn one_b() {}")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        let diff = s(field(&per_file[0], "diff"));
        assert!(diff.contains("+fn one_b() {}"));
    }

    #[test]
    fn rename_symbol_op_rejects_without_code_index() {
        let op = vm_dict(&[
            ("op", vm_string("rename_symbol")),
            (
                "symbol_ref",
                vm_dict(&[
                    ("name", vm_string("alpha")),
                    ("path", vm_string("src/lib.rs")),
                    ("kind", vm_string("Function")),
                ]),
            ),
            ("new_name", vm_string("beta")),
            ("scope", vm_string("workspace")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        let ops = match field(&result, "ops") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(s(field(&ops[0], "result")), "rejected");
        assert_eq!(s(field(&ops[0], "reason")), "code_index_unavailable");
    }

    #[test]
    fn rename_symbol_op_produces_diff_without_touching_disk() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        let original = "fn alpha() { println!(\"hi\"); }\nfn caller() { alpha(); }\n";
        fs::write(root.join("src/lib.rs"), original).unwrap();

        let capability = build_index(root);
        let shared = capability.shared();
        let op = vm_dict(&[
            ("op", vm_string("rename_symbol")),
            (
                "symbol_ref",
                vm_dict(&[
                    ("name", vm_string("alpha")),
                    ("path", vm_string("src/lib.rs")),
                    ("kind", vm_string("Function")),
                ]),
            ),
            ("new_name", vm_string("beta")),
            ("scope", vm_string("workspace")),
        ]);
        let result = invoke_with_code_index(&shared, vm_dict(&[("plan", vm_list(&[op]))]));
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(per_file.len(), 1);
        let diff = s(field(&per_file[0], "diff"));
        assert!(diff.contains("-fn alpha()"));
        assert!(diff.contains("+fn beta()"));
        assert!(diff.contains("-fn caller() { alpha(); }"));
        assert!(diff.contains("+fn caller() { beta(); }"));
        let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(on_disk, original);
    }

    #[test]
    fn multi_op_plan_aggregates_per_file_and_summary() {
        let source = "fn a() { 1 }\nfn b() { 2 }\n";
        let file = write_temp("rs", source);
        let path = file.path().to_string_lossy().to_string();
        let op1 = vm_dict(&[
            ("op", vm_string("apply_node")),
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 99 }")),
            ("select", vm_string("first")),
        ]);
        let op2 = vm_dict(&[
            ("op", vm_string("apply_node")),
            ("path", vm_string(&path)),
            ("query", vm_string("(function_item body: (block) @target)")),
            ("replacement", vm_string("{ 100 }")),
            ("select", vm_string("nth")),
            ("nth", VmValue::Int(2)),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op1, op2]))]));
        // Sequential ops in one plan share the staged session, so the
        // second op sees the first op's pending write — the resulting
        // diff carries both replacements for the same file.
        assert_eq!(s(field(&result, "result")), "ok");
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(per_file.len(), 1);
        let diff = s(field(&per_file[0], "diff"));
        assert!(diff.contains("+fn a() { 99 }"));
        assert!(diff.contains("+fn b() { 100 }"));
        let summary = field(&result, "summary");
        assert_eq!(
            match field(summary, "ops_applied") {
                VmValue::Int(n) => *n,
                _ => panic!(),
            },
            2
        );
    }

    #[test]
    fn unknown_op_is_rejected() {
        let op = vm_dict(&[
            ("op", vm_string("blow_up_the_world")),
            ("path", vm_string("/nope")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        let ops = match field(&result, "ops") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        assert_eq!(s(field(&ops[0], "reason")), "unknown_op");
    }

    #[test]
    fn diff_is_git_apply_check_compatible() {
        let source = "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\nlet e = 5;\n";
        let file = write_temp("ts", source);
        let path = file.path().to_string_lossy().to_string();
        let op = vm_dict(&[
            ("op", vm_string("safe_text_patch")),
            ("path", vm_string(&path)),
            ("old_text", vm_string("let c = 3;")),
            ("new_text", vm_string("let c = 30;")),
        ]);
        let result = invoke(vm_dict(&[("plan", vm_list(&[op]))]));
        let per_file = match field(&result, "per_file_unified_diff") {
            VmValue::List(items) => items.clone(),
            _ => panic!(),
        };
        let diff = s(field(&per_file[0], "diff"));
        // Spot-check the hunk header shape; full `git apply --check`
        // round-trip lives in the conformance suite.
        assert!(diff.contains("@@ -1,5 +1,5 @@") || diff.contains("@@ -"));
        assert!(diff.contains(" let a = 1;"));
        assert!(diff.contains("-let c = 3;"));
        assert!(diff.contains("+let c = 30;"));
    }
}
