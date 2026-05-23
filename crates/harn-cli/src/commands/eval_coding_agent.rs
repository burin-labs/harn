//! `harn eval coding-agent` — empirical preset/provider benchmark for a
//! small coding-agent fixture suite.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::clock::{Clock, RealClock};
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::cli::EvalCodingAgentArgs;
use crate::commands::eval_model_selector::{
    resolve_selector, selector_is_local, selector_label, ModelSelector,
};
use crate::commands::local::runtime::{
    local_provider_ids, ollama_unload_model, snapshot_provider, LocalProviderSnapshot,
};
use crate::commands::local_readiness;
use crate::commands::run::{execute_run, CliLlmMockMode, RunProfileOptions};

const CODING_AGENT_SUITE_HARN: &str = include_str!("../../assets/evals/coding_agent_suite.harn");

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
    tool_sequence: String,
    selector: ModelSelector,
    tool_format: String,
    status: String,
    passed: bool,
    skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_reason: Option<String>,
    output_dir: String,
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
    native_status: Option<String>,
    text_status: Option<String>,
    native_passed: Option<bool>,
    text_passed: Option<bool>,
    token_delta_text_minus_native: Option<i64>,
    iteration_delta_text_minus_native: Option<i64>,
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
    total_cost_usd: f64,
    rollups: EvalRollups,
    runs: Vec<RunReport>,
    comparisons: Vec<FormatComparison>,
    followups: Vec<FollowupSuggestion>,
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

    let summary = build_summary(
        &output_dir,
        fixtures,
        models,
        tool_formats,
        env_keys_loaded,
        reports,
    );
    if let Err(error) = write_outputs(&output_dir, &summary) {
        eprintln!("error: failed to write benchmark outputs: {error}");
        return 1;
    }
    eprintln!(
        "wrote {}, {}, {}, {}, and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_run.jsonl").display(),
        output_dir.join("local_readiness.json").display(),
        output_dir.join("summary.md").display(),
        output_dir.join("followups.md").display()
    );
    if args.json {
        match serde_json::to_string_pretty(&summary) {
            Ok(payload) => println!("{payload}"),
            Err(error) => eprintln!("warning: failed to render summary JSON: {error}"),
        }
    } else {
        println!(
            "coding-agent eval: {}/{} passed, {} skipped, total_cost_usd={:.6}",
            summary.passed_runs, summary.total_runs, summary.skipped_runs, summary.total_cost_usd
        );
    }

    if had_error {
        1
    } else {
        0
    }
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
    let outcome = execute_run(
        &script_path.to_string_lossy(),
        false,
        HashSet::new(),
        argv,
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;
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
            tool_sequence: fixture.tool_sequence.to_string(),
            selector,
            tool_format,
            status: "infra_error".to_string(),
            passed: false,
            skipped: false,
            skipped_reason: None,
            output_dir: run_dir.display().to_string(),
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
        tool_sequence: ctx.fixture.tool_sequence.to_string(),
        selector: ctx.selector,
        tool_format: ctx.tool_format,
        status,
        passed,
        skipped: false,
        skipped_reason: None,
        output_dir: ctx.run_dir.display().to_string(),
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
    argv
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
        tool_sequence: fixture.tool_sequence.to_string(),
        selector,
        tool_format,
        status: "infra_error".to_string(),
        passed: false,
        skipped: false,
        skipped_reason: None,
        output_dir: run_dir.display().to_string(),
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
        tool_sequence: fixture.tool_sequence.to_string(),
        selector,
        tool_format,
        status: "skipped".to_string(),
        passed: false,
        skipped: true,
        skipped_reason: Some(reason),
        output_dir: run_dir.display().to_string(),
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
    for raw in &args.models {
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

fn build_summary(
    output_dir: &Path,
    fixtures: Vec<FixtureDefinition>,
    models: Vec<ModelSelector>,
    tool_formats: Vec<String>,
    env_keys_loaded: Vec<LoadedEnvKey>,
    runs: Vec<RunReport>,
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
    let followups = suggest_followups(&runs, &comparisons);
    EvalSummary {
        schema_version: 2,
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
        total_cost_usd,
        rollups,
        runs,
        comparisons,
        followups,
    }
}

fn build_rollups(runs: &[RunReport]) -> EvalRollups {
    EvalRollups {
        by_fixture: rollup_by(runs, |run| run.fixture_id.clone()),
        by_provider: rollup_by(runs, |run| run.selector.provider.clone()),
        by_model: rollup_by(runs, |run| run.selector.model.clone()),
        by_tool_format: rollup_by(runs, |run| run.tool_format.clone()),
        by_tool_sequence: rollup_by(runs, |run| run.tool_sequence.clone()),
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
                "{}__{}",
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
        out.push(FormatComparison {
            fixture_id: first.fixture_id.clone(),
            selector: first.selector.clone(),
            native_status: native.map(|run| run.status.clone()),
            text_status: text.map(|run| run.status.clone()),
            native_passed: native.map(|run| run.passed),
            text_passed: text.map(|run| run.passed),
            token_delta_text_minus_native: native.zip(text).map(|(native, text)| {
                (text.input_tokens + text.output_tokens)
                    - (native.input_tokens + native.output_tokens)
            }),
            iteration_delta_text_minus_native: native
                .zip(text)
                .map(|(native, text)| text.iterations - native.iterations),
        });
    }
    out
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
        .filter(|comparison| {
            comparison.native_passed.is_some()
                && comparison.text_passed.is_some()
                && comparison.native_passed != comparison.text_passed
        })
        .map(|comparison| {
            format!(
                "{}:{}",
                comparison.fixture_id,
                selector_label(&comparison.selector)
            )
        })
        .collect::<Vec<_>>();
    if !mismatched.is_empty() {
        out.push(FollowupSuggestion {
            title: "Make native/text tool modes behaviorally interchangeable for preset harnesses"
                .to_string(),
            body: format!(
                "Native and text tool modes diverged for: {}. The preset/runtime boundary should hide provider tool-channel differences where possible.",
                mismatched.join(", ")
            ),
            labels: vec!["agents".to_string(), "tools".to_string()],
            run_ids: Vec::new(),
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

fn write_outputs(output_dir: &Path, summary: &EvalSummary) -> Result<(), String> {
    write_json_pretty(&output_dir.join("summary.json"), summary)?;
    write_jsonl(&output_dir.join("per_run.jsonl"), &summary.runs)?;
    let summary_value = serde_json::to_value(summary).map_err(|error| error.to_string())?;
    let readiness = local_readiness::report_from_summary_json(
        &summary_value,
        output_dir.display().to_string(),
    )?;
    write_json_pretty(&output_dir.join("local_readiness.json"), &readiness)?;
    fs::write(output_dir.join("summary.md"), render_markdown(summary))
        .map_err(|error| error.to_string())?;
    fs::write(output_dir.join("followups.md"), render_followups(summary))
        .map_err(|error| error.to_string())?;
    Ok(())
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
    out.push_str("| fixture | run | provider | model | tool format | tool sequence | status | iterations | tokens | cost | transcript | output |\n");
    out.push_str("|---|---|---|---|---|---|---|---:|---:|---:|---:|---|\n");
    for run in &summary.runs {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` | `{}` | `{}` | {} | {} | {} | {:.6} | {} | `{}` |\n",
            run.fixture_id,
            run.run_id,
            run.selector.provider,
            run.selector.model.replace('|', "\\|"),
            run.tool_format,
            run.tool_sequence,
            run.status,
            run.iterations,
            run.input_tokens + run.output_tokens,
            run.cost_usd,
            run.transcript_event_count,
            run.output_dir
        ));
    }
    if !summary.comparisons.is_empty() {
        out.push_str("\n## Native/Text Comparison\n\n");
        out.push_str("| fixture | selector | native | text | token delta | iteration delta |\n");
        out.push_str("|---|---|---|---|---:|---:|\n");
        for comparison in &summary.comparisons {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} | {} | {} |\n",
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
                comparison
                    .token_delta_text_minus_native
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                comparison
                    .iteration_delta_text_minus_native
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ));
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
mod tests {
    use super::*;

    #[test]
    fn dotenv_parser_strips_export_and_quotes_without_leaking_values() {
        let parsed = parse_env_line("export TOGETHER_API_KEY=\"secret\"")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.0, "TOGETHER_API_KEY");
        assert_eq!(parsed.1, "secret");
        assert!(parse_env_line("# comment").unwrap().is_none());
    }

    #[test]
    fn markdown_escapes_model_table_pipes() {
        let selector = ModelSelector {
            selector: "provider:a|b".to_string(),
            provider: "provider".to_string(),
            model: "a|b".to_string(),
        };
        let summary = EvalSummary {
            schema_version: 2,
            fixture_ids: vec!["python-add".to_string()],
            fixtures: vec![FixtureReport {
                id: "python-add".to_string(),
                name: "Python add repair".to_string(),
                tool_sequence: "multi-tool".to_string(),
                description: "One-file Python bug fix verified by unittest output.".to_string(),
            }],
            output_dir: "out".to_string(),
            models: vec![selector.clone()],
            tool_formats: vec!["native".to_string()],
            env_keys_loaded: Vec::new(),
            total_runs: 1,
            passed_runs: 1,
            failed_runs: 0,
            skipped_runs: 0,
            total_cost_usd: 0.0,
            rollups: EvalRollups {
                by_fixture: vec![RollupReport {
                    key: "python-add".to_string(),
                    total_runs: 1,
                    passed_runs: 1,
                    failed_runs: 0,
                    skipped_runs: 0,
                    total_cost_usd: 0.0,
                }],
                by_provider: Vec::new(),
                by_model: Vec::new(),
                by_tool_format: Vec::new(),
                by_tool_sequence: Vec::new(),
            },
            runs: vec![RunReport {
                run_id: "r".to_string(),
                fixture_id: "python-add".to_string(),
                fixture_name: "Python add repair".to_string(),
                tool_sequence: "multi-tool".to_string(),
                selector,
                tool_format: "native".to_string(),
                status: "passed".to_string(),
                passed: true,
                skipped: false,
                skipped_reason: None,
                output_dir: "out/r".to_string(),
                workspace_root: None,
                elapsed_ms: 1,
                duration_ms: 1,
                iterations: 1,
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: 0.0,
                pricing_known: false,
                tool_calls: 0,
                rejected_tool_calls: 0,
                successful_tools: Vec::new(),
                transcript_event_count: 0,
                verification_success: true,
                harn_exit_code: 0,
                error: None,
                stderr_excerpt: None,
                local_cleanup: None,
            }],
            comparisons: Vec::new(),
            followups: Vec::new(),
        };
        let md = render_markdown(&summary);
        assert!(md.contains("a\\|b"));
    }

    #[test]
    fn fixture_selection_supports_all_and_specific_ids() {
        let all = resolve_fixtures(&["all".to_string()]).expect("all fixtures resolve");
        assert_eq!(all.len(), FIXTURE_DEFINITIONS.len());

        let selected = resolve_fixtures(&[
            "python-add".to_string(),
            "python-add".to_string(),
            "read-only-audit".to_string(),
        ])
        .expect("specific fixtures resolve");
        assert_eq!(
            selected
                .iter()
                .map(|fixture| fixture.id)
                .collect::<Vec<_>>(),
            vec!["python-add", "read-only-audit"],
        );

        let error = resolve_fixtures(&["missing".to_string()]).expect_err("unknown fixture fails");
        assert!(error.contains("unsupported --fixture `missing`"));
    }

    #[test]
    fn matrix_max_runs_bounds_fixture_model_tool_product() {
        let fixtures = resolve_fixtures(&["all".to_string()]).expect("fixtures");
        let selector = ModelSelector {
            selector: "mock:mock".to_string(),
            provider: "mock".to_string(),
            model: "mock".to_string(),
        };
        let selectors = vec![selector];
        let tool_formats = vec!["native".to_string(), "text".to_string()];
        let matrix = build_matrix(&fixtures, &selectors, &tool_formats, Some(3));
        assert_eq!(matrix.len(), 3);
        assert_eq!(
            matrix
                .iter()
                .map(|(fixture, _selector, tool_format)| (fixture.id, tool_format.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("python-add", "native"),
                ("python-add", "text"),
                ("cli-help-flag", "native"),
            ],
        );

        let empty = build_matrix(&fixtures, &selectors, &tool_formats, Some(0));
        assert!(empty.is_empty());
    }
}
