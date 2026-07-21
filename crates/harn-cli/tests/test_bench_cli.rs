#![recursion_limit = "256"]

//! In-process coverage for the testbench composition primitive (#1440).
//!
//! These tests drive [`harn_vm::testbench::Testbench`] directly rather
//! than spawning `harn test-bench run`. The CLI subcommand is a thin
//! wrapper over the same path; thread-local mocks make a sub-process
//! invocation an outer-loop choice, not a correctness one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, env_lock};
use harn_vm::testbench::fidelity::{compare, FidelityMode};
use harn_vm::testbench::overlay_fs::{DiffKind, OverlayFs};
use harn_vm::testbench::process_tape::{
    install_process_tape, ProcessTape, ProcessTapeMode, TapeEntry,
};
use harn_vm::testbench::tape::{EventTape, TapeRecordKind};
use harn_vm::testbench::Testbench;
use tempfile::TempDir;

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-test-bench".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

fn write_file(dir: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    path
}

fn run_under_testbench(
    cwd: PathBuf,
    script: PathBuf,
    configure: impl FnOnce() -> Testbench + Send + 'static,
) -> RunOutcome {
    run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let original_cwd = std::env::current_dir().ok();
        std::env::set_current_dir(&cwd).expect("set cwd to test workspace");

        let bench = configure();
        let session = bench.activate().expect("activate testbench");

        let outcome = execute_run(
            &script.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;

        // Flush any recorded artifacts (process tape, unified tape) the
        // run produced. `finalize` is the single mutator that persists;
        // leaving the session to drop on its own is fine for axes that
        // only need teardown, but tests asserting on emitted files must
        // call this explicitly.
        let _ = session.finalize();

        if let Some(prev) = original_cwd {
            let _ = std::env::set_current_dir(prev);
        }
        outcome
    })
}

#[test]
fn paused_clock_advances_thirty_days_deterministically() {
    // Steel thread #1 from issue #1440: prove paused-clock cron-like
    // workloads advance virtual time deterministically. The marketing
    // claim — "simulates weeks of cron in milliseconds of wall time" —
    // is observable via the test harness's overall runtime; we assert
    // only the deterministic virtual-time property here. If the mock
    // clock ever regresses, the test deadlocks instead of flaking on
    // slow machines, which CI surfaces as a hard timeout.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "cron.harn",
        r#"
pipeline default() {
  const start = now_ms()
  // 30 simulated days, one tick per simulated hour.
  for _ in range(30 * 24) {
    sleep(3600000)
  }
  const advanced = now_ms() - start
  __io_println("advanced_ms=${advanced}")
}
"#,
    );

    let outcome = run_under_testbench(temp.path().to_path_buf(), script, || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .build()
    });

    assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
    assert!(
        outcome.stdout.contains("advanced_ms=2592000000"),
        "expected 30d advance in stdout, got: {}",
        outcome.stdout
    );
}

#[test]
fn fs_overlay_writes_do_not_touch_underlying_tree() {
    // Steel thread #2: a write through the overlay must be invisible
    // to the underlying filesystem.
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().to_path_buf();
    fs::write(workspace.join("seed.txt"), "underlying").unwrap();

    let script = write_file(
        &workspace,
        "writer.harn",
        r#"
pipeline default() {
  __io_println(read_file("seed.txt"))
  write_file("new-file.txt", "hello from overlay")
  __io_println(read_file("new-file.txt"))
}
"#,
    );

    let workspace_for_bench = workspace.clone();
    let outcome = run_under_testbench(workspace.clone(), script, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .fs_overlay(workspace_for_bench)
            .build()
    });

    assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
    assert!(outcome.stdout.contains("underlying"));
    assert!(outcome.stdout.contains("hello from overlay"));
    // Real disk is untouched.
    assert!(
        !workspace.join("new-file.txt").exists(),
        "overlay write must not have hit disk"
    );
}

#[test]
fn process_tape_replay_emits_recorded_output_without_spawning() {
    // Steel thread #3: a recorded subprocess tape replays without
    // spawning a real process. We bypass `harn run` and exercise the
    // sandbox::command_output entry point directly because Harn's
    // user-facing exec/shell builtins go through it.
    use harn_vm::process_sandbox::{command_output, ProcessCommandConfig};

    let tape = ProcessTape::replay_from(vec![TapeEntry {
        program: "/usr/bin/totally-fake-binary".to_string(),
        args: vec!["--flag".to_string()],
        cwd: None,
        env: Default::default(),
        stdout: "hello from tape\n".to_string(),
        stderr: String::new(),
        exit_code: 0,
        duration_ms: 50,
    }]);
    let _guard = install_process_tape(Arc::new(tape));
    let _clock =
        harn_vm::clock_mock::install_override(harn_vm::clock_mock::MockClock::at_wall_ms(0));
    let before = harn_vm::clock_mock::now_ms();

    let output = command_output(
        "/usr/bin/totally-fake-binary",
        &["--flag".to_string()],
        &ProcessCommandConfig::default(),
    )
    .expect("tape replay should succeed");

    let after = harn_vm::clock_mock::now_ms();
    assert_eq!(output.stdout, b"hello from tape\n");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(after - before, 50);
}

#[test]
fn deny_network_default_blocks_egress_until_session_drops() {
    // Steel thread #4: tear-down restores the prior egress policy. Scrub
    // ambient `HARN_EGRESS_*` first so the after-drop probe checks the
    // bench's teardown, not whatever the invoking shell exported.
    for key in [
        "HARN_EGRESS_ALLOW",
        "HARN_EGRESS_DENY",
        "HARN_EGRESS_DEFAULT",
        "HARN_EGRESS_BLOCK_PRIVATE",
        "HARN_EGRESS_ALLOW_LOOPBACK",
    ] {
        std::env::remove_var(key);
    }
    harn_vm::egress::reset_egress_policy_for_host();

    let session = Testbench::builder()
        .deny_network()
        .build()
        .activate()
        .expect("activate");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime
        .block_on(harn_vm::egress::enforce_url_allowed(
            "testbench",
            "https://example.com/x",
        ))
        .expect_err("deny-by-default blocks egress while the session is live");
    drop(session);
    runtime
        .block_on(harn_vm::egress::enforce_url_allowed(
            "testbench",
            "https://example.com/x",
        ))
        .expect("dropping the session removes the deny policy");
}

#[test]
fn overlay_diff_round_trips_through_unified_diff_render() {
    // The unified-diff render is the operator's view of "what would
    // have changed had this been a real run." Verify it covers all
    // three change kinds.
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("kept.txt"), "v1\n").unwrap();
    fs::write(temp.path().join("doomed.txt"), "x\n").unwrap();

    let overlay = OverlayFs::rooted_at(temp.path());
    overlay
        .write(&temp.path().join("kept.txt"), b"v2\n")
        .unwrap();
    overlay
        .write(&temp.path().join("brand-new.txt"), b"hi\n")
        .unwrap();
    overlay
        .remove_file(&temp.path().join("doomed.txt"))
        .unwrap();

    let mut kinds: Vec<&str> = overlay
        .diff()
        .into_iter()
        .map(|entry| match entry.kind {
            DiffKind::Added { .. } => "added",
            DiffKind::Modified { .. } => "modified",
            DiffKind::Deleted => "deleted",
        })
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["added", "deleted", "modified"]);

    let rendered = overlay.render_unified_diff();
    assert!(rendered.contains("/dev/null"));
    assert!(rendered.contains("brand-new.txt"));
    assert!(rendered.contains("kept.txt"));
    assert!(rendered.contains("doomed.txt"));
}

#[test]
fn process_tape_persist_and_load_round_trip() {
    // The persist/load path is the bridge between
    // `harn test-bench run --process-record` and
    // `harn test-bench replay --process-tape` — verify a recording
    // captured via the start_recording/finish span round-trips through disk.
    use harn_vm::process_sandbox::{command_output, ProcessCommandConfig};

    let temp = TempDir::new().unwrap();
    let tape_path = temp.path().join("process.tape");
    let (echo_cmd, echo_args) = echo_command_for_process_tape();

    // Record a real invocation via the sandbox command_output entry.
    {
        let tape = Arc::new(ProcessTape::recording());
        let _guard = install_process_tape(Arc::clone(&tape));
        let _output = command_output(echo_cmd, &echo_args, &ProcessCommandConfig::default())
            .expect("real subprocess");
        tape.persist(&tape_path).expect("persist tape");
        let recorded = tape.recorded();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].stdout.contains("hello-tape"));
    }

    // Load that tape and verify it can drive a replay.
    let loaded = ProcessTape::load(&tape_path).expect("load tape");
    assert_eq!(loaded.mode(), ProcessTapeMode::Replay);

    let replay_tape = Arc::new(loaded);
    let _guard = install_process_tape(Arc::clone(&replay_tape));
    let output = command_output(echo_cmd, &echo_args, &ProcessCommandConfig::default())
        .expect("replay should succeed");
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello-tape"));
    assert!(replay_tape.fully_consumed());
}

fn echo_command_for_process_tape() -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd",
            vec![
                "/C".to_string(),
                "echo".to_string(),
                "hello-tape".to_string(),
            ],
        )
    } else {
        ("/bin/echo", vec!["hello-tape".to_string()])
    }
}

#[test]
fn unified_tape_byte_identical_round_trip() {
    // Acceptance criterion #1 from #1441: emit a tape, replay it, score
    // byte-identical fidelity. Same script under the same paused clock
    // produces a tape that byte-matches itself.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "tape.harn",
        r#"
pipeline default() {
  const start = now_ms()
  sleep(50)
  write_file("snapshot.txt", "checkpoint at ${now_ms() - start}ms")
}
"#,
    );

    let workspace_a = temp.path().to_path_buf();
    let workspace_b = temp.path().to_path_buf();
    let script_clone = script.clone();
    let tape_a = temp.path().join("a.tape");
    let tape_b = temp.path().join("b.tape");
    let tape_a_for_a = tape_a.clone();
    let tape_b_for_b = tape_b.clone();

    let outcome_a = run_under_testbench(workspace_a.clone(), script.clone(), move || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .fs_overlay(workspace_a)
            .emit_tape_for(
                tape_a_for_a,
                Some(script_clone.to_string_lossy().into_owned()),
                Vec::new(),
            )
            .build()
    });
    assert_eq!(outcome_a.exit_code, 0, "stderr: {}", outcome_a.stderr);

    let script_clone_b = script.clone();
    let outcome_b = run_under_testbench(workspace_b.clone(), script, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .fs_overlay(workspace_b)
            .emit_tape_for(
                tape_b_for_b,
                Some(script_clone_b.to_string_lossy().into_owned()),
                Vec::new(),
            )
            .build()
    });
    assert_eq!(outcome_b.exit_code, 0, "stderr: {}", outcome_b.stderr);

    let recorded = EventTape::load(&tape_a).expect("load tape a");
    let replay = EventTape::load(&tape_b).expect("load tape b");
    assert!(
        !recorded.records.is_empty(),
        "tape captured nothing: {:?}",
        recorded.records
    );

    let report = compare(&recorded, &replay, FidelityMode::ByteIdentical);
    assert!(
        report.is_byte_identical(),
        "byte-identical replay diverged: {report:?}"
    );
    assert_eq!(report.score, 1.0);
}

#[test]
fn unified_tape_flags_unpinned_clock_divergence() {
    // Acceptance criterion #5: deliberately introduce a wall-clock read
    // in a script and confirm the tape captures it AND the oracle flags
    // the divergence between two runs that observed different times.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "drifty.harn",
        r#"
pipeline default() {
  __io_println("now=${now_ms()}")
}
"#,
    );

    let workspace_a = temp.path().to_path_buf();
    let workspace_b = temp.path().to_path_buf();
    let script_clone_a = script.clone();
    let script_clone_b = script.clone();
    let tape_a = temp.path().join("drift_a.tape");
    let tape_b = temp.path().join("drift_b.tape");
    let tape_a_for_a = tape_a.clone();
    let tape_b_for_b = tape_b.clone();

    // Run twice with *different* pinned clocks so the recorded
    // ClockRead values diverge across the two tapes.
    let outcome_a = run_under_testbench(workspace_a, script.clone(), move || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .emit_tape_for(
                tape_a_for_a,
                Some(script_clone_a.to_string_lossy().into_owned()),
                Vec::new(),
            )
            .build()
    });
    assert_eq!(outcome_a.exit_code, 0, "stderr: {}", outcome_a.stderr);

    let outcome_b = run_under_testbench(workspace_b, script, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_700_000_999_999)
            .emit_tape_for(
                tape_b_for_b,
                Some(script_clone_b.to_string_lossy().into_owned()),
                Vec::new(),
            )
            .build()
    });
    assert_eq!(outcome_b.exit_code, 0, "stderr: {}", outcome_b.stderr);

    let tape_a_loaded = EventTape::load(&tape_a).expect("load drift_a");
    let tape_b_loaded = EventTape::load(&tape_b).expect("load drift_b");

    // The tapes must contain a ClockRead — proves capture.
    assert!(
        tape_a_loaded
            .records
            .iter()
            .any(|r| matches!(r.kind, TapeRecordKind::ClockRead { .. })),
        "drift_a tape missing ClockRead record"
    );

    // The oracle must flag the divergence under byte-identical mode.
    let report = compare(&tape_a_loaded, &tape_b_loaded, FidelityMode::ByteIdentical);
    assert!(
        !report.is_byte_identical(),
        "oracle failed to flag clock drift: {report:?}"
    );
    assert!(
        report
            .divergences
            .iter()
            .any(|d| d.category == "clock_read_value"),
        "expected a clock_read_value divergence, got: {:?}",
        report.divergences
    );
}

/// A `WasiToolchain` testbench routes `command_output` to a WASI-compiled
/// tool, virtualizes its clocks via the testbench `MockClock`, and surfaces
/// the WASM module's stdout to the parent. Only meaningful when the
/// `testbench-wasi` feature is built in; without it the `WasiToolchain`
/// subprocess mode returns a "requires the feature" error by design.
#[cfg(feature = "testbench-wasi")]
#[test]
fn wasi_toolchain_runs_module_with_virtualized_clock() {
    use harn_vm::process_sandbox::{command_output, ProcessCommandConfig};
    use harn_vm::testbench::SubprocessConfig;

    // Minimal WASI module: read realtime clock, write 8 bytes (LE
    // nanoseconds) to stdout, exit 0.
    const CLOCK_READ_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "clock_time_get"
            (func $clock_time_get (param i32 i64 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "proc_exit"
            (func $proc_exit (param i32)))
          (memory (export "memory") 1)
          (data (i32.const 16) "\40\00\00\00\08\00\00\00")
          (func (export "_start")
            i32.const 0
            i64.const 1
            i32.const 64
            call $clock_time_get
            drop
            i32.const 1
            i32.const 16
            i32.const 1
            i32.const 80
            call $fd_write
            drop
            i32.const 0
            call $proc_exit
          )
        )
    "#;

    let toolchain = TempDir::new().unwrap();
    let wasm_bytes = wat::parse_str(CLOCK_READ_WAT).expect("parse WAT");
    fs::write(toolchain.path().join("readclock.wasm"), &wasm_bytes).unwrap();

    let start_ms: i64 = 1_767_225_600_000;
    let bench = Testbench {
        clock: harn_vm::testbench::ClockConfig::Paused {
            starting_at_ms: start_ms,
        },
        subprocess: SubprocessConfig::WasiToolchain {
            dir: toolchain.path().to_path_buf(),
        },
        ..Default::default()
    };
    let _session = bench.activate().expect("activate testbench");

    let output =
        command_output("readclock", &[], &ProcessCommandConfig::default()).expect("run wasi tool");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout.len(),
        8,
        "module wrote 8 LE-encoded nanoseconds"
    );
    let nanos = u64::from_le_bytes(output.stdout[..8].try_into().unwrap());
    assert_eq!(
        nanos,
        start_ms as u64 * 1_000_000,
        "clock_time_get returned the testbench wall-clock time"
    );

    // Programs without a matching `.wasm` should fall through. Since this
    // testbench has only `readclock.wasm`, an unknown program would attempt
    // a real spawn — verify by asking for a non-existent tool name; this
    // returns a spawn error from the host OS, not a tape-replay error.
    let fallback = command_output("nonexistent-cmd-xyz", &[], &ProcessCommandConfig::default());
    assert!(
        fallback.is_err(),
        "non-WASI fallback should attempt the host spawn and fail there"
    );
}

// ── DES runtime mode (#1444) ────────────────────────────────────────────────

/// Run a script inside a single-threaded `current_thread` Tokio runtime with
/// the testbench mocks active. Mirrors `run_under_testbench` but substitutes
/// `new_current_thread` for `new_multi_thread` to validate the DES mode's
/// bit-exact determinism property.
fn run_under_des_runtime<F>(cwd: PathBuf, script: PathBuf, configure: F) -> RunOutcome
where
    F: FnOnce() -> Testbench + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("harn-des-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build DES runtime");
            let outcome = rt.block_on(async move {
                let _env_guard = env_lock::lock_env().lock().await;
                let _cwd_guard = cwd_lock::lock_cwd_async().await;
                harn_vm::reset_thread_local_state();
                let original_cwd = std::env::current_dir().ok();
                std::env::set_current_dir(&cwd).expect("set cwd");

                let bench = configure();
                let session = bench.activate().expect("activate testbench");
                let outcome = execute_run(
                    &script.to_string_lossy(),
                    false,
                    HashSet::new(),
                    Vec::new(),
                    Vec::new(),
                    CliLlmMockMode::Off,
                    None,
                    RunProfileOptions::default(),
                )
                .await;
                let _ = session.finalize();

                if let Some(prev) = original_cwd {
                    let _ = std::env::set_current_dir(prev);
                }
                outcome
            });
            let _ = tx.send(outcome);
        })
        .expect("spawn DES test thread");
    rx.recv().expect("DES runtime completed")
}

#[test]
fn des_runtime_paused_sleep_returns_immediately() {
    // Acceptance criterion from #1444: the DES runtime drives a script
    // containing sleep() under a paused mock clock. The sleep must complete
    // immediately (no wall-clock wait) and virtual time must advance correctly.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "des_sleep.harn",
        r#"
pipeline default() {
  mock_time(1000000)
  const start = now_ms()
  sleep(86400000)
  const delta = now_ms() - start
  __io_println("delta=${delta}")
}
"#,
    );
    let outcome = run_under_des_runtime(temp.path().to_path_buf(), script, move || {
        Testbench::builder().paused_clock_at_ms(1_000_000).build()
    });
    assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
    assert!(
        outcome.stdout.contains("delta=86400000"),
        "expected 24h virtual advance, got: {}",
        outcome.stdout
    );
}

#[test]
fn des_runtime_concurrent_agents_settle_byte_identical() {
    // Acceptance criterion from #1444: running the same `parallel settle` script
    // twice under the DES runtime produces bit-exact tapes across reruns.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "des_settle.harn",
        r#"
pipeline default() {
  mock_time(1000000)
  const outcome = parallel settle [1, 2, 3, 4, 5] { item ->
    sleep(item * 100)
    item * item
  }
  __io_println("succeeded=${outcome.succeeded}")
  __io_println("failed=${outcome.failed}")
}
"#,
    );
    let tape_a = temp.path().join("a.tape");
    let tape_b = temp.path().join("b.tape");
    let script_b = script.clone();
    let tape_a_clone = tape_a.clone();
    let tape_b_clone = tape_b.clone();
    let ws_a = temp.path().to_path_buf();
    let ws_b = temp.path().to_path_buf();

    let outcome_a = run_under_des_runtime(ws_a, script, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_000_000)
            .emit_tape(tape_a_clone)
            .build()
    });
    assert_eq!(outcome_a.exit_code, 0, "run A stderr: {}", outcome_a.stderr);

    let outcome_b = run_under_des_runtime(ws_b, script_b, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_000_000)
            .emit_tape(tape_b_clone)
            .build()
    });
    assert_eq!(outcome_b.exit_code, 0, "run B stderr: {}", outcome_b.stderr);

    assert!(outcome_a.stdout.contains("succeeded=5"));
    assert!(outcome_b.stdout.contains("succeeded=5"));

    let tape_a_loaded = EventTape::load(&tape_a).expect("load tape A");
    let tape_b_loaded = EventTape::load(&tape_b).expect("load tape B");

    let report = compare(&tape_a_loaded, &tape_b_loaded, FidelityMode::ByteIdentical);
    assert!(
        report.is_byte_identical(),
        "DES mode must produce bit-exact tapes; divergences: {:#?}",
        report.divergences
    );
}

#[test]
fn des_runtime_output_matches_paused_tokio() {
    // Smoke: the DES runtime produces the same user-visible output as the
    // default paused-tokio runtime for a deterministic script.
    let temp = TempDir::new().unwrap();
    let script = write_file(
        temp.path(),
        "fidelity_smoke.harn",
        r#"
pipeline default() {
  mock_time(1767225600000)
  const t0 = now_ms()
  advance_time(5000)
  const t1 = now_ms()
  __io_println("delta=${t1 - t0}")
}
"#,
    );
    let script_b = script.clone();
    let ws_a = temp.path().to_path_buf();
    let ws_b = temp.path().to_path_buf();

    // Run under paused-tokio (default).
    let paused_outcome = run_under_testbench(ws_a, script, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_767_225_600_000)
            .build()
    });
    assert_eq!(paused_outcome.exit_code, 0, "{}", paused_outcome.stderr);

    // Run under DES (current_thread).
    let des_outcome = run_under_des_runtime(ws_b, script_b, move || {
        Testbench::builder()
            .paused_clock_at_ms(1_767_225_600_000)
            .build()
    });
    assert_eq!(des_outcome.exit_code, 0, "{}", des_outcome.stderr);

    assert_eq!(
        paused_outcome.stdout.trim(),
        des_outcome.stdout.trim(),
        "DES and paused-tokio must produce identical stdout"
    );
}

mod annotations {
    //! Issue #1474: annotation tape format coverage.
    //!
    //! Records a tape, writes a sidecar annotations file, and exercises
    //! the three CLI surfaces (replay, validate-annotations,
    //! export-annotations) end-to-end against the recorded artifact.

    use super::*;
    use harn_vm::testbench::annotations::{
        compute_tape_content_hash, validate_against_tape, Annotation, AnnotationAuthor,
        AnnotationHeader, AnnotationKind, AnnotationSpan, AnnotationTape, AuthorKind,
        HypothesisStatus,
    };

    /// Helper: record a deterministic tape using a tiny script. Returns
    /// the tape path so callers can write annotations against it.
    fn record_seed_tape(workspace: &Path) -> PathBuf {
        let script = write_file(
            workspace,
            "seed.harn",
            r#"
pipeline default() {
  const start = now_ms()
  sleep(500)
  const after = now_ms()
  __io_println("delta=${after - start}")
}
"#,
        );
        let tape_path = workspace.join("seed.tape");
        let bench_tape_path = tape_path.clone();
        let outcome = run_under_testbench(workspace.to_path_buf(), script, move || {
            Testbench::builder()
                .paused_clock_at_ms(1_767_225_600_000)
                .emit_tape(bench_tape_path)
                .build()
        });
        assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
        tape_path
    }

    fn note_annotation(id: &str, event_id: u64, evidence: &str) -> Annotation {
        Annotation {
            id: id.into(),
            event_id,
            kind: AnnotationKind::Note,
            evidence: Some(evidence.into()),
            suggested_fix: None,
            author: Some(AnnotationAuthor {
                id: Some("alice".into()),
                kind: AuthorKind::Human,
                surface: Some("burin-code".into()),
            }),
            timestamp: Some("2026-05-10T17:00:00Z".into()),
            span: None,
            hypothesis_status: None,
            friction_kind: None,
            links: Vec::new(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn round_trip_persist_load_validate_against_recorded_tape() {
        let temp = TempDir::new().unwrap();
        let tape_path = record_seed_tape(temp.path());
        let tape = EventTape::load(&tape_path).expect("load tape");
        assert!(
            tape.records.iter().any(|r| matches!(
                r.kind,
                harn_vm::testbench::tape::TapeRecordKind::ClockSleep { .. }
            )),
            "seed tape must contain at least one clock_sleep"
        );

        let mut ann_tape = AnnotationTape::new(AnnotationHeader::current(
            Some(tape_path.to_string_lossy().into_owned()),
            compute_tape_content_hash(&tape),
        ));
        ann_tape
            .annotations
            .push(note_annotation("ann-1", 0, "first event in seed tape"));
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::Hypothesis,
            hypothesis_status: Some(HypothesisStatus::Active),
            ..note_annotation("ann-2", 1, "is the sleep load-bearing here?")
        });
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("missing_context".into()),
            ..note_annotation("ann-3", 2, "we should pre-fetch this clock value")
        });
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::CrystallizeHere,
            span: Some(AnnotationSpan {
                start_event_id: 0,
                end_event_id: 2,
            }),
            ..note_annotation("ann-4", 0, "this 3-event sequence repeats across runs")
        });
        let ann_path = tape_path.with_extension("tape.annotations.jsonl");
        ann_tape.persist(&ann_path).unwrap();

        let reloaded = AnnotationTape::load(&ann_path).unwrap();
        assert_eq!(reloaded.annotations, ann_tape.annotations);

        let report = validate_against_tape(&reloaded, &tape);
        assert!(
            report.is_ok(),
            "validation should pass for well-formed annotations: {:?}",
            report.problems
        );
        assert_eq!(report.annotations_checked, 4);
    }

    #[test]
    fn export_filter_by_kind_round_trips_through_friction_event_format() {
        let temp = TempDir::new().unwrap();
        let tape_path = record_seed_tape(temp.path());
        let mut ann_tape = AnnotationTape::new(AnnotationHeader::current(
            Some(tape_path.to_string_lossy().into_owned()),
            None,
        ));
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("repeated_query".into()),
            ..note_annotation("f-1", 0, "Splunk lookup repeats")
        });
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("manual_handoff".into()),
            ..note_annotation("f-2", 1, "every incident pages someone")
        });
        ann_tape
            .annotations
            .push(note_annotation("note", 1, "not a friction event"));
        let ann_path = tape_path.with_extension("tape.annotations.jsonl");
        ann_tape.persist(&ann_path).unwrap();

        // Convert directly via the model (mirror what `export-annotations
        // --format friction` does inside the CLI).
        let friction_events = ann_tape.to_friction_events();
        assert_eq!(friction_events.len(), 2);
        assert_eq!(friction_events[0].kind, "repeated_query");
        assert_eq!(friction_events[1].kind, "manual_handoff");
        for event in &friction_events {
            assert!(!event.id.is_empty());
            assert!(!event.redacted_summary.is_empty());
        }
    }

    #[test]
    fn validation_flags_event_id_drift_when_tape_is_mutated() {
        let temp = TempDir::new().unwrap();
        let tape_path = record_seed_tape(temp.path());
        let tape = EventTape::load(&tape_path).expect("load tape");
        let max_seq = tape.records.iter().map(|r| r.seq).max().unwrap_or(0);

        let mut ann_tape = AnnotationTape::new(AnnotationHeader::current(
            Some(tape_path.to_string_lossy().into_owned()),
            compute_tape_content_hash(&tape),
        ));
        // Reference an event_id past the end so the validator catches it.
        ann_tape
            .annotations
            .push(note_annotation("dangling", max_seq + 99, "unreachable"));
        let report = validate_against_tape(&ann_tape, &tape);
        assert!(!report.is_ok());
        assert!(report.problems.iter().any(|p| matches!(
            p,
            harn_vm::testbench::annotations::AnnotationProblem::UnknownEventId { .. }
        )));
    }

    #[test]
    fn crystallize_anchors_link_human_judgement_to_candidate_detection() {
        // Demonstrates the integration documented in issue #1474 acceptance
        // criterion ("annotations feeding crystallization candidate detection"):
        // a `crystallize_here` annotation surfaces as a `CrystallizeAnchor`
        // ready for the candidate detector to weigh against inferred
        // candidates.
        let temp = TempDir::new().unwrap();
        let tape_path = record_seed_tape(temp.path());
        let tape = EventTape::load(&tape_path).expect("load tape");
        let max_seq = tape.records.iter().map(|r| r.seq).max().unwrap_or(0);

        let mut ann_tape = AnnotationTape::new(AnnotationHeader::current(
            Some(tape_path.to_string_lossy().into_owned()),
            compute_tape_content_hash(&tape),
        ));
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::CrystallizeHere,
            span: Some(AnnotationSpan {
                start_event_id: 0,
                end_event_id: max_seq,
            }),
            ..note_annotation("crys-1", 0, "this run is a workflow candidate")
        });
        ann_tape.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("missing_context".into()),
            ..note_annotation("f-1", 1, "we keep needing this context")
        });

        let anchors = ann_tape.crystallize_anchors();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].event_id, 0);
        assert_eq!(anchors[0].end_event_id, max_seq);
        assert_eq!(
            anchors[0].evidence.as_deref(),
            Some("this run is a workflow candidate")
        );

        // Friction annotations meanwhile are interchangeable with
        // FrictionEvent records so context-pack candidate detection
        // (orchestration::generate_context_pack_suggestions) can consume
        // both pipelines without a fork.
        let friction_events = ann_tape.to_friction_events();
        assert_eq!(friction_events.len(), 1);
        assert_eq!(friction_events[0].kind, "missing_context");
    }
}
