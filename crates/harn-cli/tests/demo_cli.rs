#![recursion_limit = "256"]

//! In-process coverage of `harn demo` (#1650).
//!
//! Each bundled scenario must run end-to-end against its checked-in
//! offline tape with no API keys, no network, no provider config.
//! These tests are the drift gate: if a scenario script changes shape
//! but the tape doesn't (or vice versa), this suite goes red.

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;

use harn_cli::commands::demo::scenario_ids;
use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};
use harn_cli::tests::common::{cwd_lock, env_lock};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-demo-test".to_string())
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

fn run_demo_scenario(id: &str) -> RunOutcome {
    let assets = PathBuf::from(MANIFEST_DIR).join("assets/demo").join(id);
    let script = assets.join("scenario.harn");
    let tape = assets.join("tape.jsonl");
    assert!(script.is_file(), "missing scenario.harn for {id}");
    assert!(tape.is_file(), "missing tape.jsonl for {id}");
    run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        execute_run(
            script.to_string_lossy().as_ref(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Replay {
                fixture_path: tape.clone(),
            },
            None,
            RunProfileOptions::default(),
        )
        .await
    })
}

#[test]
fn merge_captain_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("merge-captain");
    assert_eq!(
        outcome.exit_code, 0,
        "merge-captain demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("merge_supervision_receipt"),
        "merge-captain stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("[#421]")
            && outcome.stdout.contains("[#422]")
            && outcome.stdout.contains("[#423]"),
        "merge-captain demo should triage all three PRs:\n{}",
        outcome.stdout
    );
}

#[test]
fn review_captain_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("review-captain");
    assert_eq!(
        outcome.exit_code, 0,
        "review-captain demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("review_receipt"),
        "review-captain stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("clarifying_question_asked"),
        "review-captain demo should record HITL question:\n{}",
        outcome.stdout
    );
}

#[test]
fn provider_race_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("provider-race");
    assert_eq!(
        outcome.exit_code, 0,
        "provider-race demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("race_attribution_receipt"),
        "provider-race stdout missing attribution receipt:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("[anthropic]")
            && outcome.stdout.contains("[openai]")
            && outcome.stdout.contains("[ollama]"),
        "provider-race demo should report all three providers:\n{}",
        outcome.stdout
    );
}

#[test]
fn routing_policy_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("routing-policy");
    assert_eq!(
        outcome.exit_code, 0,
        "routing-policy demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("routing_supervision_receipt"),
        "routing-policy stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("=== task smoke ===")
            && outcome.stdout.contains("=== task rate-lim ===")
            && outcome.stdout.contains("=== task lint-fail ==="),
        "routing-policy demo should exercise all three tasks:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"escalations\":2"),
        "routing-policy demo should record two escalations (rate-lim + lint-fail):\n{}",
        outcome.stdout
    );
}

#[test]
fn every_scenario_listed_has_a_passing_smoke_run() {
    // Belt-and-suspenders: if a future scenario lands in SCENARIOS but
    // someone forgets to add a per-scenario test above, this catch-all
    // exercises it through the same offline-tape path.
    let known: HashSet<&str> = [
        "merge-captain",
        "review-captain",
        "provider-race",
        "routing-policy",
    ]
    .into_iter()
    .collect();
    for id in scenario_ids() {
        if known.contains(id) {
            continue;
        }
        let outcome = run_demo_scenario(id);
        assert_eq!(
            outcome.exit_code, 0,
            "demo scenario `{id}` failed (exit {}):\nstderr:\n{}",
            outcome.exit_code, outcome.stderr
        );
    }
}
