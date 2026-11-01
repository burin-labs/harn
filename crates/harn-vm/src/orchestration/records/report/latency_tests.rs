use super::*;
use crate::orchestration::{ExecutionEvidenceRecord, LlmUsageRecord, RunTraceSpanRecord};
use std::fs;

#[tokio::test]
async fn report_preserves_unknown_latency_and_measured_zero() {
    let root = tempfile::tempdir().unwrap();
    let run_path = root.path().join("run.json");
    let run = RunRecord {
        type_name: "run".into(),
        id: "latency".into(),
        workflow_id: "agent-session".into(),
        status: "completed".into(),
        usage: Some(LlmUsageRecord {
            call_count: 3,
            total_duration_ms: 0,
            cost_usd: Some(0.02),
            known_cost_usd: 0.02,
            ..Default::default()
        }),
        metadata: BTreeMap::from([(
            "projected_from".into(),
            serde_json::json!({"not_recoverable_from_session": ["usage.total_duration_ms"]}),
        )]),
        evidence: ExecutionEvidenceRecord {
            trace_spans: [
                ("unknown", 0, Some(false)),
                ("zero", 0, None),
                ("measured", 25, Some(true)),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (name, duration, available))| RunTraceSpanRecord {
                trace_id: "latency".into(),
                span_id: index as u64 + 1,
                kind: "llm_call".into(),
                name: name.into(),
                duration_ms: duration,
                metadata: available
                    .map(|value| {
                        BTreeMap::from([("duration_available".into(), Value::Bool(value))])
                    })
                    .unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
            ..Default::default()
        },
        ..Default::default()
    };
    fs::write(&run_path, serde_json::to_vec(&run).unwrap()).unwrap();
    let report = build_run_report(RunReportRequest {
        run_record_path: run_path,
        allowed_roots: vec![root.path().into()],
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.llm_calls.len(), 3);
    let calls = report
        .llm_calls
        .iter()
        .map(|call| call.duration_ms)
        .collect::<Vec<_>>();
    assert_eq!(calls, vec![None, Some(0), Some(25)]);
    assert_eq!(report.agents[0].usage.total_duration_ms, None);
    assert_eq!(report.agents[0].usage.cost_usd, Some(0.02));
    assert_eq!(report.agents[0].usage.call_count, 3);
    let json = serde_json::to_value(report).unwrap();
    assert!(json["llm_calls"][0]["duration_ms"].is_null());
    assert_eq!(json["llm_calls"][1]["duration_ms"], 0);
    assert!(json["agents"][0]["usage"]["total_duration_ms"].is_null());
}
