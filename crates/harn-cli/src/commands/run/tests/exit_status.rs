//! Which exit status `harn run` leaves with when a run fails.
//!
//! Split from the main run tests because the claim under test is a contract a
//! caller branches on, not a behavior of one code path: it needs the
//! preparation failure and the program failures side by side, or a status that
//! moved for every failure would satisfy it.

use super::{
    execute_run, execute_run_with_eager_project_handlers, execute_run_with_project_triggers,
    execute_standalone_run, execute_standalone_run_with_denied, write_manifest_trigger_project,
    CliLlmMockMode, RunProfileOptions,
};
use std::collections::HashSet;

/// The reported defect: a run whose dependencies could not be materialized
/// exited `1`, the same status the program exits with when it fails, so a
/// caller shelling out to Harn could not branch on the difference.
///
/// The negative control is the point of the test. A status that changed for
/// every failure would prove nothing, so the same assertion runs against a
/// program that fails on its own terms and against one that does not compile,
/// and both must still be `1`.
#[tokio::test]
async fn unpreparable_dependencies_and_program_failures_get_different_statuses() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    // A declared dependency with no lock file: the entry cannot be prepared,
    // and nothing of the program runs.
    std::fs::write(
        project.path().join("harn.toml"),
        r#"
[package]
name = "unpreparable-dependency-fixture"

[dependencies]
missing = { git = "https://example.invalid/missing.git" }
"#,
    )
    .expect("write manifest");
    let script = project.path().join("main.harn");
    let working = r#"
pipeline main(harness: Harness) {
  harness.stdio.println("target-ran")
}
"#;
    std::fs::write(&script, working).expect("write script");

    // Twice, because two owners can report this failure and they must agree.
    // A cold run materializes packages inside the type check; a warm run hits
    // the bytecode cache, skips the type check entirely, and reaches the same
    // failure while installing the manifest runtime instead.
    for attempt in ["cold", "warm"] {
        let outcome = execute_run_default(&script.to_string_lossy()).await;
        assert_eq!(
            outcome.exit_code,
            crate::exit::RUN_SETUP_FAILURE,
            "{attempt} run, stderr:\n{}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains("harn.lock"),
            "{attempt} run must still name what could not be prepared, got:\n{}",
            outcome.stderr
        );
    }

    let standalone = execute_standalone_run(&script.to_string_lossy()).await;
    assert_eq!(
        standalone.exit_code, 0,
        "standalone must not materialize ambient dependencies: {}",
        standalone.stderr
    );
    assert_eq!(standalone.stdout.trim(), "target-ran");

    // Negative control 1: same project shape, preparable, program returns a
    // failure. The program is what failed.
    let ok_project = tempfile::tempdir().expect("temp project");
    let failing = ok_project.path().join("main.harn");
    std::fs::write(&failing, "pipeline main() {\n  return Err(\"boom\")\n}\n")
        .expect("write script");
    let outcome = execute_run_default(&failing.to_string_lossy()).await;
    assert_eq!(
        outcome.exit_code,
        crate::exit::PROGRAM_FAILURE,
        "stderr:\n{}",
        outcome.stderr
    );

    // Negative control 2: a program that does not compile is still the program
    // failing, not a setup failure.
    let broken = ok_project.path().join("broken.harn");
    std::fs::write(&broken, "pipeline main( {").expect("write script");
    let outcome = execute_run_default(&broken.to_string_lossy()).await;
    assert_eq!(
        outcome.exit_code,
        crate::exit::PROGRAM_FAILURE,
        "stderr:\n{}",
        outcome.stderr
    );
    harn_vm::reset_thread_local_state();
}

async fn execute_run_default(path: &str) -> crate::commands::run::RunOutcome {
    execute_run(
        path,
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await
}

#[tokio::test]
async fn execute_run_defaults_to_lazy_handlers_and_supports_eager_validation() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    let script = write_manifest_trigger_project(
        project.path(),
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println("target-ran")
}
"#,
    );

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

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "target-ran");
    assert!(
        harn_vm::snapshot_trigger_bindings().is_empty(),
        "ordinary runs must not register manifest triggers"
    );

    let outcome = execute_run_with_project_triggers(&script.to_string_lossy()).await;
    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert!(
        harn_vm::snapshot_trigger_bindings().is_empty(),
        "an explicit run must restore its caller's trigger registry"
    );
    harn_vm::reset_thread_local_state();

    let outcome = execute_run_with_eager_project_handlers(&script.to_string_lossy()).await;

    // Installing manifest triggers happens on the program's behalf, before it
    // runs, so it reports as a setup failure rather than as the program failing.
    assert_eq!(
        outcome.exit_code,
        crate::exit::RUN_SETUP_FAILURE,
        "stdout:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stderr
            .contains("failed to install manifest triggers"),
        "stderr:\n{}",
        outcome.stderr
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_run_cannot_observe_or_fire_a_prior_runs_manifest_trigger() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    let trigger_run = write_manifest_trigger_project(
        project.path(),
        r"
pipeline main(harness: Harness) {
  harness.stdio.println(len(harness.runtime.trigger_list()))
}
",
    );
    std::fs::write(
        project.path().join("trigger_handlers.harn"),
        r"
pub fn on_tick(_event) -> dict {
  return {handled: true}
}
",
    )
    .expect("write working trigger handler");

    let first = execute_run_with_project_triggers(&trigger_run.to_string_lossy()).await;
    assert_eq!(first.exit_code, 0, "stderr:\n{}", first.stderr);
    assert_eq!(first.stdout.trim(), "1");

    let default_run = project.path().join("default.harn");
    std::fs::write(
        &default_run,
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println(len(harness.runtime.trigger_list()))
  const fired = try {
    harness.runtime.trigger_fire(
      "cron-handler",
      {id: "evt-must-not-fire", provider: "cron", kind: "cron.tick"},
    )
  }
  harness.stdio.println(is_err(fired))
}
"#,
    )
    .expect("write default run");

    let second = execute_run(
        &default_run.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;
    assert_eq!(second.exit_code, 0, "stderr:\n{}", second.stderr);
    assert_eq!(second.stdout.trim(), "0\ntrue");
    assert!(harn_vm::snapshot_trigger_bindings().is_empty());
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn in_process_runs_cannot_complete_a_prior_runs_partial_trigger_batch() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    std::fs::write(
        project.path().join("harn.toml"),
        "[package]\nname = \"batch-scope-fixture\"\n",
    )
    .expect("write manifest");

    let source = r#"
import "std/triggers"

fn handle_batch(harness: Harness, event: dict) {
  const _ = harness.channels.append(
    "batch.scope.fired",
    {batch_size: len(event.batch)},
  )
}

pipeline main(harness: Harness) {
  const _ = harness.runtime.trigger_register(
    {
      id: "same-binding",
      kind: "channel.emit",
      provider: "channel",
      autonomy_tier: "act_auto",
      handler: handle_batch,
      when: nil,
      retry: nil,
      match: {events: ["channel:batch.scope.input"]},
      events: nil,
      dedupe_key: nil,
      filter: nil,
      batch: {count: 2, window: "1h"},
      budget: nil,
      manifest_path: nil,
      package_name: "batch-scope-fixture",
    },
  )
  const _ = harness.channels.append("batch.scope.input", {source: "one-run"})
  const inputs = harness.channels.events("batch.scope.input")
  const firings = harness.channels.events("batch.scope.fired")
  harness.stdio.println(
    "inputs:" + to_string(len(inputs))
      + ",firings:" + to_string(len(firings)),
  )
  if len(firings) > 0 {
    harness.stdio.println("batch-size:" + to_string(firings[0].payload.batch_size))
  }
}
"#;
    let first_path = project.path().join("first.harn");
    let second_path = project.path().join("second.harn");
    std::fs::write(&first_path, source).expect("write first run");
    std::fs::write(&second_path, source).expect("write second run");

    let first = execute_run_default(&first_path.to_string_lossy()).await;
    assert_eq!(first.exit_code, 0, "stderr:\n{}", first.stderr);
    assert_eq!(
        first.stdout.trim(),
        "inputs:1,firings:0",
        "run A must emit one matching event and leave the batch below threshold"
    );

    let second = execute_run_default(&second_path.to_string_lossy()).await;
    assert_eq!(second.exit_code, 0, "stderr:\n{}", second.stderr);
    assert_eq!(
        second.stdout.trim(),
        "inputs:2,firings:0",
        "the later run must not dispatch a batch containing the first run's event"
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn project_trigger_opt_in_initializes_and_fires_used_handler() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    let script = write_manifest_trigger_project(
        project.path(),
        r#"
import "std/triggers"

pipeline main(harness: Harness) {
  const binding = harness.runtime.trigger_list().filter({ item -> item.id == "cron-handler" })[0]
  const fired = harness.runtime.trigger_fire(
    binding,
    {id: "evt-opt-in", provider: "cron", kind: "cron.tick"},
  )
  harness.stdio.println(fired.status)
  harness.stdio.println(fired.result.handled)
}
"#,
    );
    std::fs::write(
        project.path().join("trigger_handlers.harn"),
        r"
pub fn on_tick(_event) -> dict {
  return {handled: true}
}
",
    )
    .expect("write working trigger handler");

    let outcome = execute_run_with_project_triggers(&script.to_string_lossy()).await;
    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "dispatched\ntrue");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn standalone_run_ignores_broken_project_handlers() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    std::fs::write(
        project.path().join("harn.toml"),
        r#"
[package]
name = "broken-project-handler-fixture"

[exports]
hook_handlers = "hook_handlers.harn"

[[hooks]]
event = "PreToolUse"
handler = "hook_handlers::before_tool"
"#,
    )
    .expect("write manifest");
    std::fs::write(project.path().join("hook_handlers.harn"), "fn broken(")
        .expect("write broken handler");
    let script = project.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println("standalone-ran")
}
"#,
    )
    .expect("write main script");

    let project_outcome = execute_run_default(&script.to_string_lossy()).await;
    assert_ne!(
        project_outcome.exit_code, 0,
        "project handler must be reached"
    );

    let standalone_outcome = execute_standalone_run(&script.to_string_lossy()).await;
    assert_eq!(
        standalone_outcome.exit_code, 0,
        "stderr:\n{}",
        standalone_outcome.stderr
    );
    assert_eq!(standalone_outcome.stdout.trim(), "standalone-ran");
    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn standalone_run_keeps_relative_imports_and_explicit_denials() {
    harn_vm::reset_thread_local_state();
    let project = tempfile::tempdir().expect("temp project");
    std::fs::write(
        project.path().join("value.harn"),
        "pub fn imported_value() -> int { return 42 }\n",
    )
    .expect("write relative module");
    let script = project.path().join("main.harn");
    std::fs::write(
        &script,
        r#"
import { imported_value } from "./value"

pipeline main(harness: Harness) {
  harness.stdio.println(imported_value())
}
"#,
    )
    .expect("write main script");

    let outcome = execute_standalone_run(&script.to_string_lossy()).await;
    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert_eq!(outcome.stdout.trim(), "42");

    std::fs::write(
        &script,
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println(command_risk_scan({request: {mode: "shell", command: "git status"}}))
}
"#,
    )
    .expect("write denied script");
    let outcome = execute_standalone_run_with_denied(
        &script.to_string_lossy(),
        HashSet::from(["command_risk_scan".to_string()]),
    )
    .await;
    assert_ne!(outcome.exit_code, 0, "explicit denial must remain active");
    assert!(
        outcome.stderr.contains("command_risk_scan") && outcome.stderr.contains("tool_rejected"),
        "stderr:\n{}",
        outcome.stderr
    );
    harn_vm::reset_thread_local_state();
}
