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
use harn_vm::testbench::overlay_fs::{DiffKind, OverlayFs};
use harn_vm::testbench::process_tape::{
    install_process_tape, ProcessTape, ProcessTapeMode, TapeEntry,
};
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
        let _session = bench.activate().expect("activate testbench");

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
  let start = now_ms()
  // 30 simulated days, one tick per simulated hour.
  for _ in range(30 * 24) {
    sleep(3600000)
  }
  let advanced = now_ms() - start
  println("advanced_ms=${advanced}")
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
  println(read_file("seed.txt"))
  write_file("new-file.txt", "hello from overlay")
  println(read_file("new-file.txt"))
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
    // Steel thread #4: tear-down restores environment.
    let session = Testbench::builder()
        .deny_network()
        .build()
        .activate()
        .expect("activate");
    assert_eq!(std::env::var("HARN_EGRESS_DEFAULT").as_deref(), Ok("deny"));
    drop(session);
    assert!(std::env::var("HARN_EGRESS_DEFAULT").is_err());
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

    // Record a real invocation via the sandbox command_output entry.
    {
        let tape = Arc::new(ProcessTape::recording());
        let _guard = install_process_tape(Arc::clone(&tape));
        let _output = command_output(
            "/bin/echo",
            &["hello-tape".to_string()],
            &ProcessCommandConfig::default(),
        )
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
    let output = command_output(
        "/bin/echo",
        &["hello-tape".to_string()],
        &ProcessCommandConfig::default(),
    )
    .expect("replay should succeed");
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello-tape"));
    assert!(replay_tape.fully_consumed());
}
