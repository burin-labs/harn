//! `harn eval coding-agent` - empirical preset/provider benchmark for a
//! small coding-agent fixture suite.
//!
//! ## Dispatch boundary
//!
//! The **matrix execution pipeline** (fixture resolution, model
//! discovery, per-cell `execute_run` invocation, Ollama snapshot/
//! cleanup, scoring, rollups, native/text comparisons, follow-up
//! generation, baseline diff) stays in Rust. Every cell drives the
//! embedded `coding_agent_suite.harn` driver through `execute_run`,
//! which itself reaches into VM internals (`commands::run`,
//! `harn_vm::llm`, `commands::local::runtime`) that are not exposed as
//! script capabilities.
//!
//! The **rendering layer** (the `summary.md` body, the `followups.md`
//! body, the one-line human stdout summary, the `--json` pretty form)
//! is delegated to
//! `crates/harn-stdlib/src/stdlib/cli/eval/coding_agent.harn`. The
//! Rust shim pre-serialises the assembled `EvalSummary` to JSON,
//! forwards it via [`CODING_AGENT_SUMMARY_ENV`], dispatches four
//! times (markdown for `summary.md`, followups for `followups.md`,
//! then either the summary line or the `--json` pretty form for
//! stdout), and writes the captured payloads to disk / real stdout.
//!
//! The on-disk JSON artifacts (`summary.json`, `per_run.jsonl`,
//! `local_readiness.json`) stay on the serde-driven Rust path because
//! Harn's `json_stringify_pretty` sorts dict keys alphabetically and
//! the on-disk format is consumed by the experiment driver in
//! `experiments/step-judge/run.sh`, the local-readiness regression
//! check, and hosted ingestion — all of which depend on the serde
//! struct-field byte order.
//!
//! `HARN_CLI_IMPL=rust` keeps the direct-render path available for
//! parity snapshot coverage.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::clock::{Clock, RealClock};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::cli::EvalCodingAgentArgs;
use crate::commands::eval_coding_agent_preset::{
    resolve_step_judge_json, resolve_structural_validator_json,
};
use crate::commands::eval_model_selector::{
    resolve_selector, selector_is_local, selector_label, ModelSelector,
};
use crate::commands::local::runtime::{
    local_provider_ids, ollama_unload_model, snapshot_provider, LocalProviderSnapshot,
};
use crate::commands::local_readiness;
use crate::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunProfileOptions, RunSandboxOptions,
};
use crate::commands::tool_mode_parity::{
    self, ToolModeParityFixtureInput, ToolModeParityPairSummary, TOOL_MODE_PARITY_DIRECTORY,
    TOOL_MODE_PARITY_FIXTURE_SUITE, TOOL_MODE_PARITY_OVERLAY_FILENAME,
};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var the embedded `cli/eval/coding_agent` script reads to pick
/// up the pre-serialised [`EvalSummary`]. The Rust shim does all the
/// matrix execution and scoring and hands the script the assembled
/// summary so it only has to format it.
const CODING_AGENT_SUMMARY_ENV: &str = "HARN_EVAL_CODING_AGENT_SUMMARY_JSON";

/// Env var the script reads to pick the rendering mode — one of
/// `"markdown"` (summary.md body), `"followups"` (followups.md body),
/// `"summary"` (one-line stdout summary), or `"json"` (--json pretty
/// form). Defaulted to `"summary"` if unset so the script stays robust
/// against future Rust-side bugs.
const CODING_AGENT_MODE_ENV: &str = "HARN_EVAL_CODING_AGENT_MODE";

/// Serialises the dispatch-render path so concurrent in-process
/// callers (the existing `eval_coding_agent_cli` integration test plus
/// any future fanout caller) don't race on the global env vars the
/// Rust shim sets to hand the report off to the .harn script. The CLI
/// binary itself is single-call, so this mutex is uncontended in
/// production; in tests it serialises the dispatch window only —
/// matrix execution still parallelises freely.
///
/// Matches the other eval render shims so the cross-script env-var handoff
/// stays consistent across the eval cluster.
static DISPATCH_RENDER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const CODING_AGENT_SUITE_HARN: &str = include_str!("../../assets/evals/coding_agent_suite.harn");
const TOOL_FORMAT_OVERRIDE_WARNING_PREFIX: &str = "warning: tool_format override:";

#[derive(Debug, Clone, Copy)]
struct FixtureDefinition {
    id: &'static str,
    name: &'static str,
    tool_sequence: &'static str,
    description: &'static str,
}

static FIXTURE_DEFINITIONS: &[FixtureDefinition] = &[
    FixtureDefinition {
        id: "python-add",
        name: "Python add repair",
        tool_sequence: "multi-tool",
        description: "One-file Python bug fix verified by unittest output.",
    },
    FixtureDefinition {
        id: "cli-help-flag",
        name: "CLI help flag",
        tool_sequence: "multi-tool",
        description: "Add a tiny CLI flag, update help-facing docs, and verify behavior.",
    },
    FixtureDefinition {
        id: "test-output-first",
        name: "Test-output-first repair",
        tool_sequence: "multi-tool",
        description: "Run a failing test first, then edit the implementation and re-run it.",
    },
    FixtureDefinition {
        id: "docs-symbol-rename",
        name: "Docs symbol rename",
        tool_sequence: "multi-tool",
        description:
            "Update docs and an example after a symbol rename without touching implementation.",
    },
    FixtureDefinition {
        id: "read-only-audit",
        name: "Read-only audit",
        tool_sequence: "one-tool",
        description: "Inspect a file and report that no edits are needed.",
    },
    FixtureDefinition {
        id: "no-tool-diagnosis",
        name: "No-tool diagnosis",
        tool_sequence: "no-tool",
        description: "Answer from prompt-only context without any tools.",
    },
];

#[derive(Debug, Clone, Serialize)]
struct LoadedEnvKey {
    key: String,
    source: String,
}

#[derive(Debug)]
struct EnvOverlay {
    previous: Vec<(OsString, Option<OsString>)>,
}

impl Drop for EnvOverlay {
    fn drop(&mut self) {
        for (key, previous) in self.previous.iter().rev() {
            if let Some(value) = previous {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RunReport {
    run_id: String,
    fixture_id: String,
    fixture_name: String,
    fixture_tool_sequence: String,
    selector: ModelSelector,
    tool_format: String,
    status: String,
    passed: bool,
    skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_reason: Option<String>,
    output_dir: String,
    transcript_events_path: String,
    workspace_root: Option<String>,
    elapsed_ms: u64,
    duration_ms: u64,
    iterations: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    pricing_known: bool,
    tool_calls: usize,
    rejected_tool_calls: usize,
    tool_sequence: Vec<String>,
    successful_tools: Vec<String>,
    transcript_event_count: usize,
    verification_success: bool,
    harn_exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_excerpt: Option<String>,
    local_cleanup: Option<LocalCleanupReport>,
}

#[derive(Debug, Clone, Serialize)]
struct LocalCleanupReport {
    provider: String,
    model: String,
    initially_loaded: bool,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FormatComparison {
    fixture_id: String,
    selector: ModelSelector,
    native_run_id: Option<String>,
    text_run_id: Option<String>,
    native_evidence_path: Option<String>,
    text_evidence_path: Option<String>,
    native_status: Option<String>,
    text_status: Option<String>,
    native_passed: Option<bool>,
    text_passed: Option<bool>,
    native_tool_call_count: Option<usize>,
    text_tool_call_count: Option<usize>,
    native_rejected_tool_call_count: Option<usize>,
    text_rejected_tool_call_count: Option<usize>,
    verifier_match: Option<bool>,
    tool_sequence_match: Option<bool>,
    rejected_tool_call_delta_text_minus_native: Option<i64>,
    token_delta_text_minus_native: Option<i64>,
    iteration_delta_text_minus_native: Option<i64>,
    equivalent: Option<bool>,
    divergence_reasons: Vec<String>,
    evidence_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FollowupSuggestion {
    title: String,
    body: String,
    labels: Vec<String>,
    run_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FixtureReport {
    id: String,
    name: String,
    tool_sequence: String,
    description: String,
}

#[derive(Debug, Clone, Serialize)]
struct RollupReport {
    key: String,
    total_runs: usize,
    passed_runs: usize,
    failed_runs: usize,
    skipped_runs: usize,
    total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
struct EvalRollups {
    by_fixture: Vec<RollupReport>,
    by_provider: Vec<RollupReport>,
    by_model: Vec<RollupReport>,
    by_tool_format: Vec<RollupReport>,
    by_tool_sequence: Vec<RollupReport>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalSummary {
    schema_version: u32,
    fixture_ids: Vec<String>,
    fixtures: Vec<FixtureReport>,
    output_dir: String,
    models: Vec<ModelSelector>,
    tool_formats: Vec<String>,
    env_keys_loaded: Vec<LoadedEnvKey>,
    total_runs: usize,
    passed_runs: usize,
    failed_runs: usize,
    skipped_runs: usize,
    diverged_comparisons: usize,
    total_cost_usd: f64,
    rollups: EvalRollups,
    runs: Vec<RunReport>,
    comparisons: Vec<FormatComparison>,
    parity_by_pair: Vec<ToolModeParityPairSummary>,
    followups: Vec<FollowupSuggestion>,
    /// Step-judge preset applied to all runs in this invocation, if any.
    /// Used by the experiment driver (experiments/step-judge/run.sh) to
    /// group repeat invocations into cells.
    #[serde(skip_serializing_if = "Option::is_none")]
    step_judge_preset: Option<String>,
    /// Free-form label for grouping repeat invocations (e.g.
    /// "replicate-1", "probe-rubric-adversarial"). Empty when unset.
    #[serde(skip_serializing_if = "String::is_empty")]
    run_label: String,
    /// Optional per-fixture diff against a prior run's `summary.json`,
    /// listing regressions (baseline passed, this cell failed) and
    /// recoveries (baseline failed, this cell passed) plus aggregate
    /// counts and a net lift in percentage points. Populated when the
    /// caller passes `--baseline-comparison-against <path>` (harn#2318).
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_comparison: Option<BaselineComparison>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct BaselineComparison {
    /// `output_dir` or `run_label` of the baseline summary, for context.
    baseline_label: String,
    /// Resolved path to the baseline `summary.json` that was diffed against.
    baseline_path: String,
    regressions: Vec<FixtureStatusDelta>,
    recoveries: Vec<FixtureStatusDelta>,
    /// Fixtures that passed in both runs.
    unchanged_passes: Vec<String>,
    /// Fixtures that failed in both runs.
    unchanged_failures: Vec<String>,
    /// Fixtures present in only one of the two runs (skipped from the
    /// diff but listed for visibility).
    missing_in_baseline: Vec<String>,
    missing_in_cell: Vec<String>,
    regressions_count: usize,
    recoveries_count: usize,
    /// `(recoveries_count - regressions_count) / total_fixtures_compared * 100`,
    /// rounded to one decimal place. Negative when the cell regresses more
    /// than it recovers.
    net_lift_pp: f64,
}

#[derive(Debug, Clone, Serialize)]
struct FixtureStatusDelta {
    fixture_id: String,
    baseline_status: String,
    cell_status: String,
}

struct LocalRunGuard {
    selector: ModelSelector,
    stop_after: bool,
    snapshot: Option<LocalProviderSnapshot>,
}

struct RunSummaryContext {
    run_id: String,
    fixture: FixtureDefinition,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    elapsed_ms: u64,
    exit_code: i32,
    stderr: String,
    local_cleanup: Option<LocalCleanupReport>,
}

pub async fn run(args: EvalCodingAgentArgs) -> i32 {
    let output_dir = args.output.clone().unwrap_or_else(default_output_dir);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        eprintln!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }

    let (_env_guard, env_keys_loaded) = match load_env_files(&args.env_files) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let fixtures = match resolve_fixtures(&args.fixtures) {
        Ok(fixtures) => fixtures,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let models = match resolve_models(&args).await {
        Ok(models) => models,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let tool_formats = match normalize_tool_formats(&args.tool_formats) {
        Ok(formats) => formats,
        Err(error) => {
            eprintln!("error: {error}");
            return 2;
        }
    };
    let matrix = build_matrix(&fixtures, &models, &tool_formats, args.max_runs);
    if matrix.is_empty() {
        eprintln!("error: no coding-agent benchmark runs selected");
        return 2;
    }

    let mut reports = Vec::new();
    let mut had_error = false;
    for (fixture, selector, tool_format) in matrix {
        let report = run_matrix_entry(&args, &output_dir, fixture, selector, tool_format).await;
        if !report.passed && !report.skipped {
            had_error = true;
        }
        if report.skipped && args.fail_on_unauthorized {
            had_error = true;
        }
        eprintln!(
            "{} {} {}: {}",
            report.fixture_id,
            selector_label(&report.selector),
            report.tool_format,
            report.status
        );
        reports.push(report);
    }

    let baseline_comparison = match &args.baseline_comparison_against {
        Some(path) => match load_baseline_comparison(path, &reports) {
            Ok(comparison) => Some(comparison),
            Err(error) => {
                eprintln!("error: --baseline-comparison-against: {error}");
                return 1;
            }
        },
        None => None,
    };
    let summary = build_summary(
        &output_dir,
        fixtures,
        models,
        tool_formats,
        env_keys_loaded,
        reports,
        args.step_judge
            .clone()
            .filter(|s| !s.is_empty() && s != "none"),
        args.run_label.clone(),
        baseline_comparison,
    );
    // The JSON artifacts (summary.json, per_run.jsonl,
    // local_readiness.json) always stay on the serde-driven Rust path —
    // see module docstring for the byte-format rationale. They write
    // before any rendering so a render failure doesn't leave a partially
    // written report directory.
    if let Err(error) = write_json_artifacts(&output_dir, &summary) {
        eprintln!("error: failed to write benchmark outputs: {error}");
        return 1;
    }

    // `HARN_CLI_IMPL=rust` keeps the legacy direct-render path so the
    // parity-snapshot harness (#2299) can compare both impls until C1
    // (#2314) deletes this escape hatch.
    let use_legacy = std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust");

    if use_legacy {
        if let Err(error) = write_markdown_artifacts_legacy(&output_dir, &summary) {
            eprintln!("error: {error}");
            return 1;
        }
        announce_output_paths(&output_dir);
        if args.json {
            print_json_legacy(&summary);
        } else {
            print_summary_legacy(&summary);
        }
        return i32::from(had_error);
    }

    if let Err(code) = write_markdown_artifacts_dispatch(&output_dir, &summary).await {
        return code;
    }
    announce_output_paths(&output_dir);
    if args.json {
        if let Err(code) = print_json_dispatch(&summary).await {
            return code;
        }
    } else if let Err(code) = print_summary_dispatch(&summary).await {
        return code;
    }

    i32::from(had_error)
}

async fn run_matrix_entry(
    args: &EvalCodingAgentArgs,
    output_dir: &Path,
    fixture: FixtureDefinition,
    selector: ModelSelector,
    tool_format: String,
) -> RunReport {
    let run_id = run_id_for(fixture, &selector, &tool_format);
    let run_dir = output_dir.join(&run_id);
    if let Err(error) = reset_dir(&run_dir) {
        return error_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            format!("failed to prepare run directory: {error}"),
        );
    }

    if !provider_available(&selector) {
        let reason = format!(
            "provider `{}` has no configured credentials",
            selector.provider
        );
        return skipped_report(run_id, fixture, selector, tool_format, run_dir, reason);
    }

    let script_path = run_dir.join("coding_agent_suite.harn");
    if let Err(error) = fs::write(&script_path, CODING_AGENT_SUITE_HARN) {
        return error_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            format!("failed to write benchmark harness: {error}"),
        );
    }

    let local_guard = LocalRunGuard::before(&selector, !args.keep_local_after_run).await;
    let argv = script_argv(args, fixture, &selector, &tool_format, &run_dir);
    let clock = RealClock::new();
    let started_ms = clock.monotonic_ms();
    let outcome = execute_run_with_sandbox_options(
        &script_path.to_string_lossy(),
        false,
        HashSet::new(),
        argv,
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
        RunSandboxOptions::default().with_workspace_root(run_dir.clone()),
    )
    .await;
    if let Some(line) = tool_format_override_warning_line(&outcome.stderr) {
        eprintln!("{line}");
    }
    let elapsed_ms = clock
        .monotonic_ms()
        .saturating_sub(started_ms)
        .try_into()
        .unwrap_or(0);
    let local_cleanup = if let Some(guard) = local_guard {
        guard.cleanup().await
    } else {
        None
    };

    let summary_value =
        read_run_summary(&run_dir).or_else(|| parse_last_json_line(&outcome.stdout));
    let Some(summary) = summary_value else {
        return RunReport {
            run_id,
            fixture_id: fixture.id.to_string(),
            fixture_name: fixture.name.to_string(),
            fixture_tool_sequence: fixture.tool_sequence.to_string(),
            selector,
            tool_format,
            status: "infra_error".to_string(),
            passed: false,
            skipped: false,
            skipped_reason: None,
            output_dir: run_dir.display().to_string(),
            transcript_events_path: run_dir
                .join("transcript_events.jsonl")
                .display()
                .to_string(),
            workspace_root: None,
            elapsed_ms,
            duration_ms: 0,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            pricing_known: false,
            tool_calls: 0,
            rejected_tool_calls: 0,
            tool_sequence: Vec::new(),
            successful_tools: Vec::new(),
            transcript_event_count: 0,
            verification_success: false,
            harn_exit_code: outcome.exit_code,
            error: Some("benchmark harness produced no summary JSON".to_string()),
            stderr_excerpt: excerpt(&outcome.stderr),
            local_cleanup,
        };
    };

    report_from_summary(
        RunSummaryContext {
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            elapsed_ms,
            exit_code: outcome.exit_code,
            stderr: outcome.stderr,
            local_cleanup,
        },
        summary,
    )
}

fn report_from_summary(ctx: RunSummaryContext, summary: JsonValue) -> RunReport {
    let passed = summary
        .get("passed")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
        && ctx.exit_code == 0;
    let input_tokens = summary
        .pointer("/llm/input_tokens")
        .and_then(JsonValue::as_i64)
        .unwrap_or(0);
    let output_tokens = summary
        .pointer("/llm/output_tokens")
        .and_then(JsonValue::as_i64)
        .unwrap_or(0);
    let pricing = harn_vm::llm::llm_pricing_per_1k(&ctx.selector.provider, &ctx.selector.model);
    let cost_usd = pricing
        .map(|(input, output)| {
            (input_tokens.max(0) as f64 * input + output_tokens.max(0) as f64 * output) / 1000.0
        })
        .unwrap_or(0.0);
    let status = if passed {
        "passed".to_string()
    } else if ctx.exit_code == 0 {
        "failed".to_string()
    } else {
        summary
            .get("status")
            .and_then(JsonValue::as_str)
            .unwrap_or("failed")
            .to_string()
    };
    RunReport {
        run_id: ctx.run_id,
        fixture_id: ctx.fixture.id.to_string(),
        fixture_name: ctx.fixture.name.to_string(),
        fixture_tool_sequence: ctx.fixture.tool_sequence.to_string(),
        selector: ctx.selector,
        tool_format: ctx.tool_format,
        status,
        passed,
        skipped: false,
        skipped_reason: None,
        output_dir: ctx.run_dir.display().to_string(),
        transcript_events_path: ctx
            .run_dir
            .join("transcript_events.jsonl")
            .display()
            .to_string(),
        workspace_root: summary
            .get("workspace_root")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        elapsed_ms: ctx.elapsed_ms,
        duration_ms: summary
            .get("duration_ms")
            .and_then(JsonValue::as_u64)
            .unwrap_or(ctx.elapsed_ms),
        iterations: summary
            .pointer("/llm/iterations")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0),
        input_tokens,
        output_tokens,
        cost_usd,
        pricing_known: pricing.is_some(),
        tool_calls: summary
            .pointer("/tools/calls")
            .and_then(JsonValue::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        rejected_tool_calls: summary
            .pointer("/tools/rejected")
            .and_then(JsonValue::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        tool_sequence: tool_call_sequence(summary.pointer("/tools/calls"))
            .or_else(|| non_empty_string_array(summary.pointer("/tools/successful")))
            .unwrap_or_default(),
        successful_tools: string_array(summary.pointer("/tools/successful")),
        transcript_event_count: summary
            .get("transcript_event_count")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0) as usize,
        verification_success: summary
            .pointer("/verification/success")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        harn_exit_code: ctx.exit_code,
        error: (!passed).then(|| {
            summary
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("benchmark failed")
                .to_string()
        }),
        stderr_excerpt: excerpt(&ctx.stderr),
        local_cleanup: ctx.local_cleanup,
    }
}

impl LocalRunGuard {
    async fn before(selector: &ModelSelector, stop_after: bool) -> Option<Self> {
        if !selector_is_local(selector) {
            return None;
        }
        let snapshot = snapshot_provider(&selector.provider, Path::new("."))
            .await
            .ok();
        Some(Self {
            selector: selector.clone(),
            stop_after,
            snapshot,
        })
    }

    async fn cleanup(self) -> Option<LocalCleanupReport> {
        let snapshot = self.snapshot?;
        if self.selector.provider != "ollama" {
            return Some(LocalCleanupReport {
                provider: self.selector.provider,
                model: self.selector.model,
                initially_loaded: false,
                action: "not_applicable".to_string(),
                detail: Some(
                    "non-Ollama local providers are only stopped when Harn launched a managed server"
                        .to_string(),
                ),
            });
        }
        let initially_loaded = snapshot
            .loaded_models
            .iter()
            .any(|loaded| loaded.name == self.selector.model);
        if !self.stop_after {
            return Some(LocalCleanupReport {
                provider: self.selector.provider,
                model: self.selector.model,
                initially_loaded,
                action: "left_running".to_string(),
                detail: Some("--keep-local-after-run".to_string()),
            });
        }
        if initially_loaded {
            return Some(LocalCleanupReport {
                provider: self.selector.provider,
                model: self.selector.model,
                initially_loaded,
                action: "left_preexisting".to_string(),
                detail: None,
            });
        }
        match ollama_unload_model(&snapshot.base_url, &self.selector.model).await {
            Ok(()) => Some(LocalCleanupReport {
                provider: self.selector.provider,
                model: self.selector.model,
                initially_loaded,
                action: "unloaded".to_string(),
                detail: None,
            }),
            Err(error) => Some(LocalCleanupReport {
                provider: self.selector.provider,
                model: self.selector.model,
                initially_loaded,
                action: "unload_failed".to_string(),
                detail: Some(error),
            }),
        }
    }
}

fn script_argv(
    args: &EvalCodingAgentArgs,
    fixture: FixtureDefinition,
    selector: &ModelSelector,
    tool_format: &str,
    run_dir: &Path,
) -> Vec<String> {
    let mut argv = vec![
        "--fixture".to_string(),
        fixture.id.to_string(),
        "--output-dir".to_string(),
        run_dir.display().to_string(),
        "--provider".to_string(),
        selector.provider.clone(),
        "--model".to_string(),
        selector.model.clone(),
        "--tool-format".to_string(),
        tool_format.to_string(),
        "--max-iterations".to_string(),
        args.max_iterations.to_string(),
        "--python".to_string(),
        args.python.clone(),
    ];
    if selector.provider == "mock" {
        argv.push("--seed-mock".to_string());
    }
    if let Some(json) = resolve_step_judge_json(args, selector) {
        argv.push("--step-judge-json".to_string());
        argv.push(json);
    }
    if let Some(reason) = args
        .override_reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        argv.push("--override-reason".to_string());
        argv.push(reason.to_string());
    }
    if let Some(json) = resolve_structural_validator_json(args) {
        argv.push("--structural-validator-json".to_string());
        argv.push(json);
    }
    argv
}

fn tool_format_override_warning_line(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(TOOL_FORMAT_OVERRIDE_WARNING_PREFIX))
}

fn error_report(
    run_id: String,
    fixture: FixtureDefinition,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    error: String,
) -> RunReport {
    RunReport {
        run_id,
        fixture_id: fixture.id.to_string(),
        fixture_name: fixture.name.to_string(),
        fixture_tool_sequence: fixture.tool_sequence.to_string(),
        selector,
        tool_format,
        status: "infra_error".to_string(),
        passed: false,
        skipped: false,
        skipped_reason: None,
        output_dir: run_dir.display().to_string(),
        transcript_events_path: run_dir
            .join("transcript_events.jsonl")
            .display()
            .to_string(),
        workspace_root: None,
        elapsed_ms: 0,
        duration_ms: 0,
        iterations: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        pricing_known: false,
        tool_calls: 0,
        rejected_tool_calls: 0,
        tool_sequence: Vec::new(),
        successful_tools: Vec::new(),
        transcript_event_count: 0,
        verification_success: false,
        harn_exit_code: 1,
        error: Some(error),
        stderr_excerpt: None,
        local_cleanup: None,
    }
}

fn skipped_report(
    run_id: String,
    fixture: FixtureDefinition,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    reason: String,
) -> RunReport {
    RunReport {
        run_id,
        fixture_id: fixture.id.to_string(),
        fixture_name: fixture.name.to_string(),
        fixture_tool_sequence: fixture.tool_sequence.to_string(),
        selector,
        tool_format,
        status: "skipped".to_string(),
        passed: false,
        skipped: true,
        skipped_reason: Some(reason),
        output_dir: run_dir.display().to_string(),
        transcript_events_path: run_dir
            .join("transcript_events.jsonl")
            .display()
            .to_string(),
        workspace_root: None,
        elapsed_ms: 0,
        duration_ms: 0,
        iterations: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        pricing_known: false,
        tool_calls: 0,
        rejected_tool_calls: 0,
        tool_sequence: Vec::new(),
        successful_tools: Vec::new(),
        transcript_event_count: 0,
        verification_success: false,
        harn_exit_code: 0,
        error: None,
        stderr_excerpt: None,
        local_cleanup: None,
    }
}

fn provider_available(selector: &ModelSelector) -> bool {
    if matches!(selector.provider.as_str(), "mock" | "fake") || selector_is_local(selector) {
        return true;
    }
    harn_vm::llm_config::provider_key_available(&selector.provider)
}

fn resolve_fixtures(raw_fixtures: &[String]) -> Result<Vec<FixtureDefinition>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in raw_fixtures {
        let fixture = raw.trim().to_ascii_lowercase();
        if fixture.is_empty() {
            continue;
        }
        if fixture == "all" {
            return Ok(FIXTURE_DEFINITIONS.to_vec());
        }
        let Some(definition) = fixture_definition(&fixture) else {
            return Err(format!(
                "unsupported --fixture `{fixture}`; expected one of: all, {}",
                FIXTURE_DEFINITIONS
                    .iter()
                    .map(|definition| definition.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        if seen.insert(definition.id) {
            out.push(definition);
        }
    }
    if out.is_empty() {
        return Err("at least one coding-agent fixture must be selected".to_string());
    }
    Ok(out)
}

fn fixture_definition(id: &str) -> Option<FixtureDefinition> {
    FIXTURE_DEFINITIONS
        .iter()
        .copied()
        .find(|definition| definition.id == id)
}

async fn resolve_models(args: &EvalCodingAgentArgs) -> Result<Vec<ModelSelector>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in normalize_model_selector_args(&args.models) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let selector = resolve_selector(trimmed);
        if seen.insert(selector_label(&selector)) {
            out.push(selector);
        }
    }
    if args.include_local {
        for selector in discover_local_models(args).await {
            if seen.insert(selector_label(&selector)) {
                out.push(selector);
            }
        }
    }
    Ok(out)
}

fn normalize_model_selector_args(raw_models: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < raw_models.len() {
        let current = raw_models[index].trim();
        if current.starts_with("provider=") && index + 1 < raw_models.len() {
            let next = raw_models[index + 1].trim();
            if next.starts_with("model=") {
                out.push(format!("{current},{next}"));
                index += 2;
                continue;
            }
        }
        out.push(current.to_string());
        index += 1;
    }
    out
}

async fn discover_local_models(args: &EvalCodingAgentArgs) -> Vec<ModelSelector> {
    let providers = if args.local_providers.is_empty() {
        local_provider_ids(None)
    } else {
        args.local_providers.clone()
    };
    let mut selectors = Vec::new();
    let mut seen = BTreeSet::new();
    for provider in providers {
        if selectors.len() >= args.max_local_models {
            break;
        }
        let Ok(snapshot) = snapshot_provider(&provider, Path::new(".")).await else {
            continue;
        };
        if !snapshot.reachable {
            continue;
        }
        let mut models = snapshot
            .loaded_models
            .iter()
            .map(|model| model.name.clone())
            .collect::<Vec<_>>();
        models.extend(snapshot.served_models);
        for model in models {
            if selectors.len() >= args.max_local_models {
                break;
            }
            let selector = ModelSelector {
                selector: format!("{provider}:{model}"),
                provider: provider.clone(),
                model,
            };
            if seen.insert(selector_label(&selector)) {
                selectors.push(selector);
            }
        }
    }
    selectors
}

fn normalize_tool_formats(raw_formats: &[String]) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in raw_formats {
        let format = raw.trim().to_ascii_lowercase();
        if format.is_empty() {
            continue;
        }
        if format != "native" && format != "text" {
            return Err(format!(
                "unsupported --tool-format `{format}`; expected `native` or `text`"
            ));
        }
        if seen.insert(format.clone()) {
            out.push(format);
        }
    }
    Ok(out)
}

fn build_matrix(
    fixtures: &[FixtureDefinition],
    models: &[ModelSelector],
    tool_formats: &[String],
    max_runs: Option<usize>,
) -> Vec<(FixtureDefinition, ModelSelector, String)> {
    if max_runs == Some(0) {
        return Vec::new();
    }
    let mut matrix = Vec::new();
    for fixture in fixtures {
        for selector in models {
            for tool_format in tool_formats {
                matrix.push((*fixture, selector.clone(), tool_format.clone()));
                if max_runs.is_some_and(|limit| matrix.len() >= limit) {
                    return matrix;
                }
            }
        }
    }
    matrix
}

#[allow(clippy::too_many_arguments)]
fn build_summary(
    output_dir: &Path,
    fixtures: Vec<FixtureDefinition>,
    models: Vec<ModelSelector>,
    tool_formats: Vec<String>,
    env_keys_loaded: Vec<LoadedEnvKey>,
    runs: Vec<RunReport>,
    step_judge_preset: Option<String>,
    run_label: String,
    baseline_comparison: Option<BaselineComparison>,
) -> EvalSummary {
    let passed_runs = runs.iter().filter(|run| run.passed).count();
    let skipped_runs = runs.iter().filter(|run| run.skipped).count();
    let failed_runs = runs
        .iter()
        .filter(|run| !run.passed && !run.skipped)
        .count();
    let total_cost_usd = runs.iter().map(|run| run.cost_usd).sum();
    let rollups = build_rollups(&runs);
    let comparisons = compare_formats(&runs);
    let parity_by_pair = build_parity_by_pair(&comparisons);
    let diverged_comparisons = comparisons
        .iter()
        .filter(|comparison| !comparison.divergence_reasons.is_empty())
        .count();
    let followups = suggest_followups(&runs, &comparisons);
    EvalSummary {
        schema_version: 3,
        fixture_ids: fixtures
            .iter()
            .map(|fixture| fixture.id.to_string())
            .collect(),
        fixtures: fixtures
            .iter()
            .map(|fixture| FixtureReport {
                id: fixture.id.to_string(),
                name: fixture.name.to_string(),
                tool_sequence: fixture.tool_sequence.to_string(),
                description: fixture.description.to_string(),
            })
            .collect(),
        output_dir: output_dir.display().to_string(),
        models,
        tool_formats,
        env_keys_loaded,
        total_runs: runs.len(),
        passed_runs,
        failed_runs,
        skipped_runs,
        diverged_comparisons,
        total_cost_usd,
        rollups,
        runs,
        comparisons,
        parity_by_pair,
        followups,
        step_judge_preset,
        run_label,
        baseline_comparison,
    }
}

fn load_baseline_comparison(path: &Path, runs: &[RunReport]) -> Result<BaselineComparison, String> {
    let resolved = if path.is_dir() {
        path.join("summary.json")
    } else {
        path.to_path_buf()
    };
    let raw = fs::read_to_string(&resolved)
        .map_err(|e| format!("failed to read {}: {e}", resolved.display()))?;
    let baseline: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse {} as JSON: {e}", resolved.display()))?;
    let baseline_runs = baseline
        .get("runs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{} has no `runs` array", resolved.display()))?;
    // Index baseline status by fixture_id. When the baseline has multiple
    // runs per fixture (e.g. native + text), prefer the first passing run
    // so a fixture passes the comparison if ANY baseline variant did.
    let mut baseline_status: BTreeMap<String, &str> = BTreeMap::new();
    for run in baseline_runs {
        let fixture_id = match run.get("fixture_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let passed = run.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
        let skipped = run
            .get("skipped")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let status = if skipped {
            "skipped"
        } else if passed {
            "passed"
        } else {
            "failed"
        };
        baseline_status
            .entry(fixture_id)
            .and_modify(|existing| {
                if *existing != "passed" && status == "passed" {
                    *existing = status;
                }
            })
            .or_insert(status);
    }
    let mut cell_status: BTreeMap<String, &str> = BTreeMap::new();
    for run in runs {
        let status = if run.skipped {
            "skipped"
        } else if run.passed {
            "passed"
        } else {
            "failed"
        };
        cell_status
            .entry(run.fixture_id.clone())
            .and_modify(|existing| {
                if *existing != "passed" && status == "passed" {
                    *existing = status;
                }
            })
            .or_insert(status);
    }
    let mut regressions = Vec::new();
    let mut recoveries = Vec::new();
    let mut unchanged_passes = Vec::new();
    let mut unchanged_failures = Vec::new();
    let mut missing_in_baseline = Vec::new();
    let mut missing_in_cell = Vec::new();
    for (fixture, cell) in &cell_status {
        match baseline_status.get(fixture) {
            None => missing_in_baseline.push(fixture.clone()),
            Some(base) => match (*base, *cell) {
                ("passed", "passed") => unchanged_passes.push(fixture.clone()),
                ("passed", _) => regressions.push(FixtureStatusDelta {
                    fixture_id: fixture.clone(),
                    baseline_status: (*base).to_string(),
                    cell_status: (*cell).to_string(),
                }),
                (_, "passed") => recoveries.push(FixtureStatusDelta {
                    fixture_id: fixture.clone(),
                    baseline_status: (*base).to_string(),
                    cell_status: (*cell).to_string(),
                }),
                _ => unchanged_failures.push(fixture.clone()),
            },
        }
    }
    for fixture in baseline_status.keys() {
        if !cell_status.contains_key(fixture) {
            missing_in_cell.push(fixture.clone());
        }
    }
    let baseline_label = baseline
        .get("run_label")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| baseline.get("output_dir").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let regressions_count = regressions.len();
    let recoveries_count = recoveries.len();
    let total_compared =
        regressions_count + recoveries_count + unchanged_passes.len() + unchanged_failures.len();
    let net_lift_pp = if total_compared == 0 {
        0.0
    } else {
        let raw =
            (recoveries_count as f64 - regressions_count as f64) / total_compared as f64 * 100.0;
        (raw * 10.0).round() / 10.0
    };
    Ok(BaselineComparison {
        baseline_label,
        baseline_path: resolved.display().to_string(),
        regressions,
        recoveries,
        unchanged_passes,
        unchanged_failures,
        missing_in_baseline,
        missing_in_cell,
        regressions_count,
        recoveries_count,
        net_lift_pp,
    })
}

fn build_rollups(runs: &[RunReport]) -> EvalRollups {
    EvalRollups {
        by_fixture: rollup_by(runs, |run| run.fixture_id.clone()),
        by_provider: rollup_by(runs, |run| run.selector.provider.clone()),
        by_model: rollup_by(runs, |run| run.selector.model.clone()),
        by_tool_format: rollup_by(runs, |run| run.tool_format.clone()),
        by_tool_sequence: rollup_by(runs, |run| run.fixture_tool_sequence.clone()),
    }
}

fn rollup_by<F>(runs: &[RunReport], key_for: F) -> Vec<RollupReport>
where
    F: Fn(&RunReport) -> String,
{
    let mut grouped: BTreeMap<String, RollupReport> = BTreeMap::new();
    for run in runs {
        let key = key_for(run);
        let entry = grouped.entry(key.clone()).or_insert_with(|| RollupReport {
            key,
            total_runs: 0,
            passed_runs: 0,
            failed_runs: 0,
            skipped_runs: 0,
            total_cost_usd: 0.0,
        });
        entry.total_runs += 1;
        if run.passed {
            entry.passed_runs += 1;
        } else if run.skipped {
            entry.skipped_runs += 1;
        } else {
            entry.failed_runs += 1;
        }
        entry.total_cost_usd += run.cost_usd;
    }
    grouped.into_values().collect()
}

fn compare_formats(runs: &[RunReport]) -> Vec<FormatComparison> {
    let mut grouped: BTreeMap<String, Vec<&RunReport>> = BTreeMap::new();
    for run in runs {
        grouped
            .entry(format!(
                "{}\0{}",
                run.fixture_id,
                selector_label(&run.selector)
            ))
            .or_default()
            .push(run);
    }
    let mut out = Vec::new();
    for group in grouped.values() {
        let Some(first) = group.first() else {
            continue;
        };
        let native = group
            .iter()
            .find(|run| run.tool_format == "native")
            .copied();
        let text = group.iter().find(|run| run.tool_format == "text").copied();
        if native.is_none() && text.is_none() {
            continue;
        }
        let pair = native.zip(text);
        let mut divergence_reasons = Vec::new();
        if let Some((native, text)) = pair {
            if native.status != text.status {
                divergence_reasons.push(format!(
                    "status differs: native={} text={}",
                    native.status, text.status
                ));
            }
            if native.passed != text.passed {
                divergence_reasons.push(format!(
                    "pass result differs: native={} text={}",
                    native.passed, text.passed
                ));
            }
            if native.verification_success != text.verification_success {
                divergence_reasons.push(format!(
                    "verifier result differs: native={} text={}",
                    native.verification_success, text.verification_success
                ));
            }
            if native.tool_sequence != text.tool_sequence {
                divergence_reasons.push(format!(
                    "tool sequence differs: native=[{}] text=[{}]",
                    native.tool_sequence.join(", "),
                    text.tool_sequence.join(", ")
                ));
            }
            if native.rejected_tool_calls != text.rejected_tool_calls {
                divergence_reasons.push(format!(
                    "rejected tool-call recovery differs: native={} text={}",
                    native.rejected_tool_calls, text.rejected_tool_calls
                ));
            }
        }
        let evidence_paths = [native, text]
            .into_iter()
            .flatten()
            .map(|run| run.transcript_events_path.clone())
            .collect::<Vec<_>>();
        out.push(FormatComparison {
            fixture_id: first.fixture_id.clone(),
            selector: first.selector.clone(),
            native_run_id: native.map(|run| run.run_id.clone()),
            text_run_id: text.map(|run| run.run_id.clone()),
            native_evidence_path: native.map(|run| run.transcript_events_path.clone()),
            text_evidence_path: text.map(|run| run.transcript_events_path.clone()),
            native_status: native.map(|run| run.status.clone()),
            text_status: text.map(|run| run.status.clone()),
            native_passed: native.map(|run| run.passed),
            text_passed: text.map(|run| run.passed),
            native_tool_call_count: native.map(|run| run.tool_calls),
            text_tool_call_count: text.map(|run| run.tool_calls),
            native_rejected_tool_call_count: native.map(|run| run.rejected_tool_calls),
            text_rejected_tool_call_count: text.map(|run| run.rejected_tool_calls),
            verifier_match: pair
                .map(|(native, text)| native.verification_success == text.verification_success),
            tool_sequence_match: pair
                .map(|(native, text)| native.tool_sequence == text.tool_sequence),
            rejected_tool_call_delta_text_minus_native: pair.map(|(native, text)| {
                text.rejected_tool_calls as i64 - native.rejected_tool_calls as i64
            }),
            token_delta_text_minus_native: pair.map(|(native, text)| {
                (text.input_tokens + text.output_tokens)
                    - (native.input_tokens + native.output_tokens)
            }),
            iteration_delta_text_minus_native: pair
                .map(|(native, text)| text.iterations - native.iterations),
            equivalent: pair.map(|(native, text)| {
                native.status == text.status
                    && native.passed == text.passed
                    && native.skipped == text.skipped
                    && native.verification_success == text.verification_success
                    && native.tool_sequence == text.tool_sequence
                    && native.rejected_tool_calls == text.rejected_tool_calls
            }),
            divergence_reasons,
            evidence_paths,
        });
    }
    out
}

fn build_parity_by_pair(comparisons: &[FormatComparison]) -> Vec<ToolModeParityPairSummary> {
    let fixture_inputs = comparisons
        .iter()
        .filter_map(parity_fixture_input)
        .collect::<Vec<_>>();
    let fixture_reports = tool_mode_parity::build_fixture_reports(&fixture_inputs);
    tool_mode_parity::build_pair_summaries(&fixture_reports)
}

fn parity_fixture_input(comparison: &FormatComparison) -> Option<ToolModeParityFixtureInput> {
    let native_verdict = comparison.native_status.clone()?;
    let text_verdict = comparison.text_status.clone()?;
    if native_verdict == "skipped" || text_verdict == "skipped" {
        return None;
    }
    Some(ToolModeParityFixtureInput {
        provider: comparison.selector.provider.clone(),
        model: comparison.selector.model.clone(),
        fixture_id: comparison.fixture_id.clone(),
        native_verdict,
        text_verdict,
        native_passed: comparison.native_passed?,
        text_passed: comparison.text_passed?,
        agreement: comparison.equivalent?,
        verifier_agreement: comparison.verifier_match?,
        native_tool_call_count: comparison.native_tool_call_count?,
        text_tool_call_count: comparison.text_tool_call_count?,
        native_rejected_tool_call_count: comparison.native_rejected_tool_call_count?,
        text_rejected_tool_call_count: comparison.text_rejected_tool_call_count?,
        native_evidence_path: comparison.native_evidence_path.clone()?,
        text_evidence_path: comparison.text_evidence_path.clone()?,
    })
}

fn suggest_followups(
    runs: &[RunReport],
    comparisons: &[FormatComparison],
) -> Vec<FollowupSuggestion> {
    let mut out = Vec::new();
    let failed = runs
        .iter()
        .filter(|run| !run.passed && !run.skipped)
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        out.push(FollowupSuggestion {
            title: "Normalize coding-agent fixture failures across provider presets".to_string(),
            body: "One or more fixture/provider/tool-format runs failed. Inspect the run directories and decide whether the gap belongs in provider adapters, preset prompting, transcript handling, or host-tool ergonomics.".to_string(),
            labels: vec!["eval".to_string(), "providers".to_string()],
            run_ids: failed,
        });
    }

    let rejected = runs
        .iter()
        .filter(|run| run.rejected_tool_calls > 0)
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    if !rejected.is_empty() {
        out.push(FollowupSuggestion {
            title: "Abstract rejected tool-call recovery in agent transcripts".to_string(),
            body: "Some runs recovered after rejected tool calls. Add runtime support or preset guidance so harness authors can distinguish recoverable provider/tool-shape noise from user-relevant transcript events.".to_string(),
            labels: vec!["agents".to_string(), "transcripts".to_string()],
            run_ids: rejected,
        });
    }

    let mismatched = comparisons
        .iter()
        .filter(|comparison| !comparison.divergence_reasons.is_empty())
        .map(|comparison| {
            format!(
                "{}:{} ({})",
                comparison.fixture_id,
                selector_label(&comparison.selector),
                comparison.divergence_reasons.join("; ")
            )
        })
        .collect::<Vec<_>>();
    if !mismatched.is_empty() {
        let run_ids = comparisons
            .iter()
            .filter(|comparison| !comparison.divergence_reasons.is_empty())
            .flat_map(|comparison| {
                [
                    comparison.native_run_id.clone(),
                    comparison.text_run_id.clone(),
                ]
            })
            .flatten()
            .collect::<Vec<_>>();
        out.push(FollowupSuggestion {
            title: "Make native/text tool modes behaviorally interchangeable for preset harnesses"
                .to_string(),
            body: format!(
                "Native and text tool modes diverged for: {}. The preset/runtime boundary should hide provider tool-channel differences where possible.",
                mismatched.join(", ")
            ),
            labels: vec!["agents".to_string(), "tools".to_string()],
            run_ids,
        });
    }

    let unknown_pricing = runs
        .iter()
        .filter(|run| {
            !run.skipped
                && !run.pricing_known
                && !matches!(run.selector.provider.as_str(), "mock" | "fake")
                && !selector_is_local(&run.selector)
        })
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    if !unknown_pricing.is_empty() {
        out.push(FollowupSuggestion {
            title: "Fill provider pricing metadata for benchmarked models".to_string(),
            body: "At least one live provider/model produced usage metrics but had no pricing entry, which weakens cost comparisons in eval reports.".to_string(),
            labels: vec!["providers".to_string(), "docs".to_string()],
            run_ids: unknown_pricing,
        });
    }
    out
}

fn write_json_artifacts(output_dir: &Path, summary: &EvalSummary) -> Result<(), String> {
    write_json_pretty(&output_dir.join("summary.json"), summary)?;
    write_jsonl(&output_dir.join("per_run.jsonl"), &summary.runs)?;
    let summary_value = serde_json::to_value(summary).map_err(|error| error.to_string())?;
    let readiness = local_readiness::report_from_summary_json(
        &summary_value,
        output_dir.display().to_string(),
    )?;
    write_json_pretty(&output_dir.join("local_readiness.json"), &readiness)?;
    let generated_at = RealClock::new()
        .now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("failed to format parity overlay timestamp: {error}"))?;
    let parity_dir = output_dir.join(TOOL_MODE_PARITY_DIRECTORY);
    let parity_reports = tool_mode_parity::build_fixture_reports(
        &summary
            .comparisons
            .iter()
            .filter_map(parity_fixture_input)
            .collect::<Vec<_>>(),
    );
    for report in &parity_reports {
        let path = parity_dir
            .join(sanitize_id(&format!(
                "{}__{}:{}",
                report.fixture_id, report.provider, report.model
            )))
            .join("parity.json");
        tool_mode_parity::write_fixture_report(&path, report)?;
    }
    let overlay = tool_mode_parity::build_overlay(
        &summary.parity_by_pair,
        &generated_at,
        TOOL_MODE_PARITY_FIXTURE_SUITE,
        output_dir,
    );
    tool_mode_parity::write_overlay(
        &output_dir.join(TOOL_MODE_PARITY_OVERLAY_FILENAME),
        &overlay,
    )?;
    Ok(())
}

fn announce_output_paths(output_dir: &Path) {
    eprintln!(
        "wrote {}, {}, {}, {}, {}, {}, and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_run.jsonl").display(),
        output_dir.join("local_readiness.json").display(),
        output_dir.join(TOOL_MODE_PARITY_DIRECTORY).display(),
        output_dir.join(TOOL_MODE_PARITY_OVERLAY_FILENAME).display(),
        output_dir.join("summary.md").display(),
        output_dir.join("followups.md").display()
    );
}

// ─── Legacy direct-render path (gated by HARN_CLI_IMPL=rust) ────────────

fn write_markdown_artifacts_legacy(output_dir: &Path, summary: &EvalSummary) -> Result<(), String> {
    fs::write(output_dir.join("summary.md"), render_markdown(summary))
        .map_err(|error| format!("failed to write summary.md: {error}"))?;
    fs::write(output_dir.join("followups.md"), render_followups(summary))
        .map_err(|error| format!("failed to write followups.md: {error}"))?;
    Ok(())
}

fn print_summary_legacy(summary: &EvalSummary) {
    println!(
        "coding-agent eval: {}/{} passed, {} skipped, total_cost_usd={:.6}",
        summary.passed_runs, summary.total_runs, summary.skipped_runs, summary.total_cost_usd
    );
}

fn print_json_legacy(summary: &EvalSummary) {
    match serde_json::to_string_pretty(summary) {
        Ok(payload) => println!("{payload}"),
        Err(error) => eprintln!("warning: failed to render summary JSON: {error}"),
    }
}

// ─── Dispatch (.harn) render path ────────────────────────────────────────

async fn write_markdown_artifacts_dispatch(
    output_dir: &Path,
    summary: &EvalSummary,
) -> Result<(), i32> {
    let markdown = render_via_dispatch(summary, "markdown").await?;
    if let Err(error) = fs::write(output_dir.join("summary.md"), markdown) {
        eprintln!("error: failed to write summary.md: {error}");
        return Err(1);
    }
    let followups = render_via_dispatch(summary, "followups").await?;
    if let Err(error) = fs::write(output_dir.join("followups.md"), followups) {
        eprintln!("error: failed to write followups.md: {error}");
        return Err(1);
    }
    Ok(())
}

async fn print_summary_dispatch(summary: &EvalSummary) -> Result<(), i32> {
    let payload = render_via_dispatch(summary, "summary").await?;
    print!("{payload}");
    // The script emits exactly the legacy summary line (no trailing
    // newline); add one to match the legacy `println!` semantics.
    if !payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

async fn print_json_dispatch(summary: &EvalSummary) -> Result<(), i32> {
    let payload = render_via_dispatch(summary, "json").await?;
    print!("{payload}");
    if !payload.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Dispatch to the embedded `cli/eval/coding_agent.harn` script for one
/// of the four rendering modes (markdown / followups / summary / json).
/// Returns the captured stdout on success, or a propagated exit code
/// on failure.
///
/// **Concurrency.** Held under [`DISPATCH_RENDER_LOCK`] so concurrent
/// in-process callers don't race on the global env vars the Rust shim
/// sets to hand the report to the script. See the lock's docstring
/// for the trade-off rationale.
async fn render_via_dispatch(summary: &EvalSummary, mode: &str) -> Result<String, i32> {
    let summary_json = match serde_json::to_string(summary) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise EvalSummary for dispatch: {error}");
            return Err(1);
        }
    };
    let _guard = DISPATCH_RENDER_LOCK.lock().await;
    let _summary = ScopedEnvVar::set(CODING_AGENT_SUMMARY_ENV, &summary_json);
    let _mode = ScopedEnvVar::set(CODING_AGENT_MODE_ENV, mode);

    let outcome = dispatch::run_embedded_script("eval/coding_agent", Vec::new(), false).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if outcome.exit_code != 0 {
        return Err(outcome.exit_code);
    }
    Ok(outcome.stdout)
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, format!("{body}\n")).map_err(|error| error.to_string())
}

fn write_jsonl<T: Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
    let mut body = String::new();
    for item in items {
        let line = serde_json::to_string(item).map_err(|error| error.to_string())?;
        body.push_str(&line);
        body.push('\n');
    }
    fs::write(path, body).map_err(|error| error.to_string())
}

fn render_markdown(summary: &EvalSummary) -> String {
    let mut out = String::new();
    out.push_str("# Coding Agent Harness Quality Suite\n\n");
    out.push_str(&format!(
        "- fixtures: `{}`\n- passed: {}/{}\n- skipped: {}\n- total_cost_usd: {:.6}\n\n",
        summary.fixture_ids.join("`, `"),
        summary.passed_runs,
        summary.total_runs,
        summary.skipped_runs,
        summary.total_cost_usd
    ));
    render_rollup_table(&mut out, "By Fixture", &summary.rollups.by_fixture);
    render_rollup_table(&mut out, "By Provider", &summary.rollups.by_provider);
    render_rollup_table(&mut out, "By Model", &summary.rollups.by_model);
    render_rollup_table(&mut out, "By Tool Format", &summary.rollups.by_tool_format);
    render_rollup_table(
        &mut out,
        "By Tool Sequence",
        &summary.rollups.by_tool_sequence,
    );

    out.push_str("\n## Runs\n\n");
    out.push_str("| fixture | run | provider | model | tool format | fixture sequence | tool calls | status | iterations | tokens | cost | transcript | output |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---:|---:|---:|---|---|\n");
    for run in &summary.runs {
        let tool_sequence = if run.tool_sequence.is_empty() {
            "-".to_string()
        } else {
            run.tool_sequence.join(", ").replace('|', "\\|")
        };
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` | `{}` | `{}` | `{}` | {} | {} | {} | {:.6} | {} | `{}` |\n",
            run.fixture_id,
            run.run_id,
            run.selector.provider,
            run.selector.model.replace('|', "\\|"),
            run.tool_format,
            run.fixture_tool_sequence,
            tool_sequence,
            run.status,
            run.iterations,
            run.input_tokens + run.output_tokens,
            run.cost_usd,
            markdown_link(
                &run.transcript_event_count.to_string(),
                &run.transcript_events_path
            ),
            run.output_dir
        ));
    }
    if let Some(comparison) = &summary.baseline_comparison {
        out.push_str("\n## Baseline Comparison\n\n");
        out.push_str(&format!(
            "Compared against `{}`{}.\n\n",
            comparison.baseline_path,
            if comparison.baseline_label.is_empty() {
                String::new()
            } else {
                format!(" (label: `{}`)", comparison.baseline_label)
            },
        ));
        out.push_str(&format!(
            "- regressions: **{}** (baseline passed, this cell failed)\n- recoveries: **{}** (baseline failed, this cell passed)\n- net lift: **{:+.1}pp**\n\n",
            comparison.regressions_count,
            comparison.recoveries_count,
            comparison.net_lift_pp,
        ));
        if !comparison.regressions.is_empty() {
            out.push_str("### Regressions\n\n");
            for delta in &comparison.regressions {
                out.push_str(&format!(
                    "- `{}`: `{}` → `{}`\n",
                    delta.fixture_id, delta.baseline_status, delta.cell_status,
                ));
            }
            out.push('\n');
        }
        if !comparison.recoveries.is_empty() {
            out.push_str("### Recoveries\n\n");
            for delta in &comparison.recoveries {
                out.push_str(&format!(
                    "- `{}`: `{}` → `{}`\n",
                    delta.fixture_id, delta.baseline_status, delta.cell_status,
                ));
            }
            out.push('\n');
        }
    }
    if !summary.comparisons.is_empty() {
        out.push_str("\n## Native/Text Comparison\n\n");
        out.push_str("| fixture | selector | native | text | equivalent | verifier | tools | rejected delta | token delta | iteration delta | evidence |\n");
        out.push_str("|---|---|---|---|---|---|---|---:|---:|---:|---|\n");
        for comparison in &summary.comparisons {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                comparison.fixture_id,
                selector_label(&comparison.selector),
                comparison
                    .native_status
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                comparison
                    .text_status
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                optional_bool_mark(comparison.equivalent),
                optional_bool_mark(comparison.verifier_match),
                optional_bool_mark(comparison.tool_sequence_match),
                comparison
                    .rejected_tool_call_delta_text_minus_native
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                comparison
                    .token_delta_text_minus_native
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                comparison
                    .iteration_delta_text_minus_native
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                comparison_evidence_links(comparison)
            ));
        }
    }
    if !summary.parity_by_pair.is_empty() {
        out.push_str("\n## Parity report — native vs text\n\n");
        out.push_str("| selector | sample | native pass | text pass | agreement | verifier divergence | native_only | text_only | both_pass | both_fail |\n");
        out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for pair in &summary.parity_by_pair {
            out.push_str(&format!(
                "| `{}` | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {} | {} | {} | {} |\n",
                selector_label(&ModelSelector {
                    selector: format!("{}:{}", pair.provider, pair.model),
                    provider: pair.provider.clone(),
                    model: pair.model.clone(),
                }),
                pair.sample_size,
                pair.native.pass_rate * 100.0,
                pair.text.pass_rate * 100.0,
                pair.agreement_rate * 100.0,
                pair.verifier_divergence_rate * 100.0,
                pair.divergence_counts.native_only_pass,
                pair.divergence_counts.text_only_pass,
                pair.divergence_counts.both_pass,
                pair.divergence_counts.both_fail,
            ));
        }
    }
    let diverged = summary
        .comparisons
        .iter()
        .filter(|comparison| !comparison.divergence_reasons.is_empty())
        .collect::<Vec<_>>();
    if !diverged.is_empty() {
        out.push_str("\n## Native/Text Divergence Evidence\n\n");
        for comparison in diverged {
            out.push_str(&format!(
                "- `{}` `{}`: {}\n",
                comparison.fixture_id,
                selector_label(&comparison.selector),
                comparison.divergence_reasons.join("; ")
            ));
            if !comparison.evidence_paths.is_empty() {
                out.push_str(&format!(
                    "  Evidence: {}\n",
                    comparison_evidence_links(comparison)
                ));
            }
        }
    }
    out
}

fn render_rollup_table(out: &mut String, title: &str, rollups: &[RollupReport]) {
    out.push_str(&format!("## {title}\n\n"));
    out.push_str("| key | passed | failed | skipped | total | cost |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|\n");
    for rollup in rollups {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {:.6} |\n",
            rollup.key.replace('|', "\\|"),
            rollup.passed_runs,
            rollup.failed_runs,
            rollup.skipped_runs,
            rollup.total_runs,
            rollup.total_cost_usd
        ));
    }
    out.push('\n');
}

fn render_followups(summary: &EvalSummary) -> String {
    let mut out = String::new();
    out.push_str("# Follow-up Issue Candidates\n\n");
    if summary.followups.is_empty() {
        out.push_str("No follow-up issue candidates were generated from this run.\n");
        return out;
    }
    for followup in &summary.followups {
        out.push_str(&format!("## {}\n\n{}\n\n", followup.title, followup.body));
        if !followup.run_ids.is_empty() {
            out.push_str(&format!("- run_ids: `{}`\n", followup.run_ids.join("`, `")));
        }
        if !followup.labels.is_empty() {
            out.push_str(&format!("- labels: `{}`\n", followup.labels.join("`, `")));
        }
        out.push('\n');
    }
    out
}

fn read_run_summary(run_dir: &Path) -> Option<JsonValue> {
    let raw = fs::read_to_string(run_dir.join("summary.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn parse_last_json_line(stdout: &str) -> Option<JsonValue> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find_map(|line| serde_json::from_str::<JsonValue>(line).ok())
}

fn string_array(value: Option<&JsonValue>) -> Vec<String> {
    value
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn non_empty_string_array(value: Option<&JsonValue>) -> Option<Vec<String>> {
    let values = string_array(value);
    (!values.is_empty()).then_some(values)
}

fn tool_call_sequence(value: Option<&JsonValue>) -> Option<Vec<String>> {
    let calls = value.and_then(JsonValue::as_array)?;
    let mut sequence = Vec::new();
    for call in calls {
        if let Some(name) = call
            .get("name")
            .or_else(|| call.get("tool_name"))
            .and_then(JsonValue::as_str)
        {
            sequence.push(name.to_string());
        }
    }
    (!sequence.is_empty()).then_some(sequence)
}

fn optional_bool_mark(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    }
}

fn comparison_evidence_links(comparison: &FormatComparison) -> String {
    let mut links = Vec::new();
    if let Some(native) = comparison.native_evidence_path.as_deref() {
        links.push(markdown_link("native", native));
    }
    if let Some(text) = comparison.text_evidence_path.as_deref() {
        links.push(markdown_link("text", text));
    }
    if links.is_empty() {
        "-".to_string()
    } else {
        links.join("<br>")
    }
}

fn markdown_link(label: &str, target: &str) -> String {
    format!(
        "[{}]({})",
        label.replace('|', "\\|"),
        target
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
    )
}

fn reset_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(path).map_err(|error| error.to_string())
}

fn run_id_for(fixture: FixtureDefinition, selector: &ModelSelector, tool_format: &str) -> String {
    sanitize_id(&format!(
        "{}__{}__{}",
        fixture.id,
        selector_label(selector),
        tool_format
    ))
}

fn sanitize_id(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn default_output_dir() -> PathBuf {
    PathBuf::from(".harn-runs")
        .join("coding-agent-bench")
        .join("latest")
}

fn excerpt(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let max = 4000;
    if trimmed.len() <= max {
        return Some(trimmed.to_string());
    }
    let mut truncated = String::new();
    for ch in trimmed.chars().take(max) {
        truncated.push(ch);
    }
    truncated.push_str("...");
    Some(truncated)
}

fn load_env_files(paths: &[PathBuf]) -> Result<(EnvOverlay, Vec<LoadedEnvKey>), String> {
    let mut previous = Vec::new();
    let mut loaded = Vec::new();
    let mut touched = BTreeSet::new();
    for path in paths {
        let path = expand_home(path);
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read env file {}: {error}", path.display()))?;
        for (line_no, line) in raw.lines().enumerate() {
            let Some((key, value)) = parse_env_line(line).map_err(|error| {
                format!("{}:{}: {error}", path.display(), line_no.saturating_add(1))
            })?
            else {
                continue;
            };
            if touched.insert(key.clone()) {
                previous.push((OsString::from(&key), std::env::var_os(&key)));
            }
            std::env::set_var(&key, value);
            loaded.push(LoadedEnvKey {
                key,
                source: path.display().to_string(),
            });
        }
    }
    Ok((EnvOverlay { previous }, loaded))
}

fn parse_env_line(line: &str) -> Result<Option<(String, String)>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    let Some((key, value)) = trimmed.split_once('=') else {
        return Err("expected KEY=VALUE".to_string());
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("empty key".to_string());
    }
    if !key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!("invalid key `{key}`"));
    }
    Ok(Some((key.to_string(), unquote_env_value(value.trim()))))
}

fn unquote_env_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
#[path = "eval_coding_agent_tests.rs"]
mod tests;
