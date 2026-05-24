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
    assert_eq!(summary["schema_version"], 2);
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
    assert!(output.join("summary.md").exists());
    assert!(output.join("followups.md").exists());
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
        "transcript should record the override event; got:\n{}",
        transcript
    );
    assert!(
        transcript.contains("\"recommended_format\":\"native\""),
        "transcript should preserve the catalog recommendation; got:\n{}",
        transcript
    );
}
