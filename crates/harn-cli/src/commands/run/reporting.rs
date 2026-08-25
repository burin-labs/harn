//! Auxiliary run output, profiling, and provenance receipts.
//!
//! The execution path owns when these artifacts are emitted; this module owns
//! their wire shapes and sinks so the runner itself stays focused on lifecycle.

use std::fs;
#[cfg(unix)]
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::commands::time::{self, PhaseRecord, RunTiming};
use harn_vm::clock::{now_wall_ms, RealClock};
use harn_vm::event_log::EventLog;
use harn_vm::llm::usage::{summarize_usage_cost_certainty, UsageCostCertainty};

use super::{RunAttestationOptions, RunProfileOptions};

/// JSON event-stream configuration for `--json` runs.
#[derive(Clone, Default)]
pub struct RunJsonOptions {
    /// Suppress `stdout` / `stderr` events. Transcript, tool, hook,
    /// persona, and the terminal result/error events still flow.
    pub quiet: bool,
}

/// Post-run summary configuration for `harn run --emit-summary-json`.
#[derive(Clone, Debug)]
pub struct RunSummaryOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug)]
pub struct RunPhaseOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug)]
pub struct RunRusageOptions {
    pub sink: RunJsonSink,
}

#[derive(Clone, Debug, Default)]
pub struct RunAuxOptions {
    pub summary: Option<RunSummaryOptions>,
    pub phase: Option<RunPhaseOptions>,
    pub rusage: Option<RunRusageOptions>,
}

#[derive(Clone, Debug, Default)]
pub struct RunControlOptions {
    pub timeout: Option<Duration>,
    pub eager_project_handlers: bool,
    pub project_context: super::ProjectContextMode,
}

#[derive(Clone, Debug)]
pub struct RunJsonSink {
    pub target: RunJsonSinkTarget,
    pub fd_flag: &'static str,
}

#[derive(Clone, Debug)]
pub enum RunJsonSinkTarget {
    /// Append the summary to the captured stderr buffer so it remains
    /// terminal after all diagnostics that `run_file_with_skill_dirs`
    /// flushes on return.
    Stderr,
    File(PathBuf),
    Fd(i32),
}

#[derive(Serialize)]
struct RunSummary<'a> {
    schema_version: u32,
    event: &'static str,
    wall_time_ms: u64,
    exit_code: i32,
    llm: RunSummaryLlm,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a harn_vm::profile::RunProfile>,
}

#[derive(Serialize)]
pub(super) struct RunSummaryLlm {
    /// Completed logical LLM operations retained for wire compatibility.
    call_count: i64,
    /// Physical provider requests, including schema, transport, and content retries.
    provider_call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    time_ms: i64,
    /// Exact total when every call is priced.
    cost_usd: Option<f64>,
    /// Sum of priced calls; a lower bound when `unpriced_calls` is non-zero.
    known_cost_usd: f64,
    /// Calls the catalog prices no rate for. A non-zero count makes
    /// `cost_usd` null and `known_cost_usd` the explicit lower bound.
    unpriced_calls: i64,
    /// Physical provider requests whose token/cache usage is unknown.
    usage_unknown_calls: i64,
}

/// v4 distinguishes logical operations from physical provider requests and
/// retains unknown-usage counts alongside exact or lower-bound cost.
pub const RUN_SUMMARY_SCHEMA_VERSION: u32 = 4;
pub const RUN_PHASE_SCHEMA_VERSION: u32 = 2;
pub const RUN_RUSAGE_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct RunPhaseEvent {
    schema_version: u32,
    event: &'static str,
    phases: Vec<PhaseRecord>,
}

#[derive(Serialize)]
struct RunRusageEvent {
    schema_version: u32,
    event: &'static str,
    cpu_ms: u64,
}

fn run_summary_options_from_args(args: &crate::cli::RunArgs) -> Option<RunSummaryOptions> {
    args.emit_summary_json.then(|| RunSummaryOptions {
        sink: build_run_json_sink(args.summary_file.clone(), args.summary_fd, "--summary-fd"),
    })
}

pub(crate) fn run_aux_options_from_args(args: &crate::cli::RunArgs) -> RunAuxOptions {
    RunAuxOptions {
        summary: run_summary_options_from_args(args),
        phase: run_phase_options_from_args(args),
        rusage: run_rusage_options_from_args(args),
    }
}

pub(crate) fn run_control_options_from_args(args: &crate::cli::RunArgs) -> RunControlOptions {
    RunControlOptions {
        timeout: args.timeout,
        eager_project_handlers: args.eager_project_handlers,
        project_context: if args.standalone {
            super::ProjectContextMode::Standalone
        } else {
            super::ProjectContextMode::Project
        },
    }
}

fn run_phase_options_from_args(args: &crate::cli::RunArgs) -> Option<RunPhaseOptions> {
    args.emit_phase_json.then(|| RunPhaseOptions {
        sink: build_run_json_sink(args.phase_file.clone(), args.phase_fd, "--phase-fd"),
    })
}

fn run_rusage_options_from_args(args: &crate::cli::RunArgs) -> Option<RunRusageOptions> {
    args.emit_rusage_json.then(|| RunRusageOptions {
        sink: build_run_json_sink(args.rusage_file.clone(), args.rusage_fd, "--rusage-fd"),
    })
}

fn build_run_json_sink(
    file: Option<PathBuf>,
    fd: Option<i32>,
    fd_flag: &'static str,
) -> RunJsonSink {
    RunJsonSink {
        target: if let Some(path) = file {
            RunJsonSinkTarget::File(path)
        } else if let Some(fd) = fd {
            RunJsonSinkTarget::Fd(fd)
        } else {
            RunJsonSinkTarget::Stderr
        },
        fd_flag,
    }
}

pub(super) fn render_and_persist_profile_rollup(
    options: &RunProfileOptions,
    profile: &harn_vm::profile::RunProfile,
    stderr: &mut String,
) -> Result<(), String> {
    if options.text {
        stderr.push_str(&harn_vm::profile::render(profile));
    }
    if let Some(path) = options.json_path.as_ref() {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {error}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(profile)
            .map_err(|error| format!("serialize profile: {error}"))?;
        fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

fn build_run_summary<'a>(
    started: Instant,
    exit_code: i32,
    profile: Option<&'a harn_vm::profile::RunProfile>,
    llm: RunSummaryLlm,
) -> RunSummary<'a> {
    RunSummary {
        schema_version: RUN_SUMMARY_SCHEMA_VERSION,
        event: "run_summary",
        wall_time_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        exit_code,
        llm,
        profile,
    }
}

pub(super) fn run_summary_llm_snapshot() -> RunSummaryLlm {
    let (input_tokens, output_tokens, time_ms, call_count) = harn_vm::llm::peek_trace_summary();
    let trace = harn_vm::llm::peek_trace();
    let certainty = summarize_usage_cost_certainty(trace.iter().map(|entry| &entry.usage));
    run_summary_llm_from_parts(input_tokens, output_tokens, time_ms, call_count, certainty)
}

fn run_summary_llm_from_parts(
    input_tokens: i64,
    output_tokens: i64,
    time_ms: i64,
    call_count: i64,
    certainty: UsageCostCertainty,
) -> RunSummaryLlm {
    RunSummaryLlm {
        call_count,
        provider_call_count: certainty.provider_call_count,
        input_tokens,
        output_tokens,
        time_ms,
        cost_usd: (certainty.unpriced_calls == 0).then_some(certainty.known_cost_usd),
        known_cost_usd: certainty.known_cost_usd,
        unpriced_calls: certainty.unpriced_calls,
        usage_unknown_calls: certainty.usage_unknown_calls,
    }
}

pub(super) struct RunAuxEmission {
    pub stderr: String,
    pub exit_code: i32,
    pub error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_run_aux_for_exit(
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    exit_code: i32,
    profile: Option<&harn_vm::profile::RunProfile>,
    llm: Option<RunSummaryLlm>,
    timing: Option<&RunTiming>,
    main_events: u64,
    cpu_ms_total: Option<u64>,
    json_mode: bool,
    stderr: &mut String,
) -> RunAuxEmission {
    let mut aux_stderr = String::new();
    let mut final_exit_code = exit_code;
    let mut aux_error = None;
    let aux_target = if json_mode { &mut aux_stderr } else { stderr };
    let default_timing = RunTiming::default();
    let timing = timing.unwrap_or(&default_timing);

    if let Some(options) = summary {
        let llm = llm.unwrap_or_else(run_summary_llm_snapshot);
        let summary = build_run_summary(started, exit_code, profile, llm);
        if let Err(error) = emit_raw_json_line(&options.sink, &summary, "run summary", aux_target) {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run summary",
                error,
            );
        }
    }
    if let Some(options) = phase {
        let phase_event = RunPhaseEvent {
            schema_version: RUN_PHASE_SCHEMA_VERSION,
            event: "run_phase",
            phases: time::build_phase_records(timing, main_events),
        };
        if let Err(error) = emit_raw_json_line(&options.sink, &phase_event, "run phase", aux_target)
        {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run phase",
                error,
            );
        }
    }
    if let Some(options) = rusage {
        let rusage_event = RunRusageEvent {
            schema_version: RUN_RUSAGE_SCHEMA_VERSION,
            event: "run_rusage",
            cpu_ms: cpu_ms_total.unwrap_or(0),
        };
        if let Err(error) =
            emit_raw_json_line(&options.sink, &rusage_event, "run rusage", aux_target)
        {
            record_aux_error(
                &mut final_exit_code,
                &mut aux_error,
                aux_target,
                "run rusage",
                error,
            );
        }
    }

    RunAuxEmission {
        stderr: aux_stderr,
        exit_code: final_exit_code,
        error: aux_error,
    }
}

fn record_aux_error(
    final_exit_code: &mut i32,
    aux_error: &mut Option<String>,
    stderr: &mut String,
    label: &str,
    error: String,
) {
    stderr.push_str(&format!("error: failed to emit {label}: {error}\n"));
    if *final_exit_code == 0 {
        *final_exit_code = 1;
    }
    if aux_error.is_none() {
        *aux_error = Some(error);
    }
}

fn emit_raw_json_line(
    sink: &RunJsonSink,
    value: &impl Serialize,
    label: &str,
    stderr: &mut String,
) -> Result<(), String> {
    let line =
        serde_json::to_string(value).map_err(|error| format!("serialize {label}: {error}"))? + "\n";
    match &sink.target {
        RunJsonSinkTarget::Stderr => {
            stderr.push_str(&line);
            Ok(())
        }
        RunJsonSinkTarget::File(path) => write_raw_json_file(path, &line),
        RunJsonSinkTarget::Fd(fd) => write_raw_json_fd(*fd, &line, sink.fd_flag),
    }
}

fn write_raw_json_file(path: &Path, line: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, line).map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(unix)]
fn write_raw_json_fd(fd: i32, line: &str, flag: &str) -> Result<(), String> {
    use std::fs::File;
    use std::os::unix::io::FromRawFd;

    if fd < 0 {
        return Err(format!("invalid {flag} {fd}: must be non-negative"));
    }
    let duped = unsafe { libc::dup(fd) };
    if duped < 0 {
        return Err(format!(
            "duplicate {flag} {fd}: {}",
            io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { File::from_raw_fd(duped) };
    file.write_all(line.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| format!("write {flag} {fd}: {error}"))
}

#[cfg(not(unix))]
fn write_raw_json_fd(_fd: i32, _line: &str, flag: &str) -> Result<(), String> {
    Err(format!("{flag} is only supported on Unix platforms"))
}

pub(super) async fn append_run_provenance_event(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    kind: &str,
    payload: serde_json::Value,
) {
    let Ok(topic) = harn_vm::event_log::Topic::new("run.provenance") else {
        return;
    };
    let _ = log
        .append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await;
}

pub(super) async fn emit_run_attestation(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    path: &str,
    store_base: &Path,
    started_at_ms: i64,
    exit_code: i32,
    options: &RunAttestationOptions,
    stderr: &mut String,
) -> Result<(), String> {
    let finished_at_ms = now_ms();
    let status = if exit_code == 0 { "success" } else { "failure" };
    append_run_provenance_event(
        log,
        "finished",
        serde_json::json!({
            "pipeline": path,
            "status": status,
            "exit_code": exit_code,
        }),
    )
    .await;
    log.flush()
        .await
        .map_err(|error| format!("failed to flush attestation event log: {error}"))?;
    let secret_provider = harn_vm::secrets::configured_default_chain("harn.provenance")
        .map_err(|error| format!("failed to configure provenance secrets: {error}"))?;
    let (signing_key, key_id) =
        harn_vm::load_or_generate_agent_signing_key(&secret_provider, options.agent_id.as_deref())
            .await
            .map_err(|error| format!("failed to load provenance signing key: {error}"))?;
    let receipt = harn_vm::build_signed_receipt(
        log,
        harn_vm::ReceiptBuildOptions {
            pipeline: path.to_string(),
            status: status.to_string(),
            started_at_ms,
            finished_at_ms,
            exit_code,
            producer_name: "harn-cli".to_string(),
            producer_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        &signing_key,
        key_id,
    )
    .await
    .map_err(|error| format!("failed to build provenance receipt: {error}"))?;
    let receipt_path = receipt_output_path(store_base, options, &receipt.receipt_id);
    if let Some(parent) = receipt_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("failed to encode provenance receipt: {error}"))?;
    fs::write(&receipt_path, encoded)
        .map_err(|error| format!("failed to write {}: {error}", receipt_path.display()))?;
    stderr.push_str(&format!("provenance receipt: {}\n", receipt_path.display()));
    Ok(())
}

fn receipt_output_path(
    store_base: &Path,
    options: &RunAttestationOptions,
    receipt_id: &str,
) -> PathBuf {
    if let Some(path) = options.receipt_out.as_ref() {
        return path.clone();
    }
    harn_vm::runtime_paths::state_root(store_base)
        .join("receipts")
        .join(format!("{receipt_id}.json"))
}

pub(super) fn now_ms() -> i64 {
    now_wall_ms(&RealClock::new())
}

/// Map a script's top-level return value to a process exit code.
///
/// - `int n`             → exit n (clamped to 0..=255)
/// - `Result::Ok(_)`     → exit 0
/// - `Result::Err(_)`    → exit 1
/// - anything else       → exit 0
pub(super) fn exit_code_from_return_value(value: &harn_vm::VmValue) -> i32 {
    use harn_vm::VmValue;
    match value {
        VmValue::Int(n) => (*n).clamp(0, 255) as i32,
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => 1,
        _ => 0,
    }
}

pub(crate) fn render_trace_summary() -> String {
    render_trace_entries(&harn_vm::llm::take_trace())
}

/// Rendering is split from the thread-local read so the money arithmetic can
/// be tested against constructed entries rather than a live provider call.
fn render_trace_entries(entries: &[harn_vm::llm::LlmTraceEntry]) -> String {
    use std::fmt::Write;

    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "\n\x1b[2m─── LLM trace ───\x1b[0m");
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    let mut total_ms = 0u64;
    // Priced and unpriced calls are summed separately. This used to apply one
    // hardcoded Sonnet-4 rate ($3/$15 per MTok) to every call regardless of
    // which model served it, so a trace of any other model reported a
    // confidently wrong dollar figure.
    //
    // The price is now read off the entry rather than recomputed here. The
    // runtime already priced each call through the one owner of per-call cost,
    // which sees prompt-cache accounting and the accelerated-serving tier;
    // re-pricing from tokens alone cannot, and would make this summary
    // disagree with the run's own reported total.
    for (index, entry) in entries.iter().enumerate() {
        let certainty = summarize_usage_cost_certainty([&entry.usage]);
        let _ = writeln!(
            out,
            "  #{}: {} | {} in + {} out tokens | {} ms | {}",
            index + 1,
            entry.model,
            entry.usage.input_tokens,
            entry.usage.output_tokens,
            entry.duration_ms,
            render_cost_certainty(certainty),
        );
        total_input += entry.usage.input_tokens;
        total_output += entry.usage.output_tokens;
        total_ms += entry.duration_ms;
    }
    let total_tokens = total_input + total_output;
    let certainty = summarize_usage_cost_certainty(entries.iter().map(|entry| &entry.usage));
    let token_label = render_token_certainty(total_tokens, certainty.usage_unknown_calls);
    let _ = writeln!(
        out,
        "  \x1b[1m{} logical call{}, {} provider call{}, {} ({}in + {}out), {} ms, {}\x1b[0m",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        certainty.provider_call_count,
        if certainty.provider_call_count == 1 {
            ""
        } else {
            "s"
        },
        token_label,
        total_input,
        total_output,
        total_ms,
        render_cost_certainty(certainty),
    );
    out
}

fn render_cost_certainty(certainty: UsageCostCertainty) -> String {
    if certainty.unpriced_calls == 0 {
        format!("${:.4}", certainty.known_cost_usd)
    } else {
        // This is a floor on real spend, not an estimate. The runtime's
        // canonical ledger already includes priced siblings from a mixed
        // retry aggregate in `known_cost_usd`.
        format!(
            "≥${:.4} ({} unpriced)",
            certainty.known_cost_usd, certainty.unpriced_calls
        )
    }
}

fn render_token_certainty(known_tokens: i64, usage_unknown_calls: i64) -> String {
    if usage_unknown_calls == 0 {
        format!("{known_tokens} tokens")
    } else {
        format!("≥{known_tokens} tokens ({usage_unknown_calls} usage unknown)")
    }
}

#[cfg(test)]
mod trace_summary_pricing_tests {
    use super::{render_trace_entries, run_summary_llm_from_parts};
    use harn_vm::llm::{
        usage::{LlmUsage, UsageCostCertainty},
        LlmTraceEntry,
    };

    fn entry(model: &str, cost_usd: Option<f64>) -> LlmTraceEntry {
        LlmTraceEntry {
            model: model.to_string(),
            provider: "anthropic".to_string(),
            usage: LlmUsage {
                input_tokens: 1_000,
                output_tokens: 100,
                cost_usd,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cache_supported: true,
                cache_accounting_declared: Some(true),
                cache_hit_ratio: Some(0.0),
                cache_savings_usd: 0.0,
                cache_hit: false,
                served_fast: false,
                accounting_status: harn_vm::llm::usage::UsageAccountingStatus::Reported,
                known_cost_usd: cost_usd.unwrap_or(0.0),
                provider_call_count: 1,
                unpriced_calls: i64::from(cost_usd.is_none()),
                usage_unknown_calls: 0,
            },
            duration_ms: 5,
        }
    }

    /// The summary used to price every call at one hardcoded Sonnet-4 rate
    /// ($3/$15 per MTok) regardless of which model served it. It now sums the
    /// price the runtime already computed, so the total is whatever the run
    /// actually booked.
    #[test]
    fn the_total_is_the_sum_of_the_prices_the_runtime_recorded() {
        let rendered = render_trace_entries(&[
            entry("claude-sonnet-4-20250514", Some(0.25)),
            entry("claude-haiku-4-5-20251001", Some(0.0125)),
        ]);
        assert!(
            rendered.contains("$0.2625"),
            "the total must be the exact sum of the recorded prices: {rendered}"
        );
        assert!(
            !rendered.contains("unpriced"),
            "no call was unpriced, so nothing should be hedged: {rendered}"
        );
    }

    /// An unpriced call must not be silently treated as free. The total
    /// becomes a floor, and says how many calls it could not account for.
    #[test]
    fn an_unpriced_call_makes_the_total_a_floor_rather_than_a_figure() {
        let rendered = render_trace_entries(&[
            entry("claude-sonnet-4-20250514", Some(0.25)),
            entry("some-model-the-catalog-does-not-price", None),
        ]);
        assert!(
            rendered.contains("\u{2265}$0.2500"),
            "a partially priced total must be marked as a floor: {rendered}"
        );
        assert!(
            rendered.contains("(1 unpriced)"),
            "the count of unaccounted calls must be stated: {rendered}"
        );
        assert!(
            rendered.contains("unpriced"),
            "the unpriced call's own row must say so: {rendered}"
        );
    }

    /// Two models that priced differently must not collapse to one number.
    /// That equality was the shape of the original bug.
    #[test]
    fn two_models_priced_differently_do_not_collapse_to_one_number() {
        let rendered = render_trace_entries(&[
            entry("claude-sonnet-4-20250514", Some(0.2500)),
            entry("claude-haiku-4-5-20251001", Some(0.0125)),
        ]);
        assert!(
            rendered.contains("$0.2500") && rendered.contains("$0.0125"),
            "each call must show its own price: {rendered}"
        );
    }

    #[test]
    fn mixed_retry_keeps_known_spend_and_physical_uncertainty() {
        let mut retried = entry("retrying-model", None);
        retried.usage.input_tokens = 7;
        retried.usage.output_tokens = 5;
        retried.usage.known_cost_usd = 0.0123;
        retried.usage.provider_call_count = 2;
        retried.usage.unpriced_calls = 1;
        retried.usage.usage_unknown_calls = 1;

        let rendered = render_trace_entries(&[retried]);
        assert!(
            rendered.contains("1 logical call, 2 provider calls"),
            "physical retries must not collapse to one logical trace row: {rendered}"
        );
        assert!(
            rendered.contains("≥12 tokens (1 usage unknown)"),
            "known tokens must be labeled as a lower bound: {rendered}"
        );
        assert!(
            rendered.matches("≥$0.0123 (1 unpriced)").count() >= 2,
            "both the row and total must retain the priced sibling's lower bound: {rendered}"
        );
    }

    #[test]
    fn json_summary_projects_the_canonical_physical_certainty() {
        let summary = run_summary_llm_from_parts(
            7,
            5,
            9,
            1,
            UsageCostCertainty {
                known_cost_usd: 0.0123,
                provider_call_count: 2,
                unpriced_calls: 1,
                usage_unknown_calls: 1,
            },
        );
        assert_eq!(summary.call_count, 1);
        assert_eq!(summary.provider_call_count, 2);
        assert_eq!(summary.cost_usd, None);
        assert_eq!(summary.known_cost_usd, 0.0123);
        assert_eq!(summary.unpriced_calls, 1);
        assert_eq!(summary.usage_unknown_calls, 1);
    }
}
