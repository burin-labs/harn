//! `harn eval context` — deterministic context-engineering mode runner.
//!
//! ## .harn dispatch
//!
//! The **evaluation pipeline** (manifest load, `evaluate_context_eval_manifest`
//! invocation, per-run scoring) stays in Rust — it reaches into
//! `harn_vm::orchestration::context_eval` internals (mode runs,
//! projection policies, scoring) that aren't reachable from script-land
//! today without G4 (#2297) exposing the orchestration surface.
//!
//! The **rendering layer** (the markdown body of `summary.md`, the
//! one-line human summary, the `--json` pretty form) is delegated to
//! `crates/harn-stdlib/src/stdlib/cli/eval/context.harn`. The Rust shim
//! pre-serialises the `ContextEvalReport` to JSON, forwards it via
//! [`CONTEXT_REPORT_ENV`], dispatches three times (markdown for
//! `summary.md`, summary for stdout, optional JSON for stdout when
//! `--json` is set), and writes the captured payloads to disk / real
//! stdout. The artifacts that need byte-identical serde output
//! (`summary.json`, `per_run.jsonl`) stay on the Rust side because
//! Harn's `json_stringify_pretty` sorts dict keys alphabetically and
//! the on-disk format is consumed by the regression-check / hosted
//! ingestion paths that depend on the serde struct-field order.
//!
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    context_eval_default_output_dir, context_eval_report_v1, evaluate_context_eval_manifest_v2,
    load_context_eval_manifest, ContextEvalReportV2,
};
use serde::Serialize;

use crate::cli::EvalContextArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var the embedded `cli/eval/context` script reads to pick up the
/// pre-serialised `ContextEvalReport`. The Rust shim does all the
/// pipeline work and hands the script the assembled report so it only
/// has to format it.
const CONTEXT_REPORT_ENV: &str = "HARN_EVAL_CONTEXT_REPORT_JSON";

/// Env var the script reads to select between the three rendering
/// modes (`"markdown"` for `summary.md`, `"summary"` for the one-line
/// stdout summary, `"json"` for the `--json` pretty form).
const CONTEXT_OUTPUT_MODE_ENV: &str = "HARN_EVAL_CONTEXT_OUTPUT_MODE";

/// Serialises the dispatch-render path so concurrent in-process callers
/// (the existing `eval_context_cli` integration tests, plus any future
/// fanout caller) don't race on the process-global env vars the Rust
/// shim sets to hand the report off to the .harn script. The CLI binary
/// itself is single-call, so this mutex is uncontended in production;
/// in tests it serialises the dispatch window only — aggregation still
/// runs freely.
///
/// Mirrors the pattern W5's `eval_prompt.rs` uses (see harn#2305) so
/// the cross-script env-var hand-off stays consistent across the eval
/// cluster.
static DISPATCH_RENDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn run(args: EvalContextArgs) -> i32 {
    if !matches!(args.report_version, 1 | 2) {
        cli_diagnostic!("error: context eval report version must be 1 or 2");
        return 1;
    }
    let report = match aggregate_report(&args) {
        Ok(report) => report,
        Err(code) => return code,
    };

    let output_dir = args.output.unwrap_or_else(context_eval_default_output_dir);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        cli_diagnostic!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }

    let legacy = if args.report_version == 1 {
        cli_diagnostic!("warning: context eval report v1 is deprecated and substitutes zero for missing costs; use v2 for cost coverage");
        match context_eval_report_v1(&report) {
            Ok(value) => Some(value),
            Err(error) => {
                cli_diagnostic!("error: failed to project context eval report v1: {error}");
                return 1;
            }
        }
    } else {
        None
    };

    // The JSON artifacts (summary.json, per_run.jsonl) always stay on the
    // serde-driven Rust path — see module docstring for the byte-format
    // rationale. They write before any rendering so a render failure
    // doesn't leave a partially-written report directory.
    let result = if let Some(value) = &legacy {
        write_json_artifacts(&output_dir, value, &value.runs)
    } else {
        write_json_artifacts(&output_dir, &report, &report.runs)
    };
    if let Err(error) = result {
        cli_diagnostic!("error: failed to write context eval outputs: {error}");
        return 1;
    }

    // Human-readable output always uses the truthful v2 cost coverage.
    match write_markdown_dispatch(&output_dir, &report).await {
        Ok(()) => {}
        Err(code) => return code,
    }
    announce_output_paths(&output_dir);
    if args.json {
        let result = if let Some(value) = &legacy {
            print_json_dispatch(value).await
        } else {
            print_json_dispatch(&report).await
        };
        if let Err(code) = result {
            return code;
        }
    } else if let Err(code) = print_summary_dispatch(&report).await {
        return code;
    }
    post_render_exit_code(&report)
}

/// Build the aggregated [`ContextEvalReportV2`] without any rendering.
fn aggregate_report(args: &EvalContextArgs) -> Result<ContextEvalReportV2, i32> {
    let manifest = match load_context_eval_manifest(&args.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            cli_diagnostic!("error: {error}");
            return Err(1);
        }
    };
    let report = match evaluate_context_eval_manifest_v2(&manifest) {
        Ok(report) => report,
        Err(error) => {
            cli_diagnostic!("error: {error}");
            return Err(1);
        }
    };
    Ok(report)
}

fn post_render_exit_code(report: &ContextEvalReportV2) -> i32 {
    i32::from(!report.pass)
}

fn announce_output_paths(output_dir: &Path) {
    eprintln!(
        "wrote {}, {}, and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_run.jsonl").display(),
        output_dir.join("summary.md").display()
    );
}

fn write_json_artifacts<T: Serialize, R: Serialize>(
    output_dir: &Path,
    report: &T,
    runs: &[R],
) -> Result<(), String> {
    write_json(output_dir.join("summary.json"), report)?;
    write_jsonl(output_dir.join("per_run.jsonl"), runs)
}

fn write_json<T: Serialize>(path: PathBuf, report: &T) -> Result<(), String> {
    let payload = serde_json::to_string_pretty(report).map_err(|error| error.to_string())?;
    fs::write(path, payload).map_err(|error| error.to_string())
}

fn write_jsonl<T: Serialize>(path: PathBuf, runs: &[T]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    for run in runs {
        let line = serde_json::to_string(run).map_err(|error| error.to_string())?;
        file.write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}

// ─── Dispatch (.harn) render path ────────────────────────────────────────

async fn write_markdown_dispatch(
    output_dir: &Path,
    report: &ContextEvalReportV2,
) -> Result<(), i32> {
    let payload = render_via_dispatch(report, "markdown").await?;
    if let Err(error) = fs::write(output_dir.join("summary.md"), payload) {
        cli_diagnostic!("error: failed to write context eval markdown: {error}");
        return Err(1);
    }
    Ok(())
}

async fn print_summary_dispatch(report: &ContextEvalReportV2) -> Result<(), i32> {
    let payload = render_via_dispatch(report, "summary").await?;
    print!("{payload}");
    if !payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

async fn print_json_dispatch<T: Serialize>(report: &T) -> Result<(), i32> {
    let payload = render_via_dispatch(report, "json").await?;
    print!("{payload}");
    if !payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Dispatch to the embedded `cli/eval/context.harn` script for one of
/// the three rendering modes (markdown / summary / json). Returns the
/// captured stdout on success, or a propagated exit code on failure.
///
/// **Concurrency.** Held under [`DISPATCH_RENDER_LOCK`] so concurrent
/// in-process callers don't race on the global env vars the Rust shim
/// sets to hand the report to the script. See the lock's docstring for
/// the trade-off rationale.
async fn render_via_dispatch<T: Serialize>(report: &T, mode: &str) -> Result<String, i32> {
    let report_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => {
            cli_diagnostic!("error: failed to serialise ContextEvalReport for dispatch: {error}");
            return Err(1);
        }
    };

    let _guard = DISPATCH_RENDER_LOCK.lock().await;
    let _report = ScopedEnvVar::set(CONTEXT_REPORT_ENV, &report_json);
    let _mode = ScopedEnvVar::set(CONTEXT_OUTPUT_MODE_ENV, mode);

    let outcome = dispatch::run_embedded_script("eval/context", Vec::new(), false).await;
    if !outcome.stderr.is_empty() {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if outcome.exit_code != 0 {
        return Err(outcome.exit_code);
    }
    Ok(outcome.stdout)
}
