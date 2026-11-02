use super::*;
use crate::orchestration::LlmUsageRecord;
use serde_json::json;

fn measured_run(duration: i64) -> RunRecord {
    RunRecord {
        id: format!("measured-{duration}"),
        usage: Some(LlmUsageRecord {
            total_duration_ms: duration,
            call_count: 1,
            cost_usd: Some(0.01),
            known_cost_usd: 0.01,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn session_latency_keeps_zero_distinct_from_missing_and_partial_measurements() {
    let zero = build_run_view(&measured_run(0));
    let positive = build_run_view(&measured_run(25));
    let absent = build_run_view(&RunRecord::default());
    let mut projected = measured_run(0);
    projected.metadata.insert(
        "projected_from".into(),
        json!({"not_recoverable_from_session": ["usage.total_duration_ms"]}),
    );
    let unknown = build_run_view(&projected);
    assert_eq!(zero.usage.total_duration_ms, Some(0));
    assert_eq!(positive.usage.total_duration_ms, Some(25));
    assert_eq!(absent.usage.total_duration_ms, None);
    assert_eq!(unknown.usage.total_duration_ms, None);

    for (views, expected) in [
        (vec![], None),
        (vec![zero.clone()], Some(0)),
        (vec![zero.clone(), positive.clone()], Some(25)),
        (vec![positive, unknown], None),
        (vec![zero, absent], None),
    ] {
        let session = build_session_view_from_run_views(views, SessionViewOptions::default());
        assert_eq!(session.usage.total_duration_ms, expected);
    }
}

#[test]
fn unreadable_projection_availability_does_not_make_zero_measured() {
    for projection in [
        Value::Null,
        json!({}),
        json!({"not_recoverable_from_session": "usage.total_duration_ms"}),
        json!({"not_recoverable_from_session": [null]}),
    ] {
        let mut run = measured_run(0);
        run.metadata.insert("projected_from".into(), projection);
        let view = build_run_view(&run);
        assert_eq!(view.usage.total_duration_ms, None);
        assert_eq!(view.usage.call_count, 1);
        assert_eq!(view.usage.cost_usd, Some(0.01));
    }
}
