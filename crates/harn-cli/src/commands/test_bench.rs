//! `harn test-bench` runner.
//!
//! Wraps [`crate::commands::run::execute_run`] in a
//! [`harn_vm::testbench::TestbenchSession`] so a script runs against a
//! pinned clock, an optional LLM/process tape, and an optional
//! filesystem overlay — all with deny-by-default network egress.
//!
//! The CLI flag names map onto [`harn_vm::testbench::Testbench`] one-for-one.
//!
//! # Runtime modes
//!
//! `--runtime paused-tokio` (default): multi-threaded Tokio runtime. Tasks
//! from concurrent Harn agents run in parallel across worker threads. The
//! paused mock clock keeps virtual time stable, but task-interleaving order
//! varies between runs.
//!
//! `--runtime des`: single-threaded `current_thread` Tokio runtime. All
//! tasks, I/O completions, and timer callbacks share one OS thread. Combined
//! with the paused mock clock this produces bit-exact event tapes across
//! reruns for scripts that stay within the DES-safe primitive set (no real
//! network, no real subprocess, no real clock). See `docs/src/dev/des-mode.md`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;

use harn_vm::testbench::annotations::{
    annotations_for_record, validate_against_tape, AnnotationKind, AnnotationTape,
};
use harn_vm::testbench::fidelity::{compare, FidelityMode, FidelityReport};
use harn_vm::testbench::overlay_fs::{render_unified_diff, DiffEntry, DiffKind};
use harn_vm::testbench::tape::EventTape;
use harn_vm::testbench::{
    ClockConfig, FilesystemConfig, LlmConfig, NetworkConfig, SubprocessConfig, TapeConfig,
    Testbench,
};

use crate::cli::{
    TestBenchCommand, TestBenchExportAnnotationsArgs, TestBenchFidelityArgs, TestBenchReplayArgs,
    TestBenchRunArgs, TestBenchValidateAnnotationsArgs,
};
use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use crate::CLI_RUNTIME_STACK_SIZE;

/// Default starting point for `--clock paused` runs. Picked to be
/// stable, RFC-3339-friendly, and after every prerequisite Y2K38
/// boundary so date-of-birth math doesn't underflow:
/// 2026-01-01T00:00:00Z.
const DEFAULT_TESTBENCH_START_MS: i64 = 1_767_225_600_000;

/// Where the replay tape used by `harn test-bench fidelity` came from.
enum ReplaySource {
    /// Re-run the script under `--against` and emit a fresh tape.
    ReRun,
    /// Load an existing tape from disk.
    Tape(String),
}

pub(crate) async fn run(command: TestBenchCommand) {
    let outcome = match command {
        TestBenchCommand::Run(args) => run_args(args).await,
        TestBenchCommand::Replay(args) => replay_args(args).await,
        TestBenchCommand::Fidelity(args) => fidelity_args(args).await,
        TestBenchCommand::ValidateAnnotations(args) => validate_annotations_args(args),
        TestBenchCommand::ExportAnnotations(args) => export_annotations_args(args),
    };
    flush_outcome(outcome);
}

async fn run_args(args: TestBenchRunArgs) -> RunOutcome {
    let bench = match build_testbench(&args) {
        Ok(bench) => bench,
        Err(message) => return error_outcome(message),
    };
    let llm_mode = match build_llm_mode(&args) {
        Ok(mode) => mode,
        Err(message) => return error_outcome(message),
    };
    match args.runtime.as_str() {
        "paused-tokio" | "" => run_with_bench(args, bench, llm_mode).await,
        "des" => run_with_des_runtime(args, bench, llm_mode).await,
        other => error_outcome(format!(
            "--runtime must be `paused-tokio` or `des`, got `{other}`"
        )),
    }
}

/// Execute the script under a standard multi-thread Tokio runtime with the
/// testbench mocks already active on the calling async task.
async fn run_with_bench(
    args: TestBenchRunArgs,
    bench: Testbench,
    llm_mode: CliLlmMockMode,
) -> RunOutcome {
    let session = match bench.activate() {
        Ok(session) => session,
        Err(error) => return error_outcome(format!("activate testbench: {error}")),
    };
    let outcome = execute_run(
        &args.file,
        false,
        HashSet::new(),
        args.argv.clone(),
        Vec::new(),
        llm_mode,
        None,
        RunProfileOptions::default(),
    )
    .await;
    finalize_session(outcome, session, &args)
}

/// Execute the script under a **single-threaded** `current_thread` Tokio
/// runtime for maximum inter-task scheduling determinism.
///
/// Spawns a fresh OS thread so we can call `Runtime::block_on` without
/// nesting inside the caller's multi-thread runtime. The stack size is
/// matched to the main CLI thread so deep recursion in scripts works.
/// Thread-local testbench mocks (clock, overlay, process tape, recorder)
/// are installed inside the new thread so they are visible to every task
/// that runs there.
///
/// The `current_thread` scheduler cooperatively multiplexes all tasks on one
/// OS thread, eliminating the inter-thread wake-up races that cause tape
/// records to appear in different orders between runs. Combined with the
/// paused mock clock this yields bit-exact event tapes for DES-safe scripts.
async fn run_with_des_runtime(
    args: TestBenchRunArgs,
    bench: Testbench,
    llm_mode: CliLlmMockMode,
) -> RunOutcome {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("harn-des".to_string())
        .stack_size(CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| panic!("failed to build DES runtime: {e}"));
            let outcome = rt.block_on(async move {
                harn_vm::reset_thread_local_state();
                let session = match bench.activate() {
                    Ok(s) => s,
                    Err(e) => return error_outcome(format!("activate testbench: {e}")),
                };
                let outcome = execute_run(
                    &args.file,
                    false,
                    HashSet::new(),
                    args.argv.clone(),
                    Vec::new(),
                    llm_mode,
                    None,
                    RunProfileOptions::default(),
                )
                .await;
                finalize_session(outcome, session, &args)
            });
            let _ = tx.send(outcome);
        })
        .expect("spawn DES thread");
    tokio::task::spawn_blocking(move || {
        rx.recv()
            .unwrap_or_else(|_| error_outcome("DES runtime thread panicked".to_string()))
    })
    .await
    .unwrap_or_else(|e| error_outcome(format!("DES runtime blocking task failed: {e:?}")))
}

fn build_llm_mode(args: &TestBenchRunArgs) -> Result<CliLlmMockMode, String> {
    match (&args.llm_fixture, &args.llm_record) {
        (Some(_), Some(_)) => Err("--llm-fixture and --llm-record are mutually exclusive".into()),
        (Some(path), None) => Ok(CliLlmMockMode::Replay {
            fixture_path: PathBuf::from(path),
        }),
        (None, Some(path)) => Ok(CliLlmMockMode::Record {
            fixture_path: PathBuf::from(path),
        }),
        (None, None) => Ok(CliLlmMockMode::Off),
    }
}

fn finalize_session(
    outcome: RunOutcome,
    session: harn_vm::testbench::TestbenchSession,
    args: &TestBenchRunArgs,
) -> RunOutcome {
    let finalize = match session.finalize() {
        Ok(f) => f,
        Err(error) => return append_error(outcome, format!("finalize testbench: {error}")),
    };
    let mut outcome = outcome;
    if matches!(args.network.as_str(), "deny") {
        outcome
            .stderr
            .push_str("[testbench] network=deny applied for the duration of the run.\n");
    }
    if let Some(diff_path) = args.emit_diff.as_ref() {
        if let Err(error) = persist_overlay_diff(&finalize.fs_diff, &PathBuf::from(diff_path)) {
            outcome.stderr.push_str(&format!(
                "warning: failed to write fs diff to {diff_path}: {error}\n"
            ));
        }
    } else if !finalize.fs_diff.is_empty() {
        outcome
            .stderr
            .push_str(&render_diff_summary(&finalize.fs_diff));
    }
    if let Some(record_path) = args.process_record.as_ref() {
        outcome.stderr.push_str(&format!(
            "[testbench] recorded {} subprocess invocation(s) to {record_path}.\n",
            finalize.recorded_subprocesses.len()
        ));
    }
    if let Some(toolchain_dir) = args.process_wasi.as_ref() {
        outcome.stderr.push_str(&format!(
            "[testbench] subprocess invocations resolved against WASI toolchain at {toolchain_dir}.\n"
        ));
    }
    if let Some(tape) = finalize.tape.as_ref() {
        outcome.stderr.push_str(&format!(
            "[testbench] emitted unified tape with {} record(s) to {}.\n",
            tape.records,
            tape.path.display(),
        ));
    }
    for leak in &finalize.clock_leaks {
        outcome.stderr.push_str(&format!(
            "[testbench] clock leak: {} (count={})\n",
            leak.capability_id, leak.count,
        ));
    }
    outcome
}

async fn replay_args(args: TestBenchReplayArgs) -> RunOutcome {
    // Load + pre-validate annotations before running so a malformed
    // sidecar fails fast and the run output stays focused on the script.
    let annotations_loaded = match args.annotations.as_deref() {
        None => None,
        Some(path) => match AnnotationTape::load(Path::new(path)) {
            Ok(tape) => Some((path.to_string(), tape)),
            Err(error) => {
                return error_outcome(format!("load annotations {path}: {error}"));
            }
        },
    };

    // Surfacing annotations during replay requires the emitted tape so
    // we can resolve `event_id` → record. When the caller did not ask
    // for `--emit-tape`, allocate a temp file and persist the tape
    // there for the duration of the call.
    let tape_temp = if annotations_loaded.is_some() && args.emit_tape.is_none() {
        match tempfile::tempdir() {
            Ok(dir) => Some(dir),
            Err(error) => return error_outcome(format!("tempdir for replay tape: {error}")),
        }
    } else {
        None
    };
    let emit_tape_path = match (&args.emit_tape, tape_temp.as_ref()) {
        (Some(path), _) => Some(path.clone()),
        (None, Some(dir)) => Some(dir.path().join("run.tape").to_string_lossy().into_owned()),
        (None, None) => None,
    };

    let derived = TestBenchRunArgs {
        file: args.file.clone(),
        start_at_ms: args.start_at_ms,
        clock: "paused".to_string(),
        llm_fixture: args.llm_fixture.clone(),
        llm_record: None,
        fs_overlay: args.fs_overlay.clone(),
        process_replay: Some(args.process_tape.clone()),
        process_record: None,
        process_wasi: None,
        network: "deny".to_string(),
        allow_host: Vec::new(),
        emit_diff: None,
        emit_tape: emit_tape_path.clone(),
        runtime: "paused-tokio".to_string(),
        argv: args.argv.clone(),
    };
    let mut outcome = run_args(derived).await;

    if let (Some((annotations_path, annotations)), Some(tape_path)) =
        (annotations_loaded, emit_tape_path)
    {
        match EventTape::load(Path::new(&tape_path)) {
            Ok(tape) => {
                let report = validate_against_tape(&annotations, &tape);
                outcome.stderr.push_str(&render_annotations_block(
                    &annotations_path,
                    &annotations,
                    &tape,
                ));
                if !report.is_ok() {
                    outcome.stderr.push_str(&format!(
                        "[testbench] annotations validation failed with {} problem(s); see `harn test-bench validate-annotations` for the structured report.\n",
                        report.problems.len()
                    ));
                    outcome.exit_code = outcome.exit_code.max(2);
                }
            }
            Err(error) => {
                outcome.stderr.push_str(&format!(
                    "warning: failed to load tape for annotation surfacing: {error}\n"
                ));
            }
        }
    }
    outcome
}

/// Render a "[annotations]" stderr block grouping every annotation by
/// the tape event it targets. Output is deterministic (sorted by `seq`)
/// so it diffs cleanly across reruns.
fn render_annotations_block(
    annotations_path: &str,
    annotations: &AnnotationTape,
    tape: &EventTape,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[annotations] loaded {} annotation(s) from {annotations_path}\n",
        annotations.annotations.len()
    ));
    let mut sorted_records: Vec<_> = tape.records.iter().collect();
    sorted_records.sort_by_key(|record| record.seq);
    for record in sorted_records {
        let matches = annotations_for_record(annotations, record);
        if matches.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "  event seq={} virtual_time_ms={} kind={}\n",
            record.seq,
            record.virtual_time_ms,
            record.kind.label(),
        ));
        for annotation in matches {
            let label = annotation.kind.as_str();
            let evidence = annotation
                .evidence
                .as_deref()
                .unwrap_or("(no evidence)")
                .lines()
                .next()
                .unwrap_or("(no evidence)");
            let id = if annotation.id.is_empty() {
                "(no id)".to_string()
            } else {
                annotation.id.clone()
            };
            out.push_str(&format!("    [{label}] {id}: {evidence}\n"));
        }
    }
    out
}

fn validate_annotations_args(args: TestBenchValidateAnnotationsArgs) -> RunOutcome {
    let tape = match EventTape::load(Path::new(&args.tape)) {
        Ok(tape) => tape,
        Err(error) => return error_outcome(format!("load tape {}: {error}", args.tape)),
    };
    let annotations = match AnnotationTape::load(Path::new(&args.annotations)) {
        Ok(tape) => tape,
        Err(error) => {
            return error_outcome(format!("load annotations {}: {error}", args.annotations));
        }
    };
    let report = validate_against_tape(&annotations, &tape);
    let json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => return error_outcome(format!("serialize validation report: {error}")),
    };
    let mut outcome = RunOutcome::default();
    if let Some(path) = args.report.as_deref() {
        if let Err(error) = persist_text(&json, Path::new(path)) {
            return error_outcome(format!("write validation report: {error}"));
        }
        outcome.stderr.push_str(&format!(
            "[testbench] annotations validation: checked={} problems={} ({})\n",
            report.annotations_checked,
            report.problems.len(),
            path,
        ));
    } else {
        outcome.stdout.push_str(&json);
        outcome.stdout.push('\n');
    }
    if !report.is_ok() {
        outcome.exit_code = 2;
    }
    outcome
}

fn export_annotations_args(args: TestBenchExportAnnotationsArgs) -> RunOutcome {
    let annotations = match AnnotationTape::load(Path::new(&args.annotations)) {
        Ok(tape) => tape,
        Err(error) => {
            return error_outcome(format!("load annotations {}: {error}", args.annotations));
        }
    };

    let kinds: Vec<AnnotationKind> = if args.kind.is_empty() {
        Vec::new()
    } else {
        let mut parsed = Vec::with_capacity(args.kind.len());
        for raw in &args.kind {
            match AnnotationKind::parse_cli(raw) {
                Ok(kind) => parsed.push(kind),
                Err(error) => return error_outcome(error),
            }
        }
        parsed
    };

    let selected: Vec<_> = annotations
        .annotations
        .iter()
        .filter(|annotation| kinds.is_empty() || kinds.contains(&annotation.kind))
        .collect();

    let body = match args.format.as_str() {
        "jsonl" | "" => {
            let mut out = String::new();
            for annotation in &selected {
                match serde_json::to_string(annotation) {
                    Ok(line) => {
                        out.push_str(&line);
                        out.push('\n');
                    }
                    Err(error) => {
                        return error_outcome(format!("serialize annotation: {error}"));
                    }
                }
            }
            out
        }
        "friction" => {
            let mut out = String::new();
            for annotation in &selected {
                if let Some(event) = harn_vm::testbench::annotations::annotation_to_friction_event(
                    annotation,
                    &annotations.header,
                ) {
                    match serde_json::to_string(&event) {
                        Ok(line) => {
                            out.push_str(&line);
                            out.push('\n');
                        }
                        Err(error) => {
                            return error_outcome(format!("serialize friction event: {error}"));
                        }
                    }
                }
            }
            out
        }
        other => {
            return error_outcome(format!(
                "--format must be `jsonl` or `friction`, got `{other}`"
            ));
        }
    };

    let mut outcome = RunOutcome::default();
    if let Some(path) = args.output.as_deref() {
        if let Err(error) = persist_text(&body, Path::new(path)) {
            return error_outcome(format!("write export: {error}"));
        }
        outcome.stderr.push_str(&format!(
            "[testbench] exported {} annotation(s) to {} (format={})\n",
            selected.len(),
            path,
            args.format,
        ));
    } else {
        outcome.stdout.push_str(&body);
    }
    outcome
}

async fn fidelity_args(args: TestBenchFidelityArgs) -> RunOutcome {
    let mode = match FidelityMode::parse(&args.mode) {
        Ok(mode) => mode,
        Err(error) => return error_outcome(error),
    };

    let (recorded_path, replay_source) = match (&args.against, &args.replay) {
        (Some(recorded), _) => (recorded.clone(), ReplaySource::ReRun),
        (None, Some(replay)) => (args.primary.clone(), ReplaySource::Tape(replay.clone())),
        (None, None) => {
            return error_outcome(
                "expected either two tape paths or `--against <tape> <script>`".to_string(),
            )
        }
    };

    let recorded = match EventTape::load(Path::new(&recorded_path)) {
        Ok(tape) => tape,
        Err(error) => return error_outcome(format!("load recorded tape: {error}")),
    };

    let (replay, mut prelude) = match replay_source {
        ReplaySource::ReRun => {
            let temp = match tempfile::tempdir() {
                Ok(dir) => dir,
                Err(error) => return error_outcome(format!("create temp tape dir: {error}")),
            };
            let replay_tape_path = temp.path().join("replay.tape");
            let start_at = args
                .start_at_ms
                .or(recorded.header.started_at_unix_ms)
                .unwrap_or(DEFAULT_TESTBENCH_START_MS);
            let derived = TestBenchRunArgs {
                file: args.primary.clone(),
                start_at_ms: Some(start_at),
                clock: "paused".to_string(),
                llm_fixture: None,
                llm_record: None,
                fs_overlay: args.fs_overlay.clone(),
                process_replay: None,
                process_record: None,
                process_wasi: None,
                network: "deny".to_string(),
                allow_host: Vec::new(),
                emit_diff: None,
                emit_tape: Some(replay_tape_path.to_string_lossy().into_owned()),
                runtime: "paused-tokio".to_string(),
                argv: args.argv.clone(),
            };
            let inner = run_args(derived).await;
            match EventTape::load(&replay_tape_path) {
                Ok(tape) => (tape, inner),
                Err(error) => return append_error(inner, format!("load replay tape: {error}")),
            }
        }
        ReplaySource::Tape(path) => match EventTape::load(Path::new(&path)) {
            Ok(tape) => (tape, RunOutcome::default()),
            Err(error) => return error_outcome(format!("load replay tape: {error}")),
        },
    };

    let report = compare(&recorded, &replay, mode);
    let json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => return append_error(prelude, format!("serialize fidelity report: {error}")),
    };
    if let Some(path) = args.report.as_ref() {
        if let Err(error) = persist_fidelity_report(&json, Path::new(path)) {
            return append_error(prelude, format!("write fidelity report: {error}"));
        }
        prelude.stderr.push_str(&format!(
            "[testbench] fidelity report written to {path} (mode={:?}, score={:.4}, divergences={})\n",
            report.mode,
            report.score,
            report.divergences.len(),
        ));
    } else {
        prelude.stdout.push_str(&json);
        prelude.stdout.push('\n');
    }
    if !report.divergences.is_empty() {
        prelude.exit_code = prelude.exit_code.max(report_exit_code(&report));
    }
    prelude
}

fn report_exit_code(report: &FidelityReport) -> i32 {
    // Exit non-zero on any divergence so CI gates can rely on the
    // status code without parsing JSON.
    if report.divergences.is_empty() {
        0
    } else {
        2
    }
}

fn persist_fidelity_report(json: &str, path: &Path) -> Result<(), String> {
    persist_text(json, path)
}

fn persist_text(body: &str, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, body).map_err(|error| format!("write {}: {error}", path.display()))
}

fn build_testbench(args: &TestBenchRunArgs) -> Result<Testbench, String> {
    let clock = match args.clock.as_str() {
        "paused" => ClockConfig::Paused {
            starting_at_ms: args.start_at_ms.unwrap_or(DEFAULT_TESTBENCH_START_MS),
        },
        "real" => ClockConfig::Real,
        other => return Err(format!("--clock must be `paused` or `real`, got `{other}`")),
    };

    let llm = if let Some(fixture) = &args.llm_fixture {
        LlmConfig::Replay {
            fixture: PathBuf::from(fixture),
        }
    } else if let Some(record) = &args.llm_record {
        LlmConfig::Record {
            fixture: PathBuf::from(record),
        }
    } else {
        LlmConfig::Real
    };

    let filesystem = match &args.fs_overlay {
        None => FilesystemConfig::Real,
        Some(root) => FilesystemConfig::Overlay {
            worktree: PathBuf::from(root),
        },
    };

    let subprocess = if let Some(record) = &args.process_record {
        SubprocessConfig::Record {
            tape: PathBuf::from(record),
        }
    } else if let Some(replay) = &args.process_replay {
        SubprocessConfig::Replay {
            tape: PathBuf::from(replay),
        }
    } else if let Some(toolchain) = &args.process_wasi {
        SubprocessConfig::WasiToolchain {
            dir: PathBuf::from(toolchain),
        }
    } else {
        SubprocessConfig::Real
    };

    let network = match args.network.as_str() {
        "deny" => NetworkConfig::DenyByDefault {
            allow: args.allow_host.clone(),
        },
        "real" => NetworkConfig::Real,
        other => return Err(format!("--network must be `deny` or `real`, got `{other}`")),
    };

    let tape = match &args.emit_tape {
        None => TapeConfig::Off,
        Some(path) => TapeConfig::Emit {
            path: PathBuf::from(path),
            argv: args.argv.clone(),
            script_path: Some(args.file.clone()),
        },
    };

    Ok(Testbench {
        clock,
        llm,
        filesystem,
        subprocess,
        network,
        tape,
    })
}

fn persist_overlay_diff(diff: &[DiffEntry], path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
        }
    }
    let body = render_unified_diff(diff);
    fs::write(path, body).map_err(|err| format!("write {}: {err}", path.display()))
}

fn render_diff_summary(diff: &[DiffEntry]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[testbench] overlay fs diff: {} change(s)\n",
        diff.len()
    ));
    for entry in diff {
        let label = match &entry.kind {
            DiffKind::Added { .. } => "added",
            DiffKind::Modified { .. } => "modified",
            DiffKind::Deleted => "deleted",
        };
        out.push_str(&format!("  {label} {}\n", entry.path.display()));
    }
    out
}

fn error_outcome(message: String) -> RunOutcome {
    RunOutcome {
        stdout: String::new(),
        stderr: format!("error: {message}\n"),
        exit_code: 1,
    }
}

fn append_error(mut outcome: RunOutcome, message: String) -> RunOutcome {
    outcome.stderr.push_str(&format!("error: {message}\n"));
    outcome.exit_code = outcome.exit_code.max(1);
    outcome
}

fn flush_outcome(outcome: RunOutcome) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    if outcome.exit_code != 0 {
        process::exit(outcome.exit_code);
    }
}
