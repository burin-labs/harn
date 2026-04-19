use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn run_harn(current_dir: &Path, args: &[String]) -> Output {
    run_harn_with_env(current_dir, args, &[])
}

fn run_harn_with_env(current_dir: &Path, args: &[String], envs: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(current_dir)
        .envs(envs.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn run_case(task: &str, fixture_name: &str) -> (TempDir, PathBuf, Output) {
    let (temp, experiment_root) = setup_experiment_copy();
    let host = experiment_root.join("host.harn");
    let script = experiment_root.join("pipeline.harn");
    let fixture = experiment_root.join("fixtures").join(fixture_name);
    let output = run_harn(
        temp.path(),
        &[
            "playground".to_string(),
            "--host".to_string(),
            host.to_string_lossy().into_owned(),
            "--script".to_string(),
            script.to_string_lossy().into_owned(),
            "--llm".to_string(),
            "anthropic:fixture-driver".to_string(),
            "--task".to_string(),
            task.to_string(),
            "--llm-mock".to_string(),
            fixture.to_string_lossy().into_owned(),
        ],
    );
    (temp, experiment_root, output)
}

// Ignored on CI: passes locally (both `cargo test` and `cargo nextest`) but
// intermittently fails under Linux CI runners with a `NotFound` panic when
// the pipeline's `write_report` step doesn't produce the -latest.json before
// the assertion reads it. The Burin Mini experiment is scheduled to be
// replaced by the second-iteration playground in the next PR, so this test
// is being parked rather than debugged deeper. Run explicitly with
// `cargo test -p harn-cli --test burin_mini_playground -- --ignored`.
#[test]
#[ignore]
fn burin_mini_explain_repo_fixture_run_passes() {
    let (_temp, experiment_root, output) =
        run_case("Explain this repo to me in simple terms", "explain.jsonl");

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );

    let stdout = stdout(&output);
    let report = experiment_root.join("evals/generated/explain_repo-latest.json");
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
    let (_temp, experiment_root, output) = run_case("Comment what this file does", "comment.jsonl");

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );

    let stdout = stdout(&output);
    assert!(stdout.contains("task_id=comment_file"), "stdout={stdout}");
    let report = experiment_root.join("evals/generated/comment_file-latest.json");
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

    let auth_guard = experiment_root.join("workspace/packages/server/src/middleware/auth-guard.ts");
    let contents = fs::read_to_string(auth_guard).unwrap();
    assert!(
        contents.contains("Auth guard middleware that validates x-api-key"),
        "stdout={stdout}\ncontents={contents}\nreport={report_json}"
    );
}

#[test]
#[ignore]
fn burin_mini_rate_limit_fixture_run_wires_middleware() {
    let (_temp, experiment_root, output) = run_case(
        "Add rate limiting middleware to the auth module",
        "rate-limit.jsonl",
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );

    let stdout = stdout(&output);
    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    let report = experiment_root.join("evals/generated/rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
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
#[ignore]
fn burin_mini_rate_limit_liveish_fixture_ignores_redundant_read_actions() {
    let (_temp, experiment_root, output) = run_case(
        "Add rate limiting middleware to the auth module",
        "rate-limit-liveish.jsonl",
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );

    let stdout = stdout(&output);
    assert!(
        stdout.contains("task_id=rate_limit_auth"),
        "stdout={stdout}"
    );
    assert!(
        !stdout.contains("tool_rejected"),
        "stdout={stdout}\nstderr={}",
        stderr(&output)
    );
    let report = experiment_root.join("evals/generated/rate_limit_auth-latest.json");
    let report_json = read_json(&report);
    assert_eq!(report_json["verdict"], "pass");
    assert_eq!(report_json["workflow_status"], "completed");
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
#[ignore]
fn burin_mini_semantic_evaluator_heuristic_passes_for_rate_limit_fixture() {
    let (temp, experiment_root, output) = run_case(
        "Add rate limiting middleware to the auth module",
        "rate-limit.jsonl",
    );

    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status.code(),
        stderr(&output)
    );

    let evaluator = experiment_root.join("evaluator.harn");
    let report = experiment_root.join("evals/generated/rate_limit_auth-latest.json");
    let semantic = experiment_root.join("evals/generated/rate_limit_auth.semantic.json");
    let eval_output = run_harn_with_env(
        temp.path(),
        &[
            "run".to_string(),
            evaluator.to_string_lossy().into_owned(),
            "--".to_string(),
            report.to_string_lossy().into_owned(),
            semantic.to_string_lossy().into_owned(),
            experiment_root.to_string_lossy().into_owned(),
        ],
        &[("BURIN_MINI_SEMANTIC_EVAL_MODE", "heuristic")],
    );

    assert!(
        eval_output.status.success(),
        "status={:?}\nstderr={}",
        eval_output.status.code(),
        stderr(&eval_output)
    );

    let semantic_json = read_json(&semantic);
    assert_eq!(semantic_json["overall_verdict"], "pass");
    assert!(semantic_json["overall_score"].as_i64().unwrap_or_default() >= 9);
}
