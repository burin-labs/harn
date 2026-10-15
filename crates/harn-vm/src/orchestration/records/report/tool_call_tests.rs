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
            status: crate::orchestration::ToolCallRecordStatus::Completed,
            duration_ms: Some(9460),
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
    assert_eq!(report.tool_calls[0].call.tool_use_id, "call-flight");
    assert_eq!(report.tool_calls[0].call.tool_name, "find_flights");
    assert_eq!(report.tool_calls[0].call.args_hash, "sha256:args");
    assert_eq!(
        report.tool_calls[0].call.result,
        "offer off_test costs 275.94 USD"
    );
    assert_eq!(report.tool_calls[0].call.duration_ms, Some(9460));
    assert_eq!(report.tool_calls[0].call.iteration, 1);

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

/// A run record written before `status` existed carried only `is_rejected`.
/// It still reads, and a rejection survives as a status.
#[test]
fn a_legacy_tool_call_record_reads_its_rejection_as_a_status() {
    use crate::orchestration::ToolCallRecordStatus;
    let legacy: ToolCallRecord = serde_json::from_value(serde_json::json!({
        "tool_name": "run",
        "tool_use_id": "call-1",
        "is_rejected": true,
    }))
    .unwrap();
    assert_eq!(legacy.status, ToolCallRecordStatus::Rejected);
    let unsettled: ToolCallRecord =
        serde_json::from_value(serde_json::json!({"tool_name": "run", "is_rejected": false}))
            .unwrap();
    assert_eq!(unsettled.status, ToolCallRecordStatus::Open);
    let written = serde_json::to_value(&legacy).unwrap();
    assert_eq!(written["status"], "rejected");
    assert!(written.get("is_rejected").is_none());
}

/// The report carries the run record's own tool-call fact beside its agent.
#[test]
fn a_report_tool_call_is_the_record_with_its_agent() {
    let call = RunReportToolCall {
        agent_id: "run:root".to_string(),
        call: ToolCallRecord {
            tool_name: "edit".to_string(),
            tool_use_id: "call-1".to_string(),
            ..ToolCallRecord::default()
        },
    };
    let value = serde_json::to_value(&call).unwrap();
    assert_eq!(value["agent_id"], "run:root");
    assert_eq!(value["tool_use_id"], "call-1");
    assert_eq!(value["status"], "open");
    assert_eq!(
        serde_json::from_value::<RunReportToolCall>(value).unwrap(),
        call
    );
}
