#![recursion_limit = "256"]

//! In-process coverage for `harn eval coding-agent`.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::thread;

use harn_cli::cli::EvalCodingAgentArgs;
use harn_cli::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunOutcome, RunProfileOptions,
    RunSandboxOptions,
};
use harn_cli::tests::common::env_lock;

const PARITY_DIRECTORY: &str = "parity";
const PARITY_OVERLAY_FILENAME: &str = "tool_mode_parity_overlay.toml";

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-eval-coding-agent-test".to_string())
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

#[test]
fn mock_matrix_writes_artifacts_for_native_and_text_tools() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("bench");
    let args = EvalCodingAgentArgs {
        fixtures: vec!["all".to_string()],
        models: vec!["mock:mock".to_string()],
        tool_formats: vec!["native".to_string(), "text".to_string()],
        output: Some(output.clone()),
        env_files: Vec::new(),
        include_local: false,
        local_providers: Vec::new(),
        max_local_models: 2,
        keep_local_after_run: false,
        max_runs: None,
        max_iterations: 6,
        python: "python3".to_string(),
        fail_on_unauthorized: false,
        json: false,
        step_judge: None,
        step_judge_on_veto: None,
        step_judge_adversarial: false,
        structural_validator: None,
        run_label: String::new(),
        override_reason: None,
        baseline_comparison_against: None,
    };

    let exit = run_in_harn_runtime(|| async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_cli::commands::eval_coding_agent::run(args).await
    });
    assert_eq!(exit, 0, "mock coding-agent eval should pass");

    let summary_raw = fs::read_to_string(output.join("summary.json")).expect("summary exists");
    let summary: serde_json::Value =
        serde_json::from_str(&summary_raw).expect("summary parses as JSON");
    assert_eq!(summary["schema_version"], 3);
    assert_eq!(summary["fixture_ids"].as_array().map(Vec::len), Some(6));
    assert_eq!(summary["total_runs"], 12);
    assert_eq!(summary["passed_runs"], 12);
    assert_eq!(summary["skipped_runs"], 0);
    assert_eq!(summary["diverged_comparisons"], 0);
    let comparisons = summary["comparisons"]
        .as_array()
        .expect("comparisons should be an array");
    assert_eq!(comparisons.len(), 6);
    let mut comparison_fixtures = comparisons
        .iter()
        .map(|comparison| {
            comparison["fixture_id"]
                .as_str()
                .expect("comparison fixture_id")
                .to_string()
        })
        .collect::<Vec<_>>();
    comparison_fixtures.sort();
    assert_eq!(
        comparison_fixtures,
        vec![
            "cli-help-flag",
            "docs-symbol-rename",
            "no-tool-diagnosis",
            "python-add",
            "read-only-audit",
            "test-output-first",
        ],
    );
    for comparison in comparisons {
        assert_eq!(comparison["equivalent"], true);
        assert_eq!(comparison["verifier_match"], true);
        assert_eq!(comparison["tool_sequence_match"], true);
        assert_eq!(comparison["rejected_tool_call_delta_text_minus_native"], 0,);
        assert!(comparison["evidence_paths"]
            .as_array()
            .is_some_and(|paths| paths.len() == 2));
    }
    let parity_by_pair = summary["parity_by_pair"]
        .as_array()
        .expect("parity_by_pair should be an array");
    assert_eq!(parity_by_pair.len(), 1);
    assert_eq!(parity_by_pair[0]["sample_size"], 6);
    assert_eq!(parity_by_pair[0]["native"]["pass_rate"], 1.0);
    assert_eq!(parity_by_pair[0]["text"]["pass_rate"], 1.0);
    assert_eq!(parity_by_pair[0]["agreement_rate"], 1.0);
    assert_eq!(parity_by_pair[0]["verifier_divergence_rate"], 0.0);
    assert_eq!(
        summary
            .pointer("/rollups/by_fixture")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(6),
    );
    assert_eq!(
        summary
            .pointer("/rollups/by_tool_format")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2),
    );
    assert_eq!(
        summary["env_keys_loaded"].as_array().map(Vec::len),
        Some(0),
        "no env-file values should be serialized in the default smoke run",
    );

    let per_run = fs::read_to_string(output.join("per_run.jsonl")).expect("per-run JSONL exists");
    assert_eq!(per_run.lines().count(), 12);
    for line in per_run.lines() {
        let row: serde_json::Value = serde_json::from_str(line).expect("per-run row parses");
        assert!(
            row["fixture_id"].is_string(),
            "fixture_id should be present"
        );
        assert!(
            row["fixture_tool_sequence"].is_string(),
            "fixture_tool_sequence should be present"
        );
        assert!(
            row["transcript_events_path"].is_string(),
            "transcript_events_path should be present"
        );
        assert!(
            row["tool_sequence"].is_array(),
            "tool_sequence should be present"
        );
    }
    let readiness_raw =
        fs::read_to_string(output.join("local_readiness.json")).expect("readiness exists");
    let readiness: serde_json::Value =
        serde_json::from_str(&readiness_raw).expect("readiness parses as JSON");
    assert_eq!(readiness["schema_version"], 1);
    assert_eq!(
        readiness["outcomes"].as_array().map(Vec::len),
        Some(0),
        "mock-only runs should not produce local model readiness outcomes",
    );
    assert!(output
        .join("python-add__mock_mock__native/summary.json")
        .exists());
    assert!(output
        .join("python-add__mock_mock__native/transcript_events.jsonl")
        .exists());
    assert!(output
        .join("read-only-audit__mock_mock__text/summary.json")
        .exists());
    assert!(output
        .join("no-tool-diagnosis__mock_mock__native/transcript_events.jsonl")
        .exists());
    assert!(output
        .join(PARITY_DIRECTORY)
        .join("python-add__mock_mock")
        .join("parity.json")
        .exists());
    let parity_raw = fs::read_to_string(
        output
            .join(PARITY_DIRECTORY)
            .join("python-add__mock_mock")
            .join("parity.json"),
    )
    .expect("parity report exists");
    let parity: serde_json::Value = serde_json::from_str(&parity_raw).expect("parity parses");
    assert_eq!(parity["native_verdict"], "passed");
    assert_eq!(parity["text_verdict"], "passed");
    assert_eq!(parity["agreement"], true);
    assert_eq!(parity["divergence_class"], "both_pass");
    assert!(output.join(PARITY_OVERLAY_FILENAME).exists());
    assert!(output.join("summary.md").exists());
    assert!(output.join("followups.md").exists());
    let summary_md =
        fs::read_to_string(output.join("summary.md")).expect("summary markdown exists");
    assert!(summary_md.contains("Parity report — native vs text"));
}

#[test]
fn mock_matrix_resumes_completed_live_verify_cell_from_ledger() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("bench");
    let args = || EvalCodingAgentArgs {
        fixtures: vec!["python-add".to_string()],
        models: vec!["mock:mock".to_string()],
        tool_formats: vec!["native".to_string()],
        output: Some(output.clone()),
        env_files: Vec::new(),
        include_local: false,
        local_providers: Vec::new(),
        max_local_models: 2,
        keep_local_after_run: false,
        max_runs: None,
        max_iterations: 6,
        python: "python3".to_string(),
        fail_on_unauthorized: false,
        json: false,
        step_judge: None,
        step_judge_on_veto: None,
        step_judge_adversarial: false,
        structural_validator: None,
        run_label: String::new(),
        override_reason: None,
        baseline_comparison_against: None,
    };

    let first_exit = run_in_harn_runtime({
        let args = args();
        || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            harn_cli::commands::eval_coding_agent::run(args).await
        }
    });
    assert_eq!(first_exit, 0, "first live-verify run should pass");

    let run_summary_path = output
        .join("python-add__mock_mock__native")
        .join("summary.json");
    let mut run_summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_summary_path).expect("run summary"))
            .expect("summary json");
    run_summary["duration_ms"] = serde_json::json!(424242);
    fs::write(
        &run_summary_path,
        serde_json::to_string_pretty(&run_summary).expect("serialize summary"),
    )
    .expect("rewrite run summary");

    let second_exit = run_in_harn_runtime({
        let args = args();
        || async move {
            let _env_guard = env_lock::lock_env().lock().await;
            harn_cli::commands::eval_coding_agent::run(args).await
        }
    });
    assert_eq!(second_exit, 0, "ledger-resumed run should pass");

    let aggregate: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(output.join("summary.json")).expect("aggregate summary"),
    )
    .expect("aggregate json");
    assert_eq!(aggregate["runs"][0]["duration_ms"], 424242);
}

#[test]
fn mock_matrix_output_dir_can_be_workspace_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("bench");
    let args = EvalCodingAgentArgs {
        fixtures: vec!["python-add".to_string()],
        models: vec!["mock:mock".to_string()],
        tool_formats: vec!["native".to_string()],
        output: Some(output.clone()),
        env_files: Vec::new(),
        include_local: false,
        local_providers: Vec::new(),
        max_local_models: 2,
        keep_local_after_run: false,
        max_runs: None,
        max_iterations: 6,
        python: "python3".to_string(),
        fail_on_unauthorized: false,
        json: false,
        step_judge: None,
        step_judge_on_veto: None,
        step_judge_adversarial: false,
        structural_validator: None,
        run_label: String::new(),
        override_reason: None,
        baseline_comparison_against: None,
    };

    let exit = run_in_harn_runtime(|| async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_cli::commands::eval_coding_agent::run(args).await
    });
    assert_eq!(exit, 0, "mock coding-agent eval should pass");
    assert!(output
        .join("python-add__mock_mock__native/summary.json")
        .exists());
}

#[test]
fn read_only_audit_verifier_accepts_repeated_read_file_calls() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest_dir.join("tests/fixtures/read_only_audit_verifier.harn");
    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_vm::reset_thread_local_state();
        execute_run_with_sandbox_options(
            &fixture.to_string_lossy(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(manifest_dir),
        )
        .await
    });
    assert_eq!(
        outcome.exit_code, 0,
        "fixture failed\nstdout={}\nstderr={}",
        outcome.stdout, outcome.stderr
    );
    assert_eq!(
        outcome.stdout,
        "repeated_reads=true\nno_read=false\nedit_attempt=false\nparent_path=false\nchanged_readme=false\n"
    );
}

#[test]
fn coding_agent_suite_default_structural_validator_vetoes_phantom_completion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let fixture_path = tmp.path().join("llm-mocks.jsonl");
    fs::write(
        &fixture_path,
        r#"{"text":"I fixed it already.","model":"mock"}
{"text":"","tool_calls":[{"name":"read_file","args":{"path":"math_utils.py"}}],"model":"mock"}
{"text":"","tool_calls":[{"name":"replace_in_file","args":{"path":"math_utils.py","old_text":"def add(a, b):\n    return a - b\n","new_text":"def add(a, b):\n    return a + b\n"}}],"model":"mock"}
{"text":"","tool_calls":[{"name":"run_command","args":{"argv":["python3","-m","unittest","discover","-s","tests"],"capture":{"max_inline_bytes":4000}}}],"model":"mock"}
{"text":"Fixed add and verified the unittest suite passes.","model":"mock"}
"#,
    )
    .expect("write llm mock fixture");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let suite = manifest_dir.join("assets/evals/coding_agent_suite.harn");
    let output_dir = workspace.join("out");
    let output_dir_for_run = output_dir.clone();
    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_vm::reset_thread_local_state();
        execute_run_with_sandbox_options(
            &suite.to_string_lossy(),
            false,
            HashSet::new(),
            vec![
                "--fixture".to_string(),
                "python-add".to_string(),
                "--output-dir".to_string(),
                output_dir_for_run.display().to_string(),
                "--provider".to_string(),
                "mock".to_string(),
                "--model".to_string(),
                "mock".to_string(),
                "--tool-format".to_string(),
                "native".to_string(),
                "--max-iterations".to_string(),
                "6".to_string(),
                "--python".to_string(),
                "python3".to_string(),
            ],
            Vec::new(),
            CliLlmMockMode::Replay { fixture_path },
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(workspace),
        )
        .await
    });
    assert_eq!(
        outcome.exit_code, 0,
        "suite failed\nstdout={}\nstderr={}",
        outcome.stdout, outcome.stderr
    );
    let transcript = fs::read_to_string(output_dir.join("transcript_events.jsonl"))
        .expect("transcript events exist");
    assert!(
        transcript.contains("<runtime_feedback kind=\\\"structural_validator\\\">"),
        "default validator should inject runtime feedback; got:\n{transcript}"
    );
    assert!(
        transcript.contains("\\\"rule\\\":\\\"non_empty_when_writes_expected\\\""),
        "phantom completion should trip the write-expected rule; got:\n{transcript}"
    );
}

#[test]
fn coding_agent_suite_records_tool_format_override_transcript_event() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let suite = manifest_dir.join("assets/evals/coding_agent_suite.harn");
    let output_dir = workspace.join("out");
    let output_dir_for_run = output_dir.clone();
    let outcome: RunOutcome = run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_vm::reset_thread_local_state();
        execute_run_with_sandbox_options(
            &suite.to_string_lossy(),
            false,
            HashSet::new(),
            vec![
                "--fixture".to_string(),
                "read-only-audit".to_string(),
                "--output-dir".to_string(),
                output_dir_for_run.display().to_string(),
                "--provider".to_string(),
                "mock".to_string(),
                "--model".to_string(),
                "claude-opus-4-7".to_string(),
                "--tool-format".to_string(),
                "text".to_string(),
                "--override-reason".to_string(),
                "compare text trace".to_string(),
                "--python".to_string(),
                "python3".to_string(),
                "--seed-mock".to_string(),
            ],
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(workspace),
        )
        .await
    });
    assert_eq!(
        outcome.exit_code, 0,
        "suite failed\nstdout={}\nstderr={}",
        outcome.stdout, outcome.stderr
    );
    let transcript = fs::read_to_string(output_dir.join("transcript_events.jsonl"))
        .expect("transcript events exist");
    assert!(
        transcript.contains("\"kind\":\"tool_format_override\""),
        "transcript should record the override event; got:\n{transcript}"
    );
    assert!(
        transcript.contains("\"recommended_format\":\"native\""),
        "transcript should preserve the catalog recommendation; got:\n{transcript}"
    );
}
