#![recursion_limit = "256"]

//! In-process coverage of the `experiments/burin-mini` playground pipelines.
//!
//! Tier 1H follow-up (#1130, parent #1106) of the de-flake epic (#1057):
//! these tests previously ran the `harn` binary as a subprocess. They now
//! call `harn_cli::commands::playground::execute_playground_inputs` and
//! `harn_cli::commands::run::execute_run` directly, asserting on the
//! captured stdout / generated report files.
//!
//! These tests build their own multi-thread tokio runtime on a dedicated
//! thread, mirroring `harn_cli::run`'s setup, so the LLM-mock thread-local
//! state and `LocalSet` semantics match what `harn playground` sees in
//! production.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use harn_cli::commands::playground::{execute_playground_inputs, PlaygroundInputs};
use harn_cli::commands::run::{execute_run, CliLlmMockMode, RunOutcome};
use harn_cli::tests::common::{cwd_lock, env_lock};
use tempfile::TempDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let metadata = entry.metadata().unwrap();
        if metadata.is_dir() {
            copy_tree(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
            fs::set_permissions(&dst_path, metadata.permissions()).unwrap();
        }
    }
}

fn setup_experiment_copy() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let experiment_src = repo_root().join("experiments/burin-mini");
    let experiment_dst = temp.path().join("burin-mini");
    copy_tree(&experiment_src, &experiment_dst);
    (temp, experiment_dst)
}

fn read_json(path: &Path) -> serde_json::Value {
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse {} as JSON: {error}", path.display()))
}

fn generated_report_path(experiment_root: &Path, stdout: &str, fallback_name: &str) -> PathBuf {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("report=").map(PathBuf::from))
        .unwrap_or_else(|| experiment_root.join("evals/generated").join(fallback_name))
}

/// Mirror `harn_cli::run`'s thread + multi-thread runtime setup so the
/// `LocalSet` inside `execute_playground` binds to a thread we control,
/// matching what `harn playground` does in production. The returned
/// future is run via `block_on`, so it does not need to be `Send`.
fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-test".to_string())
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

fn run_playground_case(
    experiment_root: PathBuf,
    task: String,
    fixture_name: &str,
) -> Result<String, String> {
    let host = experiment_root.join("host.harn");
    let script = experiment_root.join("pipeline.harn");
    let fixture = experiment_root.join("fixtures").join(fixture_name);
    // Match the subprocess harness's `current_dir(experiment_root.parent())`
    // so any cwd-relative state inside the pipeline resolves the same way.
    let cwd_anchor = experiment_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| experiment_root.clone());

    run_in_harn_runtime(move || {
        async move {
            // env_lock + cwd_lock together: pipeline/host scripts read both
            // process-wide env vars (HARN_TASK / HARN_LLM_*) and the cwd via
            // RunExecutionRecord.cwd, so we must serialize on both fronts.
            let _env_guard = env_lock::lock_env().lock().await;
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            // Each test invocation runs in the same process, so wipe any
            // FIFO/LLM state left by a previous case before installing the
            // new fixture.
            harn_vm::reset_thread_local_state();
            let original_cwd = std::env::current_dir().ok();
            std::env::set_current_dir(&cwd_anchor).expect("set cwd to experiment parent");
            let result = execute_playground_inputs(PlaygroundInputs {
                host,
                script,
                task,
                llm: Some("anthropic:fixture-driver".to_string()),
                llm_mock_mode: CliLlmMockMode::Replay {
                    fixture_path: fixture,
                },
            })
            .await;
            if let Some(prev) = original_cwd {
                let _ = std::env::set_current_dir(prev);
            }
            result
        }
    })
}

#[test]
fn burin_mini_explain_repo_fixture_run_passes() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Explain this repo to me in simple terms".to_string(),
        "explain.jsonl",
    )
    .expect("playground case succeeds");

    let report = generated_report_path(&experiment_root, &stdout, "explain_repo-latest.json");
    let report_json = read_json(&report);
    assert!(stdout.contains("task_id=explain_repo"), "stdout={stdout}");
    assert!(
        stdout.contains("small TypeScript auth API demo"),
        "stdout={stdout}"
    );
    assert_eq!(report_json["verdict"], "pass");
}

#[test]
fn burin_mini_comment_file_fixture_run_updates_workspace_copy() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Comment what this file does".to_string(),
        "comment.jsonl",
    )
    .expect("playground case succeeds");

    assert!(stdout.contains("task_id=comment_file"), "stdout={stdout}");
    let report = generated_report_path(&experiment_root, &stdout, "comment_file-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
    let action_ids: Vec<String> = report_json["action_graph"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(action_ids, vec!["write_comment", "verify_comment"]);
    let actions = report_json["action_graph"]["actions"]
        .as_array()
        .expect("action graph actions");
    let write_action = actions
        .iter()
        .find(|item| item["id"] == "write_comment")
        .expect("write action");
    let verify_action = actions
        .iter()
        .find(|item| item["id"] == "verify_comment")
        .expect("verify action");
    let write_instruction = write_action["instruction"]
        .as_str()
        .expect("write instruction");
    assert!(
        write_instruction.contains("Auth guard middleware"),
        "write_instruction={write_instruction}\nreport={report_json}"
    );
    assert_eq!(
        verify_action["command"].as_str(),
        Some("grep -n 'Auth guard middleware' packages/server/src/middleware/auth-guard.ts")
    );

    let auth_guard = experiment_root.join("workspace/packages/server/src/middleware/auth-guard.ts");
    let contents = fs::read_to_string(auth_guard).unwrap();
    assert!(
        contents.contains("Auth guard middleware that validates x-api-key"),
        "stdout={stdout}\ncontents={contents}\nreport={report_json}"
    );
}

#[test]
fn burin_mini_rate_limit_fixture_run_wires_middleware() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Add rate limiting middleware to the auth module".to_string(),
        "rate-limit.jsonl",
    )
    .expect("playground case succeeds");

    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    let report = generated_report_path(&experiment_root, &stdout, "rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
    assert_eq!(
        report_json["research"].as_array().map(Vec::len),
        Some(2),
        "report={report_json}"
    );
    let action_ids: Vec<String> = report_json["action_graph"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        action_ids,
        vec![
            "create_rate_limit",
            "export_rate_limit",
            "wire_routes",
            "verify_rate_limit",
        ]
    );
    let actions = report_json["action_graph"]["actions"]
        .as_array()
        .expect("action graph actions");
    let create_action = actions
        .iter()
        .find(|item| item["id"] == "create_rate_limit")
        .expect("create action");
    let export_action = actions
        .iter()
        .find(|item| item["id"] == "export_rate_limit")
        .expect("export action");
    let wire_action = actions
        .iter()
        .find(|item| item["id"] == "wire_routes")
        .expect("wire action");
    let verify_action = actions
        .iter()
        .find(|item| item["id"] == "verify_rate_limit")
        .expect("verify action");
    assert_eq!(
        create_action["target_paths"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["packages/server/src/middleware/rate-limit.ts"]
    );
    assert_eq!(
        export_action["target_paths"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["packages/server/src/middleware/index.ts"]
    );
    assert_eq!(
        wire_action["target_paths"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["packages/server/src/routes/api.ts"]
    );
    assert_eq!(
        verify_action["command"].as_str(),
        Some("./scripts/verify-rate-limit.sh")
    );
    for action in [create_action, export_action, wire_action] {
        assert_eq!(action["command"].as_str().unwrap_or(""), "");
    }

    let rate_limit = experiment_root.join("workspace/packages/server/src/middleware/rate-limit.ts");
    let index = experiment_root.join("workspace/packages/server/src/middleware/index.ts");
    let routes = experiment_root.join("workspace/packages/server/src/routes/api.ts");
    let index_contents = fs::read_to_string(index).unwrap();
    let routes_contents = fs::read_to_string(routes).unwrap();
    assert!(
        rate_limit.exists(),
        "stdout={stdout}\nindex={index_contents}\nroutes={routes_contents}\nreport={report_json}"
    );
    assert!(
        index_contents.contains("rateLimit"),
        "stdout={stdout}\nindex={index_contents}\nreport={report_json}"
    );
    assert!(
        routes_contents.contains("rateLimit"),
        "stdout={stdout}\nroutes={routes_contents}\nreport={report_json}"
    );
}

#[test]
fn burin_mini_rate_limit_liveish_fixture_ignores_redundant_read_actions() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Add rate limiting middleware to the auth module".to_string(),
        "rate-limit-liveish.jsonl",
    )
    .expect("playground case succeeds");

    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    assert!(!stdout.contains("tool_rejected"), "stdout={stdout}");
    let report = generated_report_path(&experiment_root, &stdout, "rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
    assert_eq!(
        report_json["research"].as_array().map(Vec::len),
        Some(2),
        "report={report_json}"
    );
    let action_ids: Vec<String> = report_json["action_graph"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert!(
        !action_ids.iter().any(|id| id.starts_with("act_read_")),
        "action_ids={action_ids:?}\nreport={report_json}"
    );
    assert_eq!(
        action_ids.last().map(String::as_str),
        Some("act_verify_rate_limit"),
        "action_ids={action_ids:?}\nreport={report_json}"
    );
}

#[test]
fn burin_mini_rate_limit_weak_verify_plan_normalizes_to_single_verify_action() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Add rate limiting middleware to the auth module".to_string(),
        "rate-limit-weak-verify-plan.jsonl",
    )
    .expect("playground case succeeds");

    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    assert!(!stdout.contains("tool_rejected"), "stdout={stdout}");
    let report = generated_report_path(&experiment_root, &stdout, "rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
    let actions = report_json["action_graph"]["actions"]
        .as_array()
        .expect("action graph actions");
    let action_ids: Vec<String> = actions
        .iter()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        action_ids,
        vec![
            "create-rate-limit-middleware",
            "update-middleware-index",
            "wire-rate-limit-to-routes",
            "run-verify-script",
        ],
        "report={report_json}"
    );
    let verify_action = actions
        .iter()
        .find(|item| item["id"] == "run-verify-script")
        .expect("verify action");
    assert_eq!(verify_action["phase"].as_str(), Some("verify"));
    assert_eq!(verify_action["tool_class"].as_str(), Some("run"));
    assert_eq!(
        verify_action["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["run"]
    );
    assert_eq!(
        verify_action["command"].as_str(),
        Some("./scripts/verify-rate-limit.sh")
    );
    assert!(
        !actions.iter().any(|item| item["id"] == "verify_output"),
        "report={report_json}"
    );
}

#[test]
fn burin_mini_rate_limit_overresearch_planner_commits_final_action_graph() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let stdout = run_playground_case(
        experiment_root.clone(),
        "Add rate limiting middleware to the auth module".to_string(),
        "rate-limit-overresearch-planner.jsonl",
    )
    .expect("playground case succeeds");

    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    let report = generated_report_path(&experiment_root, &stdout, "rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
    assert_eq!(
        report_json["research"].as_array().map(Vec::len),
        Some(4),
        "report={report_json}"
    );
    let action_ids: Vec<String> = report_json["action_graph"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        action_ids,
        vec![
            "create_rate_limit_impl",
            "export_rate_limit",
            "wire_rate_limit_in_api",
            "run_verification",
        ],
        "report={report_json}"
    );
}

#[test]
fn burin_mini_semantic_evaluator_heuristic_passes_for_rate_limit_fixture() {
    let (temp, experiment_root) = setup_experiment_copy();
    let evaluator = experiment_root.join("evaluator.harn");
    let report = experiment_root.join("evals/fixtures/rate_limit_auth-report.json");
    let semantic = temp.path().join("rate_limit_auth.semantic.json");
    let semantic_clone = semantic.clone();

    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        std::env::set_var("BURIN_MINI_SEMANTIC_EVAL_MODE", "heuristic");
        let result = execute_run(
            &evaluator.to_string_lossy(),
            false,
            HashSet::new(),
            vec![
                report.to_string_lossy().into_owned(),
                semantic_clone.to_string_lossy().into_owned(),
                experiment_root.to_string_lossy().into_owned(),
            ],
            Vec::new(),
            CliLlmMockMode::Off,
            None,
        )
        .await;
        std::env::remove_var("BURIN_MINI_SEMANTIC_EVAL_MODE");
        result
    });

    assert_eq!(
        outcome.exit_code, 0,
        "stderr={}\nstdout={}",
        outcome.stderr, outcome.stdout
    );

    let semantic_json = read_json(&semantic);
    assert_eq!(semantic_json["overall_verdict"], "pass");
    assert!(semantic_json["overall_score"].as_i64().unwrap_or_default() >= 9);
}
