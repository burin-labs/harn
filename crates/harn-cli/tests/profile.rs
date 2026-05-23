#![recursion_limit = "256"]

//! Integration coverage for `harn run --profile` / `harn run --profile-json`.
//!
//! Runs a tiny self-contained script (no real LLM calls) through
//! `execute_run` with profiling enabled and asserts that:
//!   1. the text profile renders to stderr
//!   2. the JSON profile is written to disk and round-trips into `RunProfile`
//!   3. the `pipeline` span at the root has a wall time
//!
//! Stays away from `llm_call` so this test doesn't need network or mock
//! fixtures — the categorical buckets just won't include `llm_call` here.

use std::collections::HashSet;
use std::fs;
use std::thread;

use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, env_lock};
use harn_vm::profile::RunProfile;
use tempfile::TempDir;

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-profile-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, future_factory())
        })
        .expect("spawn test thread");
    handle.join().expect("join test thread")
}

#[test]
fn profile_text_and_json_roundtrip() {
    let tempdir = TempDir::new().expect("temp dir");
    let script = tempdir.path().join("script.harn");
    fs::write(
        &script,
        r#"
pipeline main() {
  __io_println("hello")
}
"#,
    )
    .expect("write script");
    let json_path = tempdir.path().join("profile.json");

    let outcome: RunOutcome = run_in_harn_runtime({
        let script = script.clone();
        let json_path = json_path.clone();
        move || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            execute_run(
                &script.to_string_lossy(),
                false,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions {
                    text: true,
                    json_path: Some(json_path.clone()),
                },
            )
            .await
        }
    });

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert!(outcome.stdout.contains("hello"));
    assert!(
        outcome.stderr.contains("Run profile"),
        "stderr should include text profile, got:\n{}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("vm/residual"),
        "stderr should include residual bucket, got:\n{}",
        outcome.stderr
    );

    let json = fs::read_to_string(&json_path).expect("read profile.json");
    let profile: RunProfile = serde_json::from_str(&json).expect("parse profile.json");
    // Wall time can be 0 for trivial scripts (sub-ms). What we care
    // about is that the JSON round-tripped into the canonical struct.
    // No @step blocks in the script, so steps must be empty.
    assert!(
        profile.steps.is_empty(),
        "unexpected steps: {:?}",
        profile.steps
    );
    assert!(
        profile.top_llm_calls.is_empty(),
        "no llm_call expected: {:?}",
        profile.top_llm_calls
    );
}

#[test]
fn profile_records_step_spans_and_attributes_llm_to_step() {
    let tempdir = TempDir::new().expect("temp dir");
    let script = tempdir.path().join("step_script.harn");
    fs::write(
        &script,
        r#"
fn classify(ctx) -> string {
  let r = llm_call("classify ${ctx}", nil, {provider: "mock"})
  return r.text
}

@step(name: "classify_step", model: "claude-haiku-4-5", error_boundary: fail)
fn classify_step(ctx) -> string {
  return classify(ctx)
}

pipeline main() {
  llm_mock_clear()
  llm_mock({
    match: "classify*",
    consume_match: false,
    text: "ok",
    input_tokens: 5,
    output_tokens: 3,
    model: "claude-haiku-4-5",
  })
  let out = classify_step("input")
  __io_println(out)
}
"#,
    )
    .expect("write script");
    let json_path = tempdir.path().join("step_profile.json");

    let outcome: RunOutcome = run_in_harn_runtime({
        let script = script.clone();
        let json_path = json_path.clone();
        move || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            execute_run(
                &script.to_string_lossy(),
                false,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions {
                    text: true,
                    json_path: Some(json_path.clone()),
                },
            )
            .await
        }
    });

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    let profile: RunProfile =
        serde_json::from_str(&fs::read_to_string(&json_path).expect("read profile.json"))
            .expect("parse profile.json");

    // The @step block must produce a step summary.
    assert_eq!(
        profile.steps.len(),
        1,
        "expected exactly one step span, got: {:?}",
        profile.steps
    );
    assert_eq!(profile.steps[0].name, "classify_step");
    assert_eq!(profile.steps[0].llm_calls, 1);

    // The llm_call must be attributed to the enclosing step.
    assert_eq!(profile.top_llm_calls.len(), 1);
    assert_eq!(
        profile.top_llm_calls[0].step.as_deref(),
        Some("classify_step"),
        "llm_call should attribute to enclosing step: {:?}",
        profile.top_llm_calls[0]
    );

    // The text output must mention the per-step section.
    assert!(
        outcome.stderr.contains("Per-@step:"),
        "stderr should include Per-@step section, got:\n{}",
        outcome.stderr
    );
}

#[test]
fn profile_json_includes_user_timing_bucket() {
    let tempdir = TempDir::new().expect("temp dir");
    let script = tempdir.path().join("timing_script.harn");
    fs::write(
        &script,
        r#"
import { timed } from "std/timing"

pipeline main() {
  timed("benchmark.work", {case_id: "fixture"}, { ->
    return 7
  })
  __io_println("done")
}
"#,
    )
    .expect("write script");
    let json_path = tempdir.path().join("timing_profile.json");

    let outcome: RunOutcome = run_in_harn_runtime({
        let script = script.clone();
        let json_path = json_path.clone();
        move || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            execute_run(
                &script.to_string_lossy(),
                false,
                HashSet::new(),
                Vec::new(),
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions {
                    text: false,
                    json_path: Some(json_path.clone()),
                },
            )
            .await
        }
    });

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    let profile: RunProfile =
        serde_json::from_str(&fs::read_to_string(&json_path).expect("read profile.json"))
            .expect("parse profile.json");

    let user_timing_bucket = profile
        .by_kind
        .iter()
        .find(|bucket| bucket.kind == "user_timing")
        .unwrap_or_else(|| panic!("user_timing bucket missing: {:?}", profile.by_kind));
    assert_eq!(
        user_timing_bucket.count, 1,
        "expected exactly one user_timing span, got bucket {:?}",
        user_timing_bucket
    );
}

#[test]
fn profile_disabled_means_no_stderr_section() {
    let tempdir = TempDir::new().expect("temp dir");
    let script = tempdir.path().join("script.harn");
    fs::write(
        &script,
        r#"
pipeline main() {
  __io_println("hi")
}
"#,
    )
    .expect("write script");

    let outcome: RunOutcome = run_in_harn_runtime({
        let script = script.clone();
        move || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            execute_run(
                &script.to_string_lossy(),
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
    });

    assert_eq!(outcome.exit_code, 0, "stderr:\n{}", outcome.stderr);
    assert!(
        !outcome.stderr.contains("Run profile"),
        "did not expect profile output without flag, got:\n{}",
        outcome.stderr
    );
}
