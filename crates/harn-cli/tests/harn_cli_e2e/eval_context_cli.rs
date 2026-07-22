//! In-process coverage for `harn eval context`.

use std::fs;
use std::path::Path;

use harn_cli::cli::EvalContextArgs;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under crates/harn-cli")
}

#[tokio::test]
async fn context_smoke_manifest_writes_stable_report_artifacts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("context-eval");
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let args = EvalContextArgs {
        manifest,
        output: Some(output.clone()),
        json: false,
    };

    let exit = harn_cli::commands::eval_context::run(args).await;
    assert_eq!(exit, 0, "context eval smoke fixture should pass");

    let summary_raw = fs::read_to_string(output.join("summary.json")).expect("summary exists");
    let summary: serde_json::Value =
        serde_json::from_str(&summary_raw).expect("summary parses as JSON");
    assert_eq!(summary["_type"], "harn.context_eval.report.v1");
    assert_eq!(summary["schema_version"], 1);
    assert_eq!(summary["pass"], true);
    assert_eq!(summary["total_runs"], 9);
    assert_eq!(summary["passed_runs"], 9);
    assert_eq!(summary["failed_runs"], 0);
    let runs = summary["runs"].as_array().expect("runs are present");
    assert_eq!(runs.len(), 9);
    assert!(
        runs.iter()
            .all(|run| run["cache"]["deterministic_order"] == true),
        "reports must be stable enough for hosted ingestion and diffing",
    );

    let per_run = fs::read_to_string(output.join("per_run.jsonl")).expect("per-run JSONL exists");
    assert_eq!(per_run.lines().count(), 9);
    assert!(output.join("summary.md").exists());
}
