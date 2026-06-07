mod fixtures;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::clock::{Clock, RealClock};
use harn_vm::orchestration::{
    eval_pack_case_fingerprint, evaluate_eval_pack_manifest_resumable_with_live_executor,
    EvalPackCase, EvalPackCommandObject, EvalPackCommandSpec, EvalPackLiveExecutor,
    EvalPackLiveExecutorRequest, EvalPackLiveVerifyOutcome, EvalPackManifest,
};
use serde_json::Value as JsonValue;

use crate::cli::EvalCodingAgentArgs;
use crate::commands::eval_model_selector::{selector_is_local, selector_label, ModelSelector};
use crate::commands::local::runtime::{
    ollama_unload_model, snapshot_provider, LocalProviderSnapshot,
};
use crate::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunProfileOptions, RunSandboxOptions,
};

use super::{
    excerpt, non_empty_string_array, parse_last_json_line, read_run_summary, script_argv,
    string_array, tool_call_sequence, LocalCleanupReport, RunReport, CODING_AGENT_EVAL_PACK_ID,
    CODING_AGENT_SUITE_HARN, TOOL_FORMAT_OVERRIDE_WARNING_PREFIX,
};
#[cfg(test)]
pub(super) use fixtures::coding_agent_live_verify_cases;
pub(super) use fixtures::{
    fixture_description, fixture_id, fixture_name, fixture_tool_sequence, resolve_fixtures,
};

struct LocalRunGuard {
    selector: ModelSelector,
    stop_after: bool,
    snapshot: Option<LocalProviderSnapshot>,
}

struct RunSummaryContext {
    run_id: String,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    elapsed_ms: u64,
    exit_code: i32,
    stderr: String,
    local_cleanup: Option<LocalCleanupReport>,
}

struct CodingAgentLiveExecutor<'a> {
    args: &'a EvalCodingAgentArgs,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_id: String,
    run_dir: PathBuf,
    report: Option<RunReport>,
}

pub(super) async fn run_matrix_entry(
    args: &EvalCodingAgentArgs,
    output_dir: &Path,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
) -> RunReport {
    let run_id = super::run_id_for(&fixture, &selector, &tool_format);
    let run_dir = output_dir.join(&run_id);
    if let Err(error) = fs::create_dir_all(&run_dir) {
        return error_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            format!("failed to create run directory: {error}"),
        );
    }

    if !provider_available(&selector) {
        let reason = format!(
            "provider `{}` has no configured credentials",
            selector.provider
        );
        return skipped_report(run_id, fixture, selector, tool_format, run_dir, reason);
    }

    let manifest =
        match coding_agent_eval_pack_manifest(&run_dir, &fixture, &selector, &tool_format) {
            Ok(manifest) => manifest,
            Err(error) => {
                return error_report(run_id, fixture, selector, tool_format, run_dir, error);
            }
        };
    let mut executor = CodingAgentLiveExecutor {
        args,
        fixture: fixture.clone(),
        selector: selector.clone(),
        tool_format: tool_format.clone(),
        run_id: run_id.clone(),
        run_dir: run_dir.clone(),
        report: None,
    };
    let previous_event_log = harn_vm::event_log::active_event_log();
    harn_vm::event_log::reset_active_event_log();
    let pack_report =
        evaluate_eval_pack_manifest_resumable_with_live_executor(&manifest, None, &mut executor);
    harn_vm::event_log::reset_active_event_log();
    if let Some(log) = previous_event_log {
        harn_vm::event_log::install_active_event_log(log);
    }
    match pack_report {
        Ok(_) => executor.report.unwrap_or_else(|| {
            report_from_existing_summary(run_id, fixture, selector, tool_format, run_dir)
        }),
        Err(error) => error_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            format!("eval pack live-verify execution failed: {error}"),
        ),
    }
}

async fn execute_coding_agent_trial(
    args: &EvalCodingAgentArgs,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_id: String,
    run_dir: PathBuf,
) -> RunReport {
    if let Err(error) = prepare_run_dir_for_trial(&run_dir) {
        return error_report(run_id, fixture, selector, tool_format, run_dir, error);
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
    let argv = script_argv(args, &fixture, &selector, &tool_format, &run_dir);
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
        return empty_run_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            "infra_error",
            false,
            None,
            outcome.exit_code,
            Some("benchmark harness produced no summary JSON".to_string()),
            elapsed_ms,
            outcome.stderr,
            local_cleanup,
        );
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

impl EvalPackLiveExecutor for CodingAgentLiveExecutor<'_> {
    fn execute(
        &mut self,
        _request: EvalPackLiveExecutorRequest,
    ) -> Result<EvalPackLiveVerifyOutcome, harn_vm::value::VmError> {
        let report = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                execute_coding_agent_trial(
                    self.args,
                    self.fixture.clone(),
                    self.selector.clone(),
                    self.tool_format.clone(),
                    self.run_id.clone(),
                    self.run_dir.clone(),
                )
                .await
            })
        });
        let outcome = live_verify_outcome_from_run_report(&report);
        self.report = Some(report);
        Ok(outcome)
    }
}

fn live_verify_outcome_from_run_report(report: &RunReport) -> EvalPackLiveVerifyOutcome {
    let mut failures = Vec::new();
    if !report.passed && !report.skipped {
        failures.push(
            report
                .error
                .clone()
                .unwrap_or_else(|| "coding-agent fixture failed".to_string()),
        );
    }
    EvalPackLiveVerifyOutcome {
        verification: Some(
            if report.skipped {
                "skip"
            } else if report.passed {
                "PASS"
            } else {
                "FAIL"
            }
            .to_string(),
        ),
        verification_exit_code: Some(report.harn_exit_code as i64),
        passed: Some(report.passed),
        timed_out: false,
        wall_time_seconds: report.elapsed_ms as f64 / 1000.0,
        cost_usd: report.cost_usd,
        produced_paths: ["summary.json", "result.json", "transcript_events.jsonl"]
            .into_iter()
            .filter(|path| Path::new(&report.output_dir).join(path).exists())
            .map(str::to_string)
            .collect(),
        tool_call_summary: serde_json::json!({
            "total": report.tool_calls,
            "rejected": report.rejected_tool_calls,
            "sequence": report.tool_sequence.clone(),
            "successful": report.successful_tools.clone(),
        }),
        failures,
        run_id: Some(report.run_id.clone()),
        workflow_id: Some(CODING_AGENT_EVAL_PACK_ID.to_string()),
        source_path: Some(report.transcript_events_path.clone()),
        stage_count: Some(report.transcript_event_count),
        ..EvalPackLiveVerifyOutcome::default()
    }
}

fn coding_agent_eval_pack_manifest(
    run_dir: &Path,
    fixture: &EvalPackCase,
    selector: &ModelSelector,
    tool_format: &str,
) -> Result<EvalPackManifest, String> {
    let mut case = fixture.clone();
    case.workspace = Some(".".to_string());
    case.case_fingerprint = eval_pack_case_fingerprint(&case).map_err(|error| error.to_string())?;
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "harness".to_string(),
        JsonValue::String(CODING_AGENT_EVAL_PACK_ID.to_string()),
    );
    metadata.insert(
        "model".to_string(),
        JsonValue::String(format!("{}:{tool_format}", selector_label(selector))),
    );
    metadata.insert(
        "provider".to_string(),
        JsonValue::String(selector.provider.clone()),
    );
    metadata.insert(
        "provider_model".to_string(),
        JsonValue::String(selector.model.clone()),
    );
    metadata.insert(
        "tool_format".to_string(),
        JsonValue::String(tool_format.to_string()),
    );
    Ok(EvalPackManifest {
        version: 1,
        id: CODING_AGENT_EVAL_PACK_ID.to_string(),
        name: Some("Coding Agent Harness Quality Suite".to_string()),
        base_dir: Some(run_dir.display().to_string()),
        executor: Some(coding_agent_executor_spec()),
        trials: 1,
        cases: vec![case],
        metadata,
        ..EvalPackManifest::default()
    })
}

fn coding_agent_executor_spec() -> EvalPackCommandSpec {
    EvalPackCommandSpec::Object(EvalPackCommandObject {
        command: Some("harn-coding-agent-suite-in-process".to_string()),
        ..EvalPackCommandObject::default()
    })
}

fn prepare_run_dir_for_trial(run_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(run_dir).map_err(|error| error.to_string())?;
    for file in [
        "coding_agent_suite.harn",
        "summary.json",
        "result.json",
        "transcript_events.jsonl",
    ] {
        let path = run_dir.join(file);
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
        }
    }
    let workspace = run_dir.join("workspace");
    if workspace.exists() {
        fs::remove_dir_all(&workspace)
            .map_err(|error| format!("failed to remove {}: {error}", workspace.display()))?;
    }
    Ok(())
}

pub(super) fn tool_format_override_warning_line(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(TOOL_FORMAT_OVERRIDE_WARNING_PREFIX))
}

fn report_from_existing_summary(
    run_id: String,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
) -> RunReport {
    let Some(summary) = read_run_summary(&run_dir) else {
        return error_report(
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            "eval pack skipped a completed ledger cell, but summary.json was missing".to_string(),
        );
    };
    let exit_code = i32::from(
        !summary
            .get("passed")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
    );
    let elapsed_ms = summary
        .get("duration_ms")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    report_from_summary(
        RunSummaryContext {
            run_id,
            fixture,
            selector,
            tool_format,
            run_dir,
            elapsed_ms,
            exit_code,
            stderr: String::new(),
            local_cleanup: None,
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
        fixture_id: fixture_id(&ctx.fixture).to_string(),
        fixture_name: fixture_name(&ctx.fixture),
        fixture_tool_sequence: fixture_tool_sequence(&ctx.fixture),
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

fn error_report(
    run_id: String,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    error: String,
) -> RunReport {
    empty_run_report(
        run_id,
        fixture,
        selector,
        tool_format,
        run_dir,
        "infra_error",
        false,
        None,
        1,
        Some(error),
        0,
        String::new(),
        None,
    )
}

fn skipped_report(
    run_id: String,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    reason: String,
) -> RunReport {
    empty_run_report(
        run_id,
        fixture,
        selector,
        tool_format,
        run_dir,
        "skipped",
        true,
        Some(reason),
        0,
        None,
        0,
        String::new(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn empty_run_report(
    run_id: String,
    fixture: EvalPackCase,
    selector: ModelSelector,
    tool_format: String,
    run_dir: PathBuf,
    status: &str,
    skipped: bool,
    skipped_reason: Option<String>,
    harn_exit_code: i32,
    error: Option<String>,
    elapsed_ms: u64,
    stderr: String,
    local_cleanup: Option<LocalCleanupReport>,
) -> RunReport {
    RunReport {
        run_id,
        fixture_id: fixture_id(&fixture).to_string(),
        fixture_name: fixture_name(&fixture),
        fixture_tool_sequence: fixture_tool_sequence(&fixture),
        selector,
        tool_format,
        status: status.to_string(),
        passed: false,
        skipped,
        skipped_reason,
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
        harn_exit_code,
        error,
        stderr_excerpt: excerpt(&stderr),
        local_cleanup,
    }
}

fn provider_available(selector: &ModelSelector) -> bool {
    if matches!(selector.provider.as_str(), "mock" | "fake") || selector_is_local(selector) {
        return true;
    }
    harn_vm::llm_config::provider_key_available(&selector.provider)
}
