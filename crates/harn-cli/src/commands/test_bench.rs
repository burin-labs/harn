//! `harn test-bench` runner.
//!
//! Wraps [`crate::commands::run::execute_run`] in a
//! [`harn_vm::testbench::TestbenchSession`] so a script runs against a
//! pinned clock, an optional LLM/process tape, and an optional
//! filesystem overlay — all with deny-by-default network egress.
//!
//! The CLI flag names map onto [`harn_vm::testbench::Testbench`] one-for-one.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process;

use harn_vm::testbench::overlay_fs::{render_unified_diff, DiffEntry, DiffKind};
use harn_vm::testbench::{
    ClockConfig, FilesystemConfig, LlmConfig, NetworkConfig, SubprocessConfig, Testbench,
};

use crate::cli::{TestBenchCommand, TestBenchReplayArgs, TestBenchRunArgs};
use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};

/// Default starting point for `--clock paused` runs. Picked to be
/// stable, RFC-3339-friendly, and after every prerequisite Y2K38
/// boundary so date-of-birth math doesn't underflow:
/// 2026-01-01T00:00:00Z.
const DEFAULT_TESTBENCH_START_MS: i64 = 1_767_225_600_000;

pub(crate) async fn run(command: TestBenchCommand) {
    let outcome = match command {
        TestBenchCommand::Run(args) => run_args(args).await,
        TestBenchCommand::Replay(args) => replay_args(args).await,
    };
    flush_outcome(outcome);
}

async fn run_args(args: TestBenchRunArgs) -> RunOutcome {
    let bench = match build_testbench(&args) {
        Ok(bench) => bench,
        Err(message) => return error_outcome(message),
    };
    let llm_mode = match (&args.llm_fixture, &args.llm_record) {
        (Some(_), Some(_)) => {
            return error_outcome(
                "--llm-fixture and --llm-record are mutually exclusive".to_string(),
            )
        }
        (Some(path), None) => CliLlmMockMode::Replay {
            fixture_path: PathBuf::from(path),
        },
        (None, Some(path)) => CliLlmMockMode::Record {
            fixture_path: PathBuf::from(path),
        },
        (None, None) => CliLlmMockMode::Off,
    };
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
        network: "deny".to_string(),
        allow_host: Vec::new(),
        emit_diff: None,
        argv: args.argv.clone(),
    };
    run_args(derived).await
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

    Ok(Testbench {
        clock,
        llm,
        filesystem,
        subprocess,
        network,
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
