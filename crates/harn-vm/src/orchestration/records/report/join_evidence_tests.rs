use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;

use crate::agent_events::{AgentEvent, AgentRunRef, DelegatedRunLineage};
use crate::event_log::{EventLog, LogEvent, SqliteEventLog};
use crate::orchestration::{save_run_record, RunChildRecord, RunRecord};

use super::{build_run_report, RunReportRequest};

#[tokio::test]
async fn report_projects_three_canonical_join_receipts() {
    let dir = std::env::temp_dir().join(format!("harn-run-report-joins-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&dir).unwrap();
    let parent_path = dir.join("parent.json");
    let child_paths = (1..=3)
        .map(|index| dir.join(format!("child-{index}.json")))
        .collect::<Vec<_>>();
    for (index, child_path) in child_paths.iter().enumerate() {
        let child_number = index + 1;
        save_run_record(
            &RunRecord {
                type_name: "workflow_run".to_string(),
                id: format!("child-{child_number}"),
                status: "completed".to_string(),
                started_at: "2026-08-02T10:00:01Z".to_string(),
                finished_at: Some("2026-08-02T10:00:03Z".to_string()),
                parent_run_id: Some("parent".to_string()),
                root_run_id: Some("parent".to_string()),
                metadata: BTreeMap::from([
                    (
                        "session_id".to_string(),
                        Value::String(format!("child-session-{child_number}")),
                    ),
                    (
                        "parent_session_id".to_string(),
                        Value::String("parent-session".to_string()),
                    ),
                ]),
                ..RunRecord::default()
            },
            Some(child_path.to_str().unwrap()),
        )
        .unwrap();
    }
    let child_runs = child_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let child_number = index + 1;
            RunChildRecord {
                worker_id: format!("worker-{child_number}"),
                worker_name: format!("child-{child_number}"),
                status: "completed".to_string(),
                started_at: "2026-08-02T10:00:01Z".to_string(),
                finished_at: Some("2026-08-02T10:00:03Z".to_string()),
                session_id: Some(format!("child-session-{child_number}")),
                parent_session_id: Some("parent-session".to_string()),
                run_id: Some(format!("child-{child_number}")),
                run_path: Some(path.to_string_lossy().into_owned()),
                ..RunChildRecord::default()
            }
        })
        .collect::<Vec<_>>();
    save_run_record(
        &RunRecord {
            type_name: "workflow_run".to_string(),
            id: "parent".to_string(),
            status: "completed".to_string(),
            started_at: "2026-08-02T10:00:00Z".to_string(),
            finished_at: Some("2026-08-02T10:00:04Z".to_string()),
            root_run_id: Some("parent".to_string()),
            child_runs,
            metadata: BTreeMap::from([(
                "session_id".to_string(),
                Value::String("parent-session".to_string()),
            )]),
            ..RunRecord::default()
        },
        Some(parent_path.to_str().unwrap()),
    )
    .unwrap();

    let events_path = dir.join("events.sqlite");
    let log = SqliteEventLog::open(events_path.clone(), 16).unwrap();
    let topic = crate::session_timeline::agent_events_topic("parent-session");
    for (index, lag_ms) in [10_i64, 75, 30].into_iter().enumerate() {
        let child_number = index + 1;
        let completed_at_ms = 1_000 + i64::try_from(index).unwrap() * 100;
        let event = AgentEvent::SubagentJoin {
            session_id: "parent-session".to_string(),
            lineage: DelegatedRunLineage {
                parent: AgentRunRef {
                    session_id: "parent-session".to_string(),
                    run_id: "parent".to_string(),
                },
                child: AgentRunRef {
                    session_id: format!("child-session-{child_number}"),
                    run_id: format!("child-{child_number}"),
                },
            },
            worker_id: format!("worker-{child_number}"),
            completed_at_ms,
            joined_at_ms: completed_at_ms + lag_ms,
        };
        log.append(
            &topic,
            LogEvent::new(
                "subagent_join",
                serde_json::json!({
                    "session_id": "parent-session",
                    "event": serde_json::to_value(event).unwrap(),
                }),
            ),
        )
        .await
        .unwrap();
    }
    log.flush().await.unwrap();

    let report = build_run_report(RunReportRequest {
        run_record_path: parent_path,
        events_db: Some(events_path),
        allowed_roots: vec![dir.clone()],
        source_root: Some(dir.clone()),
    })
    .await
    .unwrap();

    assert_eq!(report.coordination.spawned, 3);
    assert_eq!(report.coordination.terminal, 3);
    assert_eq!(report.coordination.unjoined, Some(0));
    assert_eq!(report.coordination.observed_join_ms, Some(75));
    assert_eq!(report.coordination.observed_wait_ms, None);
    assert!(!report
        .checks
        .iter()
        .any(|check| check.code == "subagent_join_missing"));
    let timing_check = report
        .checks
        .iter()
        .find(|check| check.code == "coordination_timing_unavailable")
        .unwrap();
    assert!(timing_check
        .message
        .contains("no canonical wait-start event"));
    assert!(report
        .timelines
        .iter()
        .find(|timeline| timeline.query.run_id.as_deref() == Some("parent"))
        .is_some_and(|timeline| {
            !timeline.coverage.truncated
                && timeline
                    .nodes
                    .iter()
                    .filter(|node| node.name == "subagent_join")
                    .count()
                    == 3
        }));

    drop(log);
    fs::remove_dir_all(dir).unwrap();
}
