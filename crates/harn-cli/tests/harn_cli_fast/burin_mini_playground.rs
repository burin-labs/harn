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
use harn_cli::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunOutcome, RunProfileOptions,
    RunSandboxOptions,
};
use harn_cli::tests::common::{cwd_lock, harn_state_lock};
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

/// Summarize each `execution.run.stages[]` entry (a batch, a verify stage,
/// or the final verifier) as one line, and for a failed stage pull the tail
/// of its `tool_call_update` errors out of the buried transcript so the
/// actual verifier/tool failure — a Node "not recognized" error, a wrong
/// cwd, an unresolved PATH entry — reads directly in the panic message
/// instead of requiring a human to grep the full nested report by hand.
/// `report` still prints in full below this summary, so nothing is lost;
/// this only puts the part someone needs first, first.
fn summarize_stages(report: &serde_json::Value) -> String {
    let Some(stages) = report["stages"].as_array() else {
        return "  (no execution.run.stages in this report)\n".to_string();
    };
    let mut out = String::new();
    for stage in stages {
        let node_id = stage["node_id"]
            .as_str()
            .or_else(|| stage["kind"].as_str())
            .unwrap_or("<unnamed-stage>");
        let status = stage["status"].as_str().unwrap_or("<unknown-status>");
        let outcome = stage["outcome"].as_str().unwrap_or("<unknown-outcome>");
        out.push_str(&format!(
            "  stage {node_id}: status={status} outcome={outcome}\n"
        ));
        if status != "failed" {
            continue;
        }
        let Some(events) = stage["transcript"]["events"].as_array() else {
            continue;
        };
        // `raw_input` (the tool's own arguments, e.g. its shell command) is
        // carried by the initiating `tool_call` event and by an
        // `in_progress` `tool_call_update`, but NOT by the terminal
        // `"status":"failed"` update — that update only carries the
        // outcome. Reading `raw_input` off the failed event itself (as an
        // earlier version of this function did) silently prints an empty
        // command on every real failure; a lookup by `tool_call_id` across
        // the whole stage finds it on the event that actually carries it.
        let mut commands_by_call_id: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for event in events {
            let Some(call_id) = event["metadata"]["tool_call_id"].as_str() else {
                continue;
            };
            if let Some(command) = event["metadata"]["raw_input"]["command"].as_str() {
                commands_by_call_id.insert(call_id, command);
            }
        }
        for event in events {
            if event["kind"] != "tool_call_update" || event["metadata"]["status"] != "failed" {
                continue;
            }
            let tool_name = event["metadata"]["tool_name"].as_str().unwrap_or("?");
            let call_id = event["metadata"]["tool_call_id"].as_str().unwrap_or("");
            let command = commands_by_call_id.get(call_id).copied().unwrap_or("");
            let error = event["metadata"]["error"].as_str().unwrap_or("");
            // The tool's own error text is a single long line; a tail is
            // plenty to see "not recognized" / "cannot find" / a wrong path
            // without dumping the whole (often multi-KB) tool payload.
            let tail: String = error
                .chars()
                .rev()
                .take(600)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            out.push_str(&format!(
                "    failed tool_call_update: tool={tool_name} command={command:?}\n    error tail: {tail}\n"
            ));
        }
    }
    out
}

fn assert_report_passes(report: &serde_json::Value, stdout: &str) {
    if report["verdict"] == "pass" {
        return;
    }
    let stage_summary = summarize_stages(report);
    // On Windows, a failure here has repeatedly turned out to be a `node`
    // resolution problem inside the sandboxed `run` tool (harn#7993), and the
    // standalone diagnostic probe test that used to be the only place this
    // was visible does not reliably show a PASS/FAIL line under nextest's
    // `ci` profile (`success-output = "never"` on its siblings, and its own
    // line has gone missing from at least one real CI run). Driving the same
    // probe from inside THIS already-failing test's own panic message
    // guarantees visibility: nextest's `failure-output = "immediate-final"`
    // always prints a failing test's message in full, with no dependency on
    // whether a sibling test's own report renders.
    #[cfg(windows)]
    let env_probe = format!(
        "\nwindows env probe (same Inherited-policy run tool, run inline \
         because this test just failed):\n{}\n",
        windows_env_probe_dump()
    );
    #[cfg(not(windows))]
    let env_probe = String::new();
    panic!(
        "playground report did not pass\nstdout:\n{stdout}\nstage summary (failed tool errors, if any):\n{stage_summary}{env_probe}\nfull report:\n{report}"
    );
}

/// Run the same `where node & set ...` probe
/// `windows_only_diagnostic_probe_of_the_sandboxed_run_child_env` runs, in its
/// own throwaway sandbox, through the same `Inherited`-policy sandboxed `run`
/// tool. Returns a formatted dump (never panics) so callers can fold it into
/// their own failure message regardless of whether this probe itself
/// resolved `node`.
///
/// Each command below is its OWN `harness.process.exec` call, and status is
/// read from Harn's own structured `result.status`/`result.success`, never
/// from a same-line `%ERRORLEVEL%` expansion: cmd.exe parses and expands
/// every `%VAR%` on a logical line BEFORE running any of the `&`-joined
/// commands on it, so `cmd & set X=%ERRORLEVEL% & echo %X%` always reports
/// the errorlevel from BEFORE `cmd` ran, not its actual result — a real
/// defect an earlier version of this probe had (harn#7993 round 2), which
/// made two earlier "PASS" verdicts on this exact check vacuous. Splitting
/// each command into its own call sidesteps the whole class of same-line
/// expansion timing bugs.
#[cfg(windows)]
fn windows_env_probe_dump() -> String {
    let temp = TempDir::new().unwrap();
    let sandbox_root = temp.path().to_path_buf();
    let script = sandbox_root.join("probe.harn");
    fs::write(
        &script,
        r#"
fn dump(harness: Harness, label: string, command: string) {
  let result = harness.process.exec("cmd.exe", "/D", "/C", command)
  harness.stdio.println("${label}_STATUS=${result.status}")
  harness.stdio.println("${label}_SUCCESS=${result.success}")
  harness.stdio.println("${label}_STDOUT_START")
  harness.stdio.println(result.stdout)
  harness.stdio.println("${label}_STDOUT_END")
  harness.stdio.println("${label}_STDERR_START")
  harness.stdio.println(result.stderr)
  harness.stdio.println("${label}_STDERR_END")
}

fn main(harness: Harness) {
  dump(harness, "WHERE", "where node")
  dump(harness, "WHOAMI", "whoami /groups /fo list")
  dump(harness, "ICACLS", "icacls \"C:\\Program Files\\nodejs\"")
  dump(harness, "TYPE", "type \"C:\\Program Files\\nodejs\\package.json\"")
  dump(harness, "ENV", "echo PROBE_PATH=%PATH% & echo PROBE_PATHEXT=%PATHEXT% & echo PROBE_COMSPEC=%ComSpec% & echo PROBE_SYSTEMROOT=%SystemRoot% & echo PROBE_CD=%CD% & echo PROBE_ENV_START & set & echo PROBE_ENV_END")
}
"#,
    )
    .unwrap();

    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = harn_state_lock::lock_harn_state_async().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        execute_run_with_sandbox_options(
            &script.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(sandbox_root),
        )
        .await
    });

    let where_status_ok = outcome
        .stdout
        .lines()
        .any(|line| line.trim() == "WHERE_STATUS=0");
    format!(
        "where_node_ok={where_status_ok}\nharness exec exit_code={}\nfull script stdout (WHERE = `where node`; WHOAMI = the token's groups and integrity level; ICACLS = the nodejs install dir's ACL; TYPE = a plain read of a file inside it; ENV = PATH/PATHEXT/ComSpec/SystemRoot/CD and the full child env key list):\n{}\nfull script stderr:\n{}",
        outcome.exit_code, outcome.stdout, outcome.stderr
    )
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
            // harn_state_lock + cwd_lock together: pipeline/host scripts read
            // both process-wide env vars (HARN_TASK / HARN_LLM_*) and the cwd
            // via RunExecutionRecord.cwd, so we must serialize on both fronts.
            let _env_guard = harn_state_lock::lock_harn_state_async().await;
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
    assert_report_passes(&report_json, &stdout);
    assert!(
        stdout.contains("small TypeScript auth API demo"),
        "stdout={stdout}\nreport={report_json}"
    );
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
    assert_report_passes(&report_json, &stdout);
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
    // The verification is deliberately a Node script and not a shell `grep`.
    // `grep -n 'Auth guard middleware' <path>` is Unix-shell-shaped twice over:
    // `grep` is absent from a stock Windows PATH, and cmd.exe does not strip
    // single quotes, so even where grep exists the pattern arrives as three
    // arguments. That command failed on Windows, and before harn#7915 a run
    // whose only verification failed still sealed `done`, so this test was
    // green on a run that verified nothing (harn#7968).
    assert_eq!(
        verify_action["command"].as_str(),
        Some("node scripts/verify-comment.js")
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
    assert_report_passes(&report_json, &stdout);
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
        Some("node scripts/verify-rate-limit.js")
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
    assert_report_passes(&report_json, &stdout);
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
    assert_report_passes(&report_json, &stdout);
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
        Some("node scripts/verify-rate-limit.js")
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
    assert_report_passes(&report_json, &stdout);
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
    let sandbox_root = temp.path().to_path_buf();

    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = harn_state_lock::lock_harn_state_async().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        std::env::set_var("BURIN_MINI_SEMANTIC_EVAL_MODE", "heuristic");
        let result = execute_run_with_sandbox_options(
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
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(sandbox_root),
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

#[cfg(test)]
mod stage_summary_tests {
    use super::summarize_stages;
    use serde_json::json;

    /// Trimmed to the fields `summarize_stages` reads, from the actual
    /// `Workspace nextest (windows-latest)` CI failure this instrumentation
    /// was written for (job 100748266102, harn#7993): the "run" tool's
    /// `node scripts/verify-comment.js` call failed on Windows with
    /// `'node' is not recognized as an internal or external command`. Before
    /// this change, the panic message only printed the top-level report,
    /// which requires paging through several KB of nested transcript JSON
    /// to find that one line. This is a real report shape, not an invented
    /// one — the three-event `tool_call` / `tool_call_update` (in_progress)
    /// / `tool_call_update` (failed) sequence, and specifically that only
    /// the first two carry `raw_input` while the terminal failure does not,
    /// is copied from the actual job log, not guessed. An earlier version
    /// of this fixture put `raw_input` directly on the failed event, which
    /// is not the real shape: it made the matching version of
    /// `summarize_stages` (reading `raw_input` off the failed event) pass
    /// this test while still printing `command=""` in production, exactly
    /// the false-positive this file's [[a-probe-that-cannot-return-a-non-null-answer-is-not-a-probe]]
    /// discipline warns about.
    fn windows_node_not_recognized_report() -> serde_json::Value {
        json!({
            "verdict": "fail",
            "stages": [
                {
                    "node_id": "batch_1",
                    "status": "completed",
                    "outcome": "completed",
                    "transcript": {"events": []},
                },
                {
                    "node_id": "batch_2",
                    "status": "failed",
                    "outcome": "completion_unverified",
                    "transcript": {
                        "events": [
                            {
                                "kind": "tool_call",
                                "metadata": {
                                    "status": "pending",
                                    "tool_name": "run",
                                    "tool_call_id": "mock_call_5",
                                    "raw_input": {"command": "node scripts/verify-comment.js"},
                                },
                            },
                            {
                                "kind": "tool_call_update",
                                "metadata": {
                                    "status": "in_progress",
                                    "tool_name": "run",
                                    "tool_call_id": "mock_call_5",
                                    "raw_input": {"command": "node scripts/verify-comment.js"},
                                },
                            },
                            {
                                "kind": "tool_call_update",
                                "metadata": {
                                    "status": "failed",
                                    "tool_name": "run",
                                    "tool_call_id": "mock_call_5",
                                    "error": "{combined: 'node' is not recognized as an internal or external command,\r\noperable program or batch file.\r\n, exit_code: 1, pid: nil, status: completed, stderr: 'node' is not recognized as an internal or external command,\r\noperable program or batch file.\r\n, success: false}",
                                },
                            },
                        ]
                    },
                },
                {
                    "node_id": "final_verify",
                    "status": "completed",
                    "outcome": "completed",
                    "transcript": {"events": []},
                },
            ],
        })
    }

    #[test]
    fn a_failed_tool_call_names_the_command_and_the_real_error_text() {
        let summary = summarize_stages(&windows_node_not_recognized_report());
        assert!(
            summary.contains("stage batch_2: status=failed"),
            "summary={summary}"
        );
        assert!(
            summary.contains("command=\"node scripts/verify-comment.js\""),
            "the failing stage's actual command must be named, not just its id\nsummary={summary}"
        );
        assert!(
            summary.contains("not recognized as an internal or external command"),
            "the tool's own error text must surface, not just status=failed\nsummary={summary}"
        );
        // The two completed stages get a one-line status each and nothing
        // more: a passing stage's transcript is not worth paging through.
        assert!(summary.contains("stage batch_1: status=completed"));
        assert!(summary.contains("stage final_verify: status=completed"));
        assert_eq!(
            summary.matches("failed tool_call_update:").count(),
            1,
            "only the failed stage's failed tool call should be pulled out\nsummary={summary}"
        );
    }

    #[test]
    fn a_report_with_no_stages_array_says_so_instead_of_panicking() {
        let summary = summarize_stages(&json!({"verdict": "fail"}));
        assert!(summary.contains("no execution.run.stages"), "{summary}");
    }
}

/// DIAGNOSTIC PROBE (harn#7993). Asserts that `where node` resolves inside an
/// `Inherited`-policy sandboxed `run`-tool child — the exact seam
/// `burin_mini_comment_file_fixture_run_updates_workspace_copy`'s failing
/// `node scripts/verify-comment.js` call goes through —
/// `harness.process.exec` -> `process_command_config` ->
/// `session_closed_env` -> `crate::security::resolve_env` — under
/// `RunSandboxOptions::default()`, the same default `Inherited` policy that
/// test runs under. `windows_env_probe_dump` does the actual work and never
/// panics; `assert_report_passes` also calls it inline on a Windows failure
/// so the diagnostic is guaranteed to appear in a report that is ALREADY
/// failing (and therefore always shown in full), rather than depending on
/// this test's own PASS/FAIL line rendering under nextest's `ci` profile.
/// Keep both alongside the fixture test as a standing regression guard once
/// the Windows red above is fixed; this one is cheaper to run in isolation
/// and pinpoints the same seam.
#[cfg(windows)]
#[test]
fn windows_only_diagnostic_probe_of_the_sandboxed_run_child_env() {
    let dump = windows_env_probe_dump();
    // `eprintln!` (not `harness.stdio`) so the transcript survives even if
    // the probe script itself never got far enough to print anything.
    eprintln!(
        "=== windows_only_diagnostic_probe_of_the_sandboxed_run_child_env ===\n{dump}\n=== end probe ==="
    );
    assert!(
        dump.starts_with("where_node_ok=true"),
        "the sandboxed run tool's child could not resolve 'node' via \
         `where node`, under the SAME Inherited environment policy the \
         burin-mini playground test uses\n{dump}"
    );
}

/// Every `run` tool call the report contains, as
/// `(command, status, raw_output, error)`.
///
/// `raw_input` (and therefore the command string) is carried by the
/// initiating `tool_call` event and by an `in_progress` update, but not by
/// the terminal update that carries the status and output, so the two halves
/// are joined by `tool_call_id`. Reading the command off the terminal event
/// alone yields an empty string on every real failure.
#[cfg(windows)]
fn run_tool_calls_by_command(report: &serde_json::Value) -> Vec<(String, String, String, String)> {
    let mut calls = Vec::new();
    let Some(stages) = report["stages"].as_array() else {
        return calls;
    };
    for stage in stages {
        let Some(events) = stage["transcript"]["events"].as_array() else {
            continue;
        };
        let mut commands_by_call_id: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for event in events {
            let Some(call_id) = event["metadata"]["tool_call_id"].as_str() else {
                continue;
            };
            if let Some(command) = event["metadata"]["raw_input"]["command"].as_str() {
                commands_by_call_id.insert(call_id, command);
            }
        }
        for event in events {
            if event["kind"] != "tool_call_update" || event["metadata"]["tool_name"] != "run" {
                continue;
            }
            let status = event["metadata"]["status"].as_str().unwrap_or("<none>");
            if status == "in_progress" {
                continue;
            }
            let call_id = event["metadata"]["tool_call_id"].as_str().unwrap_or("");
            calls.push((
                commands_by_call_id
                    .get(call_id)
                    .copied()
                    .unwrap_or("")
                    .to_string(),
                status.to_string(),
                event["metadata"]["raw_output"].to_string(),
                event["metadata"]["error"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            ));
        }
    }
    calls
}

/// True when `text` carries something shaped like a Node version banner: a
/// `v` immediately followed by a digit. `node --version` prints exactly that
/// and nothing else, so this distinguishes a real answer from an empty
/// output or a shell's "not recognized" complaint, neither of which can
/// match.
#[cfg(windows)]
fn contains_version_banner(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .windows(2)
        .any(|pair| pair[0] == b'v' && pair[1].is_ascii_digit())
}

/// The Windows sandbox must let a confined child READ the system toolchains
/// the machine already has, while still confining its WRITES to the
/// workspace. This test pins the read half at the product seam: it drives
/// `execute_playground_inputs` — the same entry point, experiment and
/// mocked-LLM orchestration the five fixture tests above use — and runs four
/// read-only commands through the `run` tool, then asserts the child can
/// resolve and execute a Node interpreter that lives outside the workspace.
///
/// The three commands beside `node --version` exist so a failure explains
/// itself in one CI round instead of costing another: `whoami /groups` names
/// the child's own groups, capabilities and mandatory integrity level;
/// `icacls` on the interpreter's directory shows whether that directory's
/// ACL grants anything the child's token carries; and `type` on a small file
/// inside it separates "the directory cannot be listed" from "a file in it
/// cannot be opened". All four outputs print in the failure message, so the
/// dump is available whether the assertion passes or fails for a reason the
/// assertion itself does not name.
///
/// Read-only by construction: none of the four commands writes anything. The
/// matching write-confinement assertion is a separate test; this one must
/// never be turned into a write probe.
#[cfg(windows)]
#[test]
fn windows_sandboxed_run_child_can_read_a_system_toolchain_outside_the_workspace() {
    let (_temp, experiment_root) = setup_experiment_copy();
    let outcome = run_playground_case(
        experiment_root.clone(),
        "Comment what this file does".to_string(),
        "windows_sandbox_read_diagnostic.jsonl",
    );
    let stdout = match &outcome {
        Ok(stdout) => stdout.clone(),
        Err(error) => error.clone(),
    };
    let report_path = generated_report_path(&experiment_root, &stdout, "comment_file-latest.json");
    let calls = if report_path.exists() {
        run_tool_calls_by_command(&read_json(&report_path))
    } else {
        Vec::new()
    };

    let mut dump = String::new();
    if calls.is_empty() {
        dump.push_str(&format!(
            "  (no run tool calls found; report at {} exists={}, playground result: {outcome:?})\n",
            report_path.display(),
            report_path.exists()
        ));
    }
    for (command, status, raw_output, error) in &calls {
        dump.push_str(&format!(
            "  command={command:?}\n    status={status}\n    raw_output={raw_output}\n    error={error}\n"
        ));
    }

    let node_call = calls
        .iter()
        .find(|(command, ..)| command.trim() == "node --version");
    let node_ok = node_call.is_some_and(|(_, status, raw_output, _)| {
        status != "failed" && contains_version_banner(raw_output)
    });
    assert!(
        node_ok,
        "a sandboxed `run` child could not read and execute a Node interpreter \
         installed outside the workspace, so every system toolchain on this \
         machine is invisible to the agent. The four read-only diagnostic \
         commands and their outputs:\n{dump}\nplayground stdout/error:\n{stdout}"
    );
}

/// ADVERSARIAL WRITE-CONFINEMENT CHECK (harn#7993). Every candidate fix for
/// the Windows read-closed defect widens READS; none of them may widen
/// WRITES. This drives the same seam the failing fixtures use
/// (`execute_playground_inputs` -> the `run` tool -> `process.shell_at` ->
/// `run_captured_spawn` -> the Windows sandbox backend) and checks the real
/// filesystem afterward, not `cmd.exe`'s own stdout wording — different
/// candidates may implement confinement through different mechanisms with
/// different denial text/exit codes, but the file either exists on disk or
/// it does not, and that check is mechanism-agnostic.
///
/// Both write targets sit under `%USERPROFILE%`, OUTSIDE the workspace: one
/// directly in the profile root (a path that exists on every machine), and
/// one inside a fresh subdirectory this test creates itself (proving, on
/// the host side and before the sandboxed run, that the subdirectory exists
/// and is writable) — so a candidate cannot pass by only closing off a
/// well-known top-level path while leaving a freshly created one open. A
/// failure to deny either write can only be the sandbox's own write
/// confinement, never an ambient OS permission the account never had.
///
/// This deliberately does NOT target the system temp directory the way an
/// earlier version of this test did. The temp directory is a legitimately
/// granted write root under this backend's own `UserTemp` preset whenever
/// workspace writes are allowed, so a successful write there is the policy
/// working correctly, not an escape — using it as a denial arm produced a
/// false violation. (An even earlier version used `%TEMP%` directly inside
/// the sandboxed command, which was vacuous for a different reason: the
/// Windows backend overrides the child's own `TEMP`/`TMP` to an
/// AppContainer-local path — see `environment_overrides` in `windows.rs` —
/// so `%TEMP%` expanded inside the child never resolved to the host path
/// this test was checking.)
///
/// A third write lands inside the workspace (the run tool's cwd is
/// `workspace_root(fs)`, see `experiments/burin-mini/lib/workspace.harn`)
/// and MUST succeed: a candidate that closes reads by also closing writes
/// it used to allow is a regression, not a fix, even if the 5 real
/// fixtures happen to pass.
///
/// Before trusting an absent escape file as a genuine denial, a negative
/// control writes to the exact same two `%USERPROFILE%`-rooted paths
/// through an *unsandboxed* spawn (no harn sandbox in the path at all) and
/// asserts both files appear, then removes them. Skipping this would make
/// the whole test vacuous in exactly the way the earlier `%TEMP%` checks
/// were: an absent file proves nothing unless something first proves the
/// file would have been there to find.
///
/// Any escape file this test finds is deleted before the assertion panics,
/// and the subdirectory it creates is always removed on the way out, so a
/// broken candidate does not leave stray files or directories on the
/// runner.
#[cfg(windows)]
#[test]
fn windows_sandbox_fix_keeps_writes_confined_to_the_workspace() {
    let (_temp, experiment_root) = setup_experiment_copy();

    let userprofile_dir = std::env::var("USERPROFILE").expect(
        "USERPROFILE must be set to run the write-confinement check or its negative control",
    );
    let userprofile_dir = PathBuf::from(userprofile_dir);
    let userprofile_escape = userprofile_dir.join("harn-escape-7993-userprofile.txt");
    let subdir = userprofile_dir.join("harn-escape-7993-subdir");
    let subdir_escape = subdir.join("harn-escape-7993-nested.txt");

    // Create the subdirectory on the host side and prove, before the
    // sandboxed run, that this (unsandboxed) test process can actually
    // write inside it. If this fails, the real check below would not be
    // measuring the sandbox at all.
    fs::create_dir_all(&subdir).unwrap_or_else(|error| {
        panic!(
            "could not create the write-confinement subdirectory {} on the host: {error}",
            subdir.display()
        )
    });
    let subdir_sentinel = subdir.join("harn-subdir-host-writable-sentinel-7993.txt");
    fs::write(&subdir_sentinel, "host-writable").unwrap_or_else(|error| {
        panic!(
            "the write-confinement subdirectory {} is not host-writable, so it cannot be used \
             as a denial target: {error}",
            subdir.display()
        )
    });
    fs::remove_file(&subdir_sentinel).unwrap_or_else(|error| {
        panic!(
            "could not clean up the host-writable sentinel at {}: {error}",
            subdir_sentinel.display()
        )
    });

    // Negative control: prove the detector can see a write at these exact
    // paths before trusting that an absent file means the sandbox denied
    // it. Same target paths, no sandbox anywhere in the call path.
    let control_status = std::process::Command::new("cmd.exe")
        .args([
            "/D",
            "/C",
            &format!(
                "echo control > \"{}\" & echo control > \"{}\"",
                userprofile_escape.display(),
                subdir_escape.display()
            ),
        ])
        .status()
        .expect("spawn the unsandboxed write-confinement negative control");
    assert!(
        control_status.success(),
        "unsandboxed negative control command itself failed to run"
    );
    for control_path in [&userprofile_escape, &subdir_escape] {
        assert!(
            control_path.exists(),
            "negative control: an unsandboxed write to {} did not appear; the detector \
             cannot tell a real denial from a broken probe, so the real check below would \
             prove nothing",
            control_path.display()
        );
    }
    for control_path in [&userprofile_escape, &subdir_escape] {
        fs::remove_file(control_path).unwrap_or_else(|error| {
            panic!(
                "negative control: could not clean up {} before the real run: {error}",
                control_path.display()
            )
        });
    }

    let outcome = run_playground_case(
        experiment_root.clone(),
        "Comment what this file does".to_string(),
        "windows_write_confinement_probe.jsonl",
    );
    let stdout = match &outcome {
        Ok(stdout) => stdout.clone(),
        Err(error) => error.clone(),
    };

    let inside_write = experiment_root
        .join("workspace")
        .join("harn-write-confinement-7993-inside.txt");

    let mut failures = Vec::new();
    for (label, escape_path) in [
        ("USERPROFILE", &userprofile_escape),
        ("USERPROFILE subdirectory", &subdir_escape),
    ] {
        if escape_path.exists() {
            let contents = fs::read_to_string(escape_path).unwrap_or_default();
            failures.push(format!(
                "  WRITE ESCAPED CONFINEMENT: {label} target {} exists (contents: {contents:?}) \
                 -- the sandboxed run tool wrote outside the workspace",
                escape_path.display()
            ));
            let _ = fs::remove_file(escape_path);
        }
    }
    match fs::read_to_string(&inside_write) {
        Ok(contents) if contents.contains("inside-workspace") => {}
        Ok(contents) => failures.push(format!(
            "  WRITE INSIDE THE WORKSPACE PRODUCED WRONG CONTENT: {} = {contents:?}",
            inside_write.display()
        )),
        Err(error) => failures.push(format!(
            "  WRITE INSIDE THE WORKSPACE DID NOT HAPPEN: {} ({error}) -- a confinement fix \
             must not also break writes the sandbox is supposed to allow",
            inside_write.display()
        )),
    }

    let _ = fs::remove_dir_all(&subdir);

    if failures.is_empty() {
        return;
    }
    let report_path = generated_report_path(&experiment_root, &stdout, "comment_file-latest.json");
    let report_dump = if report_path.exists() {
        format!("{}", read_json(&report_path))
    } else {
        format!("(no report at {})", report_path.display())
    };
    panic!(
        "write-confinement check failed:\n{}\nplayground stdout/error:\n{stdout}\nfull report:\n{report_dump}",
        failures.join("\n")
    );
}
