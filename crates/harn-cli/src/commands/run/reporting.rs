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
    call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    time_ms: i64,
    cost_usd: f64,
}

pub const RUN_SUMMARY_SCHEMA_VERSION: u32 = 1;
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
    let cost_usd = harn_vm::llm::peek_total_cost();
    RunSummaryLlm {
        call_count,
        input_tokens,
        output_tokens,
        time_ms,
        cost_usd: if cost_usd.is_finite() { cost_usd } else { 0.0 },
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
    use std::fmt::Write;

    let entries = harn_vm::llm::take_trace();
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "\n\x1b[2m─── LLM trace ───\x1b[0m");
    let mut total_input = 0i64;
    let mut total_output = 0i64;
    let mut total_ms = 0u64;
    for (index, entry) in entries.iter().enumerate() {
        let _ = writeln!(
            out,
            "  #{}: {} | {} in + {} out tokens | {} ms",
            index + 1,
            entry.model,
            entry.input_tokens,
            entry.output_tokens,
            entry.duration_ms,
        );
        total_input += entry.input_tokens;
        total_output += entry.output_tokens;
        total_ms += entry.duration_ms;
    }
    let total_tokens = total_input + total_output;
    // Rough cost estimate using Sonnet 4 pricing ($3/MTok in, $15/MTok out).
    let cost = (total_input as f64 * 3.0 + total_output as f64 * 15.0) / 1_000_000.0;
    let _ = writeln!(
        out,
        "  \x1b[1m{} call{}, {} tokens ({}in + {}out), {} ms, ~${:.4}\x1b[0m",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        total_tokens,
        total_input,
        total_output,
        total_ms,
        cost,
    );
    out
}
