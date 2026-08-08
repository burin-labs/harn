//! In-process coverage of `harn merge-captain mock` (#1020).
//!
//! Mirrors the de-flake / in-process pattern used by
//! `merge_captain_cli.rs`: every assertion calls the library surface
//! directly so we don't pay the subprocess tax. The CLI handlers in
//! `crates/harn-cli/src/commands/merge_captain_mock.rs` are thin
//! wrappers over the same surface.

use std::path::Path;

use harn_vm::orchestration::playground::{
    apply_one_action, builtin_scenario_names, cleanup_playground_at, init_playground_at,
    load_builtin, load_playground, run_named_step, InitOptions, ScenarioAction,
};

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn skip_if_no_git() -> bool {
    if !git_available() {
        eprintln!("(skipping merge-captain mock CLI test — `git` not on PATH)");
        return true;
    }
    false
}

#[test]
fn builtin_scenarios_smoke_test() {
    for name in builtin_scenario_names() {
        load_builtin(name)
            .unwrap_or_else(|e| panic!("builtin scenario {name} failed to parse: {e}"));
    }
}

#[test]
fn init_then_step_then_cleanup_round_trip() {
    if skip_if_no_git() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("pg");
    let manifest = load_builtin("three_repo_basic").unwrap();
    let _state = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();

    // status load
    let (mut state, manifest_loaded) = load_playground(&dir).unwrap();
    assert_eq!(state.scenario, "three_repo_basic");
    assert_eq!(manifest_loaded.repos.len(), 3);
    assert_eq!(state.pull_requests.len(), 3);

    // run a step that flips a check + clean mergeable.
    let report =
        run_named_step(&dir, &mut state, &manifest_loaded, "gamma_force_push_fix").unwrap();
    assert!(report.actions_applied >= 1);
    let pr = state.pull_requests.get("gamma#303").expect("PR gamma#303");
    assert_eq!(pr.mergeable_state, "clean");
    assert_eq!(
        pr.checks
            .iter()
            .find(|c| c.name == "ci")
            .and_then(|c| c.conclusion.clone())
            .as_deref(),
        Some("success")
    );
    state.save(&dir).unwrap();

    // ad-hoc action also routes through the same code.
    let action: ScenarioAction = serde_json::from_str(
        r#"{"kind":"add_comment","repo":"alpha","pr_number":101,"user":"captain","body":"ack"}"#,
    )
    .unwrap();
    let report = apply_one_action(&dir, &mut state, &manifest_loaded, &action).unwrap();
    assert_eq!(report.actions_applied, 1);
    assert_eq!(state.pull_requests["alpha#101"].comments.len(), 1);

    // cleanup is idempotent.
    assert!(cleanup_playground_at(&dir).unwrap());
    assert!(!cleanup_playground_at(&dir).unwrap());
}

#[test]
fn init_force_re_inits_existing_dir() {
    if skip_if_no_git() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("pg");
    let manifest = load_builtin("single_green").unwrap();
    init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    let err = init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap_err();
    assert!(format!("{err}").contains("already initialized"));
    cleanup_playground_at(&dir).unwrap();
    init_playground_at(InitOptions {
        dir: &dir,
        manifest: &manifest,
        allow_existing: false,
    })
    .unwrap();
    assert!(Path::new(&dir).join("playground.json").exists());
}
