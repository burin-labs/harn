use super::*;
use crate::orchestration::{save_run_record, ToolCallRecord};
use std::fs;

#[tokio::test]
async fn report_retains_recorded_tool_evidence_without_a_sidecar() {
    let dir = std::env::temp_dir().join(format!(
        "harn-run-report-tool-evidence-{}",
        uuid::Uuid::now_v7()
    ));
    fs::create_dir_all(&dir).unwrap();
    let run_path = dir.join("run.json");
    let run = RunRecord {
        type_name: "workflow_run".to_string(),
        id: "root".to_string(),
        status: "completed".to_string(),
        tool_recordings: vec![ToolCallRecord {
            tool_name: "find_flights".to_string(),
            tool_use_id: "call-flight".to_string(),
            args_hash: "sha256:args".to_string(),
            result: "offer off_test costs 275.94 USD".to_string(),
            is_rejected: false,
            duration_ms: 9460,
            iteration: 1,
            timestamp: "2026-08-12T01:42:15Z".to_string(),
        }],
        ..RunRecord::default()
    };
    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();

    let report = build_run_report(RunReportRequest {
        run_record_path: run_path,
        allowed_roots: vec![dir.clone()],
        source_root: Some(dir.clone()),
        ..RunReportRequest::default()
    })
    .await
    .unwrap();

    assert_eq!(report.tool_calls.len(), 1);
    assert_eq!(report.tool_calls[0].agent_id, "run:root");
    assert_eq!(report.tool_calls[0].call_id, "call-flight");
    assert_eq!(report.tool_calls[0].tool_name, "find_flights");
    assert_eq!(report.tool_calls[0].args_hash, "sha256:args");
    assert_eq!(
        report.tool_calls[0].result,
        "offer off_test costs 275.94 USD"
    );
    assert_eq!(report.tool_calls[0].duration_ms, 9460);
    assert_eq!(report.tool_calls[0].iteration, 1);

    let value = serde_json::to_value(&report).unwrap();
    let (evidence, projection) =
        crate::orchestration::run_review::build_evidence_projection_for_test(&value);
    assert_eq!(
        evidence["tool_calls"][0]["result"],
        "offer off_test costs 275.94 USD"
    );
    assert!(projection.omissions.is_empty());

    fs::remove_dir_all(dir).unwrap();
}
