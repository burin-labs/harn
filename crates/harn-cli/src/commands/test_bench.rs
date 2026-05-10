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

use harn_vm::testbench::fidelity::{compare, FidelityMode, FidelityReport};
use harn_vm::testbench::overlay_fs::{render_unified_diff, DiffEntry, DiffKind};
use harn_vm::testbench::tape::EventTape;
use harn_vm::testbench::{
    ClockConfig, FilesystemConfig, LlmConfig, NetworkConfig, SubprocessConfig, TapeConfig,
    Testbench,
};

use crate::cli::{TestBenchCommand, TestBenchFidelityArgs, TestBenchReplayArgs, TestBenchRunArgs};
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
    outcome
}

async fn replay_args(args: TestBenchReplayArgs) -> RunOutcome {
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
        emit_tape: args.emit_tape.clone(),
        runtime: "paused-tokio".to_string(),
        argv: args.argv.clone(),
    };
    run_args(derived).await
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
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))
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
