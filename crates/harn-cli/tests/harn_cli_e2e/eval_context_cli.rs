//! In-process coverage for `harn eval context`.

use std::fs;

use crate::test_util::cli::workspace_root;
use harn_cli::cli::EvalContextArgs;

#[tokio::test]
async fn context_smoke_manifest_writes_stable_report_artifacts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("context-eval");
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let args = EvalContextArgs {
        manifest,
        output: Some(output.clone()),
        json: false,
        report_version: 2,
    };

    let exit = harn_cli::commands::eval_context::run(args).await;
    assert_eq!(exit, 0, "context eval smoke fixture should pass");

    let summary_raw = fs::read_to_string(output.join("summary.json")).expect("summary exists");
    let summary: serde_json::Value =
        serde_json::from_str(&summary_raw).expect("summary parses as JSON");
    assert_eq!(summary["_type"], "harn.context_eval.report.v2");
    assert_eq!(summary["schema_version"], 2);
    assert_eq!(summary["pass"], true);
    assert_eq!(summary["total_runs"], 9);
    assert_eq!(summary["passed_runs"], 9);
    assert_eq!(summary["failed_runs"], 0);
    assert_eq!(summary["aggregate"]["cost_measured_runs"], 9);
    assert_eq!(summary["aggregate"]["cost_unmeasured_runs"], 0);
    assert_eq!(summary["aggregate"]["total_cost_usd"], 0.003);
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

#[tokio::test]
async fn context_cost_coverage_survives_cli_artifacts_and_v1_projection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source =
        fs::read_to_string(workspace_root().join("examples/evals/context-engineering-smoke.json"))
            .expect("smoke manifest");
    let mut manifest: serde_json::Value = serde_json::from_str(&source).expect("manifest JSON");
    manifest["tasks"][0]["observed"]["cost_usd"] = serde_json::json!(0.0);
    manifest["tasks"][1]["observed"]
        .as_object_mut()
        .unwrap()
        .remove("cost_usd");
    manifest["tasks"][1]["observed"]["known_cost_usd"] = serde_json::json!(0.0002);
    let manifest_path = tmp.path().join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    for version in [2, 1] {
        let output = tmp.path().join(format!("report-{version}"));
        let exit = harn_cli::commands::eval_context::run(EvalContextArgs {
            manifest: manifest_path.clone(),
            output: Some(output.clone()),
            json: false,
            report_version: version,
        })
        .await;
        assert_eq!(exit, 0);
        let raw_summary = fs::read_to_string(output.join("summary.json")).unwrap();
        let summary: serde_json::Value = serde_json::from_str(&raw_summary).unwrap();
        let schema_path = workspace_root().join(format!(
            "spec/schemas/context-eval-report.v{version}.schema.json"
        ));
        let schema: serde_json::Value =
            serde_json::from_slice(&fs::read(schema_path).unwrap()).unwrap();
        jsonschema::draft202012::meta::validate(&schema).expect("valid report schema");
        let validator = jsonschema::draft202012::new(&schema).expect("compile report schema");
        assert!(
            validator.is_valid(&summary),
            "CLI report must satisfy its published schema"
        );
        let mut wrong_version = summary.clone();
        wrong_version["schema_version"] = serde_json::json!(3);
        assert!(
            !validator.is_valid(&wrong_version),
            "schema must reject a different version"
        );
        let aggregate = &summary["aggregate"];
        assert_eq!(summary["schema_version"], version);
        assert_eq!(summary["runs"][0]["cost_usd"], 0.0);
        let lines = fs::read_to_string(output.join("per_run.jsonl")).unwrap();
        let runs: Vec<serde_json::Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(runs.len(), 9);
        assert_eq!(runs[3]["cost_usd"], summary["runs"][3]["cost_usd"]);
        let markdown = fs::read_to_string(output.join("summary.md")).unwrap();
        assert!(markdown.contains("cost USD: unavailable"));
        assert!(markdown.contains("known subtotal 0.001500"));
        assert!(markdown.contains("6 measured, 3 unmeasured"));
        if version == 2 {
            assert_eq!(aggregate["cost_measured_runs"], 6);
            assert_eq!(aggregate["cost_unmeasured_runs"], 3);
            assert_eq!(aggregate["total_known_cost_usd"], 0.0015);
            assert!(aggregate["total_cost_usd"].is_null());
            assert!(runs[3]["cost_usd"].is_null());
            assert_eq!(runs[3]["known_cost_usd"], 0.0002);
            let mut missing_coverage = summary.clone();
            missing_coverage["aggregate"]
                .as_object_mut()
                .unwrap()
                .remove("cost_unmeasured_runs");
            assert!(
                !validator.is_valid(&missing_coverage),
                "coverage cannot be omitted"
            );
        } else {
            assert_eq!(aggregate["total_cost_usd"], 0.0009);
            assert_eq!(runs[3]["cost_usd"], 0.0);
            assert!(raw_summary.starts_with("{\n  \"_type\":"));
            assert!(lines.starts_with("{\"run_id\":"));
        }
    }
}

#[tokio::test]
async fn context_report_rejects_unsupported_version_before_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("report");
    let result = harn_cli::commands::eval_context::run(EvalContextArgs {
        manifest: workspace_root().join("examples/evals/context-engineering-smoke.json"),
        output: Some(output.clone()),
        json: false,
        report_version: 3,
    })
    .await;
    assert_eq!(result, 1);
    assert!(!output.exists());
}

#[tokio::test]
async fn context_report_rejects_negative_cost_before_writing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source =
        fs::read_to_string(workspace_root().join("examples/evals/context-engineering-smoke.json"))
            .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&source).unwrap();
    manifest["tasks"][0]["observed"]["cost_usd"] = serde_json::json!(-0.01);
    let manifest_path = tmp.path().join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = tmp.path().join("report");
    let result = harn_cli::commands::eval_context::run(EvalContextArgs {
        manifest: manifest_path,
        output: Some(output.clone()),
        json: false,
        report_version: 2,
    })
    .await;
    assert_eq!(result, 1);
    assert!(!output.exists());
}
