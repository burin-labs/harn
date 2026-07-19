//! `harn replay --counterfactual <plan.harn>` — evaluate an alternate edit
//! plan after replay rehydrates a `--at` cutoff and report the divergent
//! file set without mutating the recorded session or the workspace.
//!
//! This composes two shipped primitives rather than reimplementing any of
//! their machinery:
//!
//! - **B.5 `edit.dry_run`** (`std/edit::edit_dry_run`, backed by the
//!   `hostlib_ast_dry_run` builtin) lowers an ordered edit plan into a
//!   per-file unified-diff bundle.
//! - **#1722 staged-fs** isolates that dry-run inside a throw-away overlay
//!   so the on-disk tree is byte-identical before and after the call.
//!
//! The CLI's only job here is to (1) evaluate the operator's plan file(s)
//! into an edit plan, (2) run the chained ops through `edit_dry_run`, and
//! (3) project the dry-run result down to a *divergence* — the set of files
//! that would differ from the recorded outcome at the cutoff.
//!
//! ## Plan contract
//!
//! Each `plan.harn` program `return`s one of (a bare trailing expression
//! returns `nil` in Harn, so the plan must use `return`):
//!
//! - a **list** of edit ops — the `plan` argument to `edit_dry_run`
//!   (`return [{op: "apply_node", path, query, replacement}, ...]`); the CLI
//!   calls `edit_dry_run` for you, or
//! - for a single plan only, a **dict** that is already an `edit_dry_run` result (carries
//!   `per_file_unified_diff`) — for plans that prefer to call `edit_dry_run`
//!   themselves and shape the result (`return edit_dry_run({plan: [...]})`).
//!
//! Either way the divergence is read off the same `per_file_unified_diff` /
//! `summary` shape, so the CLI never reimplements diffing.

#[cfg(any(feature = "hostlib", test))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(feature = "hostlib")]
use std::sync::Arc;

use serde::Serialize;
#[cfg(any(feature = "hostlib", test))]
use serde_json::Value as JsonValue;

#[cfg(feature = "hostlib")]
use harn_lexer::Lexer;
#[cfg(feature = "hostlib")]
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

/// One file the counterfactual plan would touch — the unit of divergence.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DivergedFile {
    pub path: String,
    /// `created`, `modified`, or `deleted` — derived from the dry-run line
    /// deltas the same way `edit.dry_run` classifies a change.
    pub status: String,
    pub lines_added: u64,
    pub lines_removed: u64,
}

/// Structured divergence summary stitched into the replay envelope under
/// `counterfactual`. Mirrors the `edit.dry_run` roll-up so callers can
/// reconcile the two without a second lookup.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CounterfactualReport {
    /// Absolute or workspace-relative path to the evaluated plan.
    pub plan_path: String,
    /// Every plan in the counterfactual chain, in evaluation order.
    pub plan_paths: Vec<String>,
    /// Number of chained plan files evaluated.
    pub step_count: u64,
    /// `ok`, `partial`, or `no_ops_applied`, straight from `edit.dry_run`.
    pub result: String,
    /// The divergent file set — every file the plan's edits would touch.
    pub diverged: Vec<DivergedFile>,
    pub files_touched: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub ops_applied: u64,
    pub ops_rejected: u64,
}

#[cfg(any(feature = "hostlib", test))]
#[derive(Debug)]
enum CounterfactualPlan {
    EditOps(JsonValue),
    DryRun(JsonValue),
}

#[cfg(feature = "hostlib")]
struct CounterfactualSandbox {
    session_id: String,
    root: PathBuf,
}

#[cfg(feature = "hostlib")]
impl CounterfactualSandbox {
    fn enter(root: &Path) -> Result<Self, String> {
        let session_id = format!("harn-counterfactual-{}", uuid::Uuid::now_v7());
        harn_hostlib::fs::configure_session_root(&session_id, root);
        harn_hostlib::fs::set_mode(&session_id, harn_hostlib::fs::FsMode::Staged, Some(root))
            .map_err(|error| format!("failed to isolate counterfactual filesystem: {error}"))?;
        Ok(Self {
            session_id,
            root: root.to_path_buf(),
        })
    }
}

#[cfg(feature = "hostlib")]
impl Drop for CounterfactualSandbox {
    fn drop(&mut self) {
        let _ = harn_hostlib::fs::discard_staged(&self.session_id, &[]);
        let _ = harn_hostlib::fs::remove_session_state(&self.session_id, Some(&self.root));
    }
}

/// Evaluate `plan_paths` and return their cumulative divergence. `Err` carries a
/// human-readable message suitable for both the `error.message` JSON field
/// and the human `error:` line.
#[cfg(feature = "hostlib")]
pub(crate) fn evaluate(plan_paths: &[PathBuf]) -> Result<CounterfactualReport, String> {
    if plan_paths.is_empty() {
        return Err("at least one --counterfactual plan is required".to_string());
    }

    let mut chained_ops = Vec::new();
    let mut single_dry_run = None;
    for plan_path in plan_paths {
        let source = std::fs::read_to_string(plan_path).map_err(|error| {
            format!(
                "failed to read counterfactual plan {}: {error}",
                plan_path.display()
            )
        })?;
        match normalize_plan_value(run_plan_source(&source, plan_path)?, plan_path)? {
            CounterfactualPlan::EditOps(JsonValue::Array(ops)) => chained_ops.extend(ops),
            CounterfactualPlan::EditOps(other) => {
                return Err(format!(
                    "counterfactual plan {} must return a list of edit ops, got {}",
                    plan_path.display(),
                    json_type_name(&other)
                ));
            }
            CounterfactualPlan::DryRun(dry_run) if plan_paths.len() == 1 => {
                single_dry_run = Some(dry_run);
            }
            CounterfactualPlan::DryRun(_) => {
                return Err(format!(
                    "counterfactual plan {} returned an edit_dry_run result, which cannot be \
                     chained; return the raw edit-op list instead",
                    plan_path.display()
                ));
            }
        }
    }

    let dry_run = match single_dry_run {
        Some(dry_run) => dry_run,
        None => run_edit_dry_run(JsonValue::Array(chained_ops), &plan_paths[0])?,
    };
    project_divergence(&dry_run, plan_paths)
}

#[cfg(not(feature = "hostlib"))]
pub(crate) fn evaluate(_plan_paths: &[PathBuf]) -> Result<CounterfactualReport, String> {
    Err("`harn replay --counterfactual` requires the `hostlib` feature (default-on); it is unavailable in this build".to_string())
}

/// Compile and execute `plan.harn`, returning its final value as JSON. The
/// VM is wired exactly like `harn run`'s — stdlib plus the default hostlib —
/// so `edit_dry_run` and the staged-fs overlay it relies on are available.
#[cfg(feature = "hostlib")]
fn run_plan_source(source: &str, plan_path: &Path) -> Result<JsonValue, String> {
    let source = source.to_string();
    let plan_path = plan_path.to_path_buf();
    std::thread::Builder::new()
        .name("harn-counterfactual-plan".to_string())
        .spawn(move || run_plan_source_inner(&source, &plan_path))
        .map_err(|error| format!("failed to start counterfactual plan runner: {error}"))?
        .join()
        .map_err(|_| "counterfactual plan runner panicked".to_string())?
}

#[cfg(feature = "hostlib")]
fn run_plan_source_inner(source: &str, plan_path: &Path) -> Result<JsonValue, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|error| format!("counterfactual plan lex error: {error}"))?;
    let mut parser = Parser::new(tokens);
    let program = parser
        .parse()
        .map_err(|error| format!("counterfactual plan parse error: {error}"))?;

    let checker =
        crate::typecheck_imports::checker_with_resolved_imports(TypeChecker::new(), plan_path);
    for diag in checker.check(&program) {
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            return Err(format!("counterfactual plan type error: {}", diag.message));
        }
    }

    let chunk = crate::typecheck_imports::compiler_with_resolved_imports(
        harn_vm::Compiler::new(),
        plan_path,
    )
    .compile(&program)
    .map_err(|error| format!("counterfactual plan compile error: {error}"))?;

    let source_parent = plan_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let project_root = harn_vm::stdlib::process::find_project_root(&source_parent);
    let store_base = project_root
        .clone()
        .unwrap_or_else(|| source_parent.clone());
    let overlay = Arc::new(harn_vm::testbench::overlay_fs::OverlayFs::rooted_at(
        &store_base,
    ));

    let local = tokio::task::LocalSet::new();
    futures::executor::block_on(local.run_until(async move {
        let _overlay_guard = harn_vm::testbench::overlay_fs::install_overlay(overlay);
        let sandbox = CounterfactualSandbox::enter(&store_base)?;
        let _session_guard =
            harn_vm::agent_sessions::enter_current_session(sandbox.session_id.clone());
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        crate::install_default_hostlib(&mut vm);
        harn_vm::register_store_builtins(&mut vm, &store_base);
        harn_vm::register_metadata_builtins(&mut vm, &store_base);
        let pipeline_name = plan_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("counterfactual");
        harn_vm::register_checkpoint_builtins(&mut vm, &store_base, pipeline_name);
        vm.set_source_info(&plan_path.to_string_lossy(), source);
        if let Some(root) = project_root.as_ref() {
            vm.set_project_root(root);
        }
        vm.set_source_dir(&source_parent);
        vm.set_harness(harn_vm::Harness::real());
        let value = vm
            .execute(&chunk)
            .await
            .map_err(|error| format!("counterfactual plan runtime error: {error}"))?;
        Ok(harn_vm::llm::vm_value_to_json(&value))
    }))
}

/// Normalize the plan program's final value into either raw edit ops or an
/// already-rendered `edit_dry_run` result.
#[cfg(any(feature = "hostlib", test))]
fn normalize_plan_value(value: JsonValue, plan_path: &Path) -> Result<CounterfactualPlan, String> {
    match value {
        JsonValue::Array(_) => Ok(CounterfactualPlan::EditOps(value)),
        JsonValue::Object(ref map) if map.contains_key("per_file_unified_diff") => {
            Ok(CounterfactualPlan::DryRun(value))
        }
        JsonValue::Object(ref map) if map.contains_key("plan") => {
            Ok(CounterfactualPlan::EditOps(map["plan"].clone()))
        }
        other => Err(format!(
            "counterfactual plan {} must `return` an edit-op list or an edit_dry_run result, \
             got {} (a bare trailing expression returns nil in Harn — use `return`)",
            plan_path.display(),
            json_type_name(&other)
        )),
    }
}

/// Run a raw edit plan through `std/edit::edit_dry_run` (which opens and
/// discards a transient staged-fs overlay) and return its result JSON.
///
/// The plan is a JSON value (lists/dicts/strings/numbers/bools), which is a
/// subset of Harn's dict/list literal syntax — JSON-style quoted keys parse
/// fine — so we embed it directly into a tiny driver program and run it
/// through the same VM path as a plan that called `edit_dry_run` itself.
/// This keeps a single execution path and avoids round-tripping the plan
/// through a host global.
///
/// The driver is written to a temp `.harn` file beside the plan so the
/// cross-module typechecker resolves the `std/edit` import the same way it
/// does for an on-disk plan (the import graph is keyed by file path).
#[cfg(feature = "hostlib")]
fn run_edit_dry_run(plan: JsonValue, plan_path: &Path) -> Result<JsonValue, String> {
    let plan_literal = serde_json::to_string(&plan)
        .map_err(|error| format!("failed to serialize counterfactual plan: {error}"))?;
    let driver = format!(
        "import {{ edit_dry_run }} from \"std/edit\"\nreturn edit_dry_run({{plan: {plan_literal}}})\n"
    );

    let dir = plan_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let driver_file = tempfile::Builder::new()
        .prefix(".harn-counterfactual-driver-")
        .suffix(".harn")
        .tempfile_in(&dir)
        .map_err(|error| format!("failed to stage counterfactual dry-run driver: {error}"))?;
    std::fs::write(driver_file.path(), &driver)
        .map_err(|error| format!("failed to write counterfactual dry-run driver: {error}"))?;

    run_plan_source(&driver, driver_file.path())
}

/// Read the divergent file set off the `edit.dry_run` result. The status of
/// each file is classified from its line deltas exactly the way
/// `edit.dry_run` itself does (pure additions → `created`, pure removals →
/// `deleted`, both → `modified`).
#[cfg(any(feature = "hostlib", test))]
fn project_divergence(
    dry_run: &JsonValue,
    plan_paths: &[PathBuf],
) -> Result<CounterfactualReport, String> {
    let summary = dry_run.get("summary").cloned().unwrap_or(JsonValue::Null);
    let mut diverged = Vec::new();
    if let Some(entries) = dry_run
        .get("per_file_unified_diff")
        .and_then(JsonValue::as_array)
    {
        for entry in entries {
            let path = entry
                .get("path")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            let lines_added = entry
                .get("lines_added")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let lines_removed = entry
                .get("lines_removed")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let status = if lines_removed == 0 && lines_added > 0 {
                "created"
            } else if lines_added == 0 && lines_removed > 0 {
                "deleted"
            } else {
                "modified"
            };
            diverged.push(DivergedFile {
                path,
                status: status.to_string(),
                lines_added,
                lines_removed,
            });
        }
    }

    let plan_path_labels = plan_paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Ok(CounterfactualReport {
        plan_path: plan_path_labels.join(" -> "),
        plan_paths: plan_path_labels,
        step_count: plan_paths.len() as u64,
        result: dry_run
            .get("result")
            .and_then(JsonValue::as_str)
            .unwrap_or("no_ops_applied")
            .to_string(),
        diverged,
        files_touched: summary
            .get("files_touched")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        lines_added: summary
            .get("lines_added")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        lines_removed: summary
            .get("lines_removed")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        ops_applied: summary
            .get("ops_applied")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
        ops_rejected: summary
            .get("ops_rejected")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0),
    })
}

#[cfg(any(feature = "hostlib", test))]
fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "nil",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "list",
        JsonValue::Object(_) => "dict",
    }
}

/// Render the divergence for the human (non-`--json`) replay path.
pub(crate) fn print_human(report: &CounterfactualReport) {
    println!("Counterfactual: {} ({})", report.plan_path, report.result);
    if report.diverged.is_empty() {
        println!("  no files would diverge from the recorded outcome.");
    } else {
        println!(
            "  would touch {} file(s) (+{} / -{} lines, {} op(s) applied, {} rejected):",
            report.files_touched,
            report.lines_added,
            report.lines_removed,
            report.ops_applied,
            report.ops_rejected,
        );
        for file in &report.diverged {
            println!(
                "    {} {} (+{} / -{})",
                file.status, file.path, file.lines_added, file.lines_removed
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan_path() -> &'static Path {
        Path::new("/tmp/what-if.harn")
    }

    fn plan_paths() -> Vec<PathBuf> {
        vec![plan_path().to_path_buf()]
    }

    #[test]
    fn projects_diverged_files_and_classifies_status_by_line_deltas() {
        let dry_run = json!({
            "result": "ok",
            "per_file_unified_diff": [
                {"path": "a.rs", "diff": "...", "lines_added": 3, "lines_removed": 0},
                {"path": "b.rs", "diff": "...", "lines_added": 0, "lines_removed": 4},
                {"path": "c.rs", "diff": "...", "lines_added": 2, "lines_removed": 2},
            ],
            "summary": {
                "files_touched": 3,
                "lines_added": 5,
                "lines_removed": 6,
                "ops_applied": 3,
                "ops_rejected": 0,
            },
        });
        let report = project_divergence(&dry_run, &plan_paths()).expect("project");
        assert_eq!(report.result, "ok");
        assert_eq!(report.step_count, 1);
        assert_eq!(report.plan_paths, vec!["/tmp/what-if.harn"]);
        assert_eq!(report.files_touched, 3);
        assert_eq!(report.lines_added, 5);
        assert_eq!(report.lines_removed, 6);
        assert_eq!(report.ops_applied, 3);
        assert_eq!(report.diverged.len(), 3);
        assert_eq!(report.diverged[0].status, "created");
        assert_eq!(report.diverged[1].status, "deleted");
        assert_eq!(report.diverged[2].status, "modified");
    }

    #[test]
    fn empty_dry_run_projects_to_no_divergence() {
        let dry_run = json!({
            "result": "no_ops_applied",
            "per_file_unified_diff": [],
            "summary": {"files_touched": 0, "lines_added": 0, "lines_removed": 0, "ops_applied": 0, "ops_rejected": 0},
        });
        let report = project_divergence(&dry_run, &plan_paths()).expect("project");
        assert!(report.diverged.is_empty());
        assert_eq!(report.result, "no_ops_applied");
    }

    #[test]
    fn normalize_plan_value_rejects_a_nil_plan_with_a_return_hint() {
        let error = normalize_plan_value(JsonValue::Null, plan_path()).unwrap_err();
        assert!(error.contains("got nil"), "error: {error}");
        assert!(
            error.contains("return"),
            "error should hint at `return`: {error}"
        );
    }

    #[test]
    fn normalize_plan_value_passes_through_an_existing_dry_run_result() {
        let dry_run = json!({
            "result": "ok",
            "per_file_unified_diff": [{"path": "x.rs", "diff": "...", "lines_added": 1, "lines_removed": 0}],
            "summary": {"files_touched": 1},
        });
        let passed = normalize_plan_value(dry_run.clone(), plan_path()).expect("passthrough");
        match passed {
            CounterfactualPlan::DryRun(value) => assert_eq!(value, dry_run),
            CounterfactualPlan::EditOps(_) => panic!("expected dry-run result"),
        }
    }

    #[test]
    fn normalize_plan_value_reads_plan_field_as_raw_edit_ops() {
        let value = normalize_plan_value(json!({"plan": [{"op": "safe_text_patch"}]}), plan_path())
            .expect("normalize");
        match value {
            CounterfactualPlan::EditOps(JsonValue::Array(ops)) => assert_eq!(ops.len(), 1),
            CounterfactualPlan::EditOps(other) => panic!("expected list, got {other:?}"),
            CounterfactualPlan::DryRun(_) => panic!("expected raw edit ops"),
        }
    }
}
