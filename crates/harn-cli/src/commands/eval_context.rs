//! `harn eval context` — deterministic context-engineering mode runner.
//!
//! ## .harn dispatch (W6 partial port — see harn#2306)
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
//! `HARN_CLI_IMPL=rust` keeps the legacy direct-render path for the
//! parity-snapshot harness (#2299) until the C1 ratchet (#2314) lands.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    context_eval_default_output_dir, evaluate_context_eval_manifest, load_context_eval_manifest,
    ContextEvalReport, ContextEvalRunReport,
};

use crate::cli::EvalContextArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::format::escape_md;

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
    let report = match aggregate_report(&args) {
        Ok(report) => report,
        Err(code) => return code,
    };

    let output_dir = args.output.unwrap_or_else(context_eval_default_output_dir);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        eprintln!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }

    // The JSON artifacts (summary.json, per_run.jsonl) always stay on the
    // serde-driven Rust path — see module docstring for the byte-format
    // rationale. They write before any rendering so a render failure
    // doesn't leave a partially-written report directory.
    if let Err(error) = write_json_artifacts(&output_dir, &report) {
        eprintln!("error: failed to write context eval outputs: {error}");
        return 1;
    }

    // `HARN_CLI_IMPL=rust` keeps the legacy direct-render path so the
    // parity-snapshot harness (#2299) can compare both impls byte-for-byte
    // until C1 (#2314) deletes it.
    let use_legacy = std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust");

    if use_legacy {
        if let Err(error) = write_markdown_legacy(&output_dir, &report) {
            eprintln!("error: failed to write context eval markdown: {error}");
            return 1;
        }
        announce_output_paths(&output_dir);
        if args.json {
            if let Err(code) = print_json_legacy(&report) {
                return code;
            }
        } else {
            print_summary_legacy(&report);
        }
        return post_render_exit_code(&report);
    }

    match write_markdown_dispatch(&output_dir, &report).await {
        Ok(()) => {}
        Err(code) => return code,
    }
    announce_output_paths(&output_dir);
    if args.json {
        if let Err(code) = print_json_dispatch(&report).await {
            return code;
        }
    } else if let Err(code) = print_summary_dispatch(&report).await {
        return code;
    }
    post_render_exit_code(&report)
}

/// Build the aggregated [`ContextEvalReport`] without any rendering.
/// Pulled out of [`run`] so both the legacy direct-render path and the
/// .harn dispatch path see the same report.
fn aggregate_report(args: &EvalContextArgs) -> Result<ContextEvalReport, i32> {
    let manifest = match load_context_eval_manifest(&args.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(1);
        }
    };
    let report = match evaluate_context_eval_manifest(&manifest) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(1);
        }
    };
    Ok(report)
}

fn post_render_exit_code(report: &ContextEvalReport) -> i32 {
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

fn write_json_artifacts(output_dir: &Path, report: &ContextEvalReport) -> Result<(), String> {
    write_json(output_dir.join("summary.json"), report)?;
    write_jsonl(output_dir.join("per_run.jsonl"), &report.runs)
}

fn write_json(path: PathBuf, report: &ContextEvalReport) -> Result<(), String> {
    let payload = serde_json::to_string_pretty(report).map_err(|error| error.to_string())?;
    fs::write(path, payload).map_err(|error| error.to_string())
}

fn write_jsonl(path: PathBuf, runs: &[ContextEvalRunReport]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    for run in runs {
        let line = serde_json::to_string(run).map_err(|error| error.to_string())?;
        file.write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}

// ─── Legacy direct-render path (gated by HARN_CLI_IMPL=rust) ────────────

fn write_markdown_legacy(output_dir: &Path, report: &ContextEvalReport) -> Result<(), String> {
    fs::write(
        output_dir.join("summary.md"),
        legacy_render_markdown(report),
    )
    .map_err(|error| error.to_string())
}

fn print_json_legacy(report: &ContextEvalReport) -> Result<(), i32> {
    match serde_json::to_string_pretty(report) {
        Ok(payload) => {
            println!("{payload}");
            Ok(())
        }
        Err(error) => {
            eprintln!("error: failed to serialize context eval summary: {error}");
            Err(1)
        }
    }
}

fn print_summary_legacy(report: &ContextEvalReport) {
    println!(
        "context eval: {}/{} passed, mean_correctness={:.2}, mean_tool_quality={:.2}",
        report.passed_runs,
        report.total_runs,
        report.aggregate.mean_final_correctness,
        report.aggregate.mean_tool_call_quality
    );
}

fn legacy_render_markdown(report: &ContextEvalReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Context Eval: {}\n\n",
        report
            .manifest_name
            .as_deref()
            .unwrap_or(report.manifest_id.as_str())
    ));
    out.push_str(&format!(
        "- status: {}\n- runs: {}/{} passed\n- mean correctness: {:.4}\n- mean tool quality: {:.4}\n- input tokens: {}\n- output tokens: {}\n- cost USD: {:.6}\n\n",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed_runs,
        report.total_runs,
        report.aggregate.mean_final_correctness,
        report.aggregate.mean_tool_call_quality,
        report.aggregate.total_input_tokens,
        report.aggregate.total_output_tokens,
        report.aggregate.total_cost_usd,
    ));
    out.push_str("| task | mode | pass | correctness | tools | reads before edit | input tokens | compactions | cache key |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---|\n");
    for run in &report.runs {
        out.push_str(&format!(
            "| {} | {} | {} | {:.4} | {:.4} | {} | {} | {} | `{}` |\n",
            escape_md(&run.task_id),
            escape_md(&run.mode_id),
            if run.passed { "yes" } else { "no" },
            run.final_correctness.score,
            run.tool_call_quality.score,
            run.reads_before_first_edit,
            run.input_tokens,
            run.compaction_count,
            run.cache.key,
        ));
    }
    out
}

// ─── Dispatch (.harn) render path ────────────────────────────────────────

async fn write_markdown_dispatch(output_dir: &Path, report: &ContextEvalReport) -> Result<(), i32> {
    let payload = render_via_dispatch(report, "markdown").await?;
    if let Err(error) = fs::write(output_dir.join("summary.md"), payload) {
        eprintln!("error: failed to write context eval markdown: {error}");
        return Err(1);
    }
    Ok(())
}

async fn print_summary_dispatch(report: &ContextEvalReport) -> Result<(), i32> {
    let payload = render_via_dispatch(report, "summary").await?;
    print!("{payload}");
    // The script emits exactly the legacy summary line (no trailing
    // newline); add one to match the legacy `println!` semantics.
    if !payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

async fn print_json_dispatch(report: &ContextEvalReport) -> Result<(), i32> {
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
async fn render_via_dispatch(report: &ContextEvalReport, mode: &str) -> Result<String, i32> {
    let report_json = match serde_json::to_string(report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise ContextEvalReport for dispatch: {error}");
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
