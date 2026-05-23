#![recursion_limit = "256"]

//! In-process coverage for `harn eval coding-agent`.

use std::fs;
use std::thread;

use harn_cli::cli::EvalCodingAgentArgs;
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
    };

    let exit = run_in_harn_runtime(|| async move {
        let _env_guard = env_lock::lock_env().lock().await;
        harn_cli::commands::eval_coding_agent::run(args).await
    });
    assert_eq!(exit, 0, "mock coding-agent eval should pass");

    let summary_raw = fs::read_to_string(output.join("summary.json")).expect("summary exists");
    let summary: serde_json::Value =
        serde_json::from_str(&summary_raw).expect("summary parses as JSON");
    assert_eq!(summary["total_runs"], 2);
    assert_eq!(summary["passed_runs"], 2);
    assert_eq!(summary["skipped_runs"], 0);
    assert_eq!(
        summary["env_keys_loaded"].as_array().map(Vec::len),
        Some(0),
        "no env-file values should be serialized in the default smoke run",
    );

    let per_run = fs::read_to_string(output.join("per_run.jsonl")).expect("per-run JSONL exists");
    assert_eq!(per_run.lines().count(), 2);
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
    assert!(output.join("mock_mock__native/summary.json").exists());
    assert!(output
        .join("mock_mock__native/transcript_events.jsonl")
        .exists());
    assert!(output.join("mock_mock__text/summary.json").exists());
    assert!(output
        .join("mock_mock__text/transcript_events.jsonl")
        .exists());
    assert!(output.join("summary.md").exists());
    assert!(output.join("followups.md").exists());
}
