use std::sync::Arc;

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_race(
    dispatch: &str,
    policy: &RoutingPolicyConfig,
    attempts_used: usize,
    link: &ChainLink,
    link_label: &str,
    opts: &LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    race_after_ms: u64,
    primary_timeout_ms: u64,
    backup_label: String,
    backup_opts: LlmCallOptions,
) -> (
    Result<crate::llm::api::LlmResult, VmError>,
    Vec<RoutingAttempt>,
) {
    let primary_start = std::time::Instant::now();
    let primary_attempt_no = attempts_used + 1;
    let backup_attempt_no = attempts_used + 2;

    let primary_link = link.clone();
    let primary_label = link_label.to_string();
    let primary_opts = opts.clone();

    let mut primary_future = Box::pin(async move {
        let (res, _) = execute_link(&primary_opts, bridge, None).await;
        (res, primary_start.elapsed())
    });

    tokio::select! {
        biased;
        primary = &mut primary_future => {
            let (res, elapsed) = primary;
            let mut record = pending_attempt_record(
                primary_attempt_no,
                &primary_link,
                &primary_label,
                elapsed,
            );
            if let Ok(ref v) = res {
                record.status = AttemptStatus::Succeeded;
                record.cost_usd = Some(project_link_cost_usd(v));
                record.input_tokens = Some(v.input_tokens);
                record.output_tokens = Some(v.output_tokens);
            }
            (res, vec![record])
        }
        _ = crate::clock_mock::sleep(Duration::from_millis(race_after_ms)) => {
            let mut race_meta = serde_json::Map::new();
            race_meta.insert("policy".to_string(), json!(policy.label.clone()));
            race_meta.insert("race_after_ms".to_string(), json!(race_after_ms));
            race_meta.insert("primary_label".to_string(), json!(primary_label.clone()));
            race_meta.insert("backup_label".to_string(), json!(backup_label.clone()));
            emit_routing_event(dispatch, "race_started", race_meta);

            let backup_start = std::time::Instant::now();
            let backup_link_clone = ChainLink {
                provider: backup_opts.provider.clone(),
                model: backup_opts.model.clone(),
                timeout_ms: link.timeout_ms,
                label: Some(backup_label.clone()),
                region: backup_opts.region.clone(),
                overrides: None,
            };
            let mut backup_future = Box::pin({
                let backup_opts = backup_opts.clone();
                async move {
                    let (res, _) = execute_link(&backup_opts, bridge, None).await;
                    (res, backup_start.elapsed())
                }
            });

            let primary_deadline = primary_timeout_ms.saturating_add(race_after_ms);

            tokio::select! {
                biased;
                primary = &mut primary_future => {
                    let (res, elapsed) = primary;
                    let mut primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        elapsed,
                    );
                    if let Ok(ref v) = res {
                        primary_record.status = AttemptStatus::Succeeded;
                        primary_record.cost_usd = Some(project_link_cost_usd(v));
                        primary_record.input_tokens = Some(v.input_tokens);
                        primary_record.output_tokens = Some(v.output_tokens);
                    }
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        backup_start.elapsed(),
                    );
                    backup_record.status = AttemptStatus::RaceLost;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("winner".to_string(), json!(primary_label));
                    meta.insert("loser".to_string(), json!(backup_label));
                    emit_routing_event(dispatch, "race_won", meta.clone());
                    let mut lost_meta = meta;
                    lost_meta.insert("reason".to_string(), json!("primary_finished_first"));
                    emit_routing_event(dispatch, "race_lost", lost_meta);
                    (res, vec![primary_record, backup_record])
                }
                backup = &mut backup_future => {
                    let (res, elapsed) = backup;
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        elapsed,
                    );
                    if let Ok(ref v) = res {
                        backup_record.status = AttemptStatus::Succeeded;
                        backup_record.cost_usd = Some(project_link_cost_usd(v));
                        backup_record.input_tokens = Some(v.input_tokens);
                        backup_record.output_tokens = Some(v.output_tokens);
                    }
                    let mut primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        primary_start.elapsed(),
                    );
                    primary_record.status = AttemptStatus::RaceLost;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("winner".to_string(), json!(backup_label));
                    meta.insert("loser".to_string(), json!(primary_label));
                    emit_routing_event(dispatch, "race_won", meta.clone());
                    let mut lost_meta = meta;
                    lost_meta.insert("reason".to_string(), json!("backup_finished_first"));
                    emit_routing_event(dispatch, "race_lost", lost_meta);
                    (res, vec![primary_record, backup_record])
                }
                _ = crate::clock_mock::sleep(Duration::from_millis(primary_deadline)) => {
                    let primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        Duration::from_millis(primary_deadline),
                    );
                    let backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        Duration::from_millis(primary_deadline),
                    );
                    (
                        Err(runtime_error(
                            "routing_policy: race exhausted both primary and backup attempts".to_string(),
                        )),
                        vec![primary_record, backup_record],
                    )
                }
            }
        }
    }
}
