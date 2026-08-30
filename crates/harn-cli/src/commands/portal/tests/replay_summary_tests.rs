use super::build_replay_summary;

#[test]
fn build_replay_summary_reads_fixture_metadata() {
    let fixture = harn_vm::orchestration::ReplayFixture {
        id: "fixture-1".to_string(),
        source_run_id: "run-1".to_string(),
        created_at: "2026-04-04T00:00:00Z".to_string(),
        expected_status: "completed".to_string(),
        stage_assertions: vec![harn_vm::orchestration::ReplayStageAssertion {
            node_id: "plan".to_string(),
            expected_status: "completed".to_string(),
            expected_outcome: "success".to_string(),
            expected_branch: Some("true".to_string()),
            required_artifact_kinds: vec!["notes".to_string()],
            visible_text_contains: Some("done".to_string()),
        }],
        ..Default::default()
    };

    let summary = build_replay_summary(Some(&fixture)).unwrap();
    assert_eq!(summary.fixture_id, "fixture-1");
    assert_eq!(summary.stage_assertions.len(), 1);
    assert_eq!(summary.stage_assertions[0].node_id, "plan");
}
