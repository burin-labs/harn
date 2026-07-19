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
    TerminalRoute,
) {
    // Mark the record whose future produced the returned result as the terminal
    // attempt (Succeeded on ok, Failed on err) so the caller attributes the
    // terminal route from the race OUTCOME, never from the outer/base link.
    fn finalize_terminal(
        record: &mut RoutingAttempt,
        res: &Result<crate::llm::api::LlmResult, VmError>,
    ) {
        match res {
            Ok(v) => {
                record.status = AttemptStatus::Succeeded;
                record.cost_usd = Some(project_link_cost_usd(v));
                record.input_tokens = Some(v.input_tokens);
                record.output_tokens = Some(v.output_tokens);
            }
            Err(_) => {
                record.status = AttemptStatus::Failed;
            }
        }
    }
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
            finalize_terminal(&mut record, &res);
            (res, vec![record], TerminalRoute::Attempt(primary_attempt_no))
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
                    finalize_terminal(&mut primary_record, &res);
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
                    (
                        res,
                        vec![primary_record, backup_record],
                        TerminalRoute::Attempt(primary_attempt_no),
                    )
                }
                backup = &mut backup_future => {
                    let (res, elapsed) = backup;
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        elapsed,
                    );
                    finalize_terminal(&mut backup_record, &res);
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
                    (
                        res,
                        vec![primary_record, backup_record],
                        TerminalRoute::Attempt(backup_attempt_no),
                    )
                }
                _ = crate::clock_mock::sleep(Duration::from_millis(primary_deadline)) => {
                    // Both racers hit the deadline: no single route is responsible.
                    // Mark both Failed and report a Composite terminal — the caller
                    // must NOT fabricate a single provider/model for this case.
                    let mut primary_record = pending_attempt_record(
                        primary_attempt_no,
                        &primary_link,
                        &primary_label,
                        Duration::from_millis(primary_deadline),
                    );
                    primary_record.status = AttemptStatus::Failed;
                    let mut backup_record = pending_attempt_record(
                        backup_attempt_no,
                        &backup_link_clone,
                        &backup_label,
                        Duration::from_millis(primary_deadline),
                    );
                    backup_record.status = AttemptStatus::Failed;
                    (
                        Err(runtime_error(
                            "routing_policy: race exhausted both primary and backup attempts".to_string(),
                        )),
                        vec![primary_record, backup_record],
                        TerminalRoute::Composite,
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod race_terminal_tests {
    //! `run_race` must attribute the terminal route from the race OUTCOME, not
    //! from configured order. Records are always returned `[primary, backup]`,
    //! so a positional read (`attempts.last()`) misnames the terminal route
    //! whenever the primary produced the result. These tests pin the marker for
    //! each branch. Determinism comes from a `start_paused` runtime: the fake
    //! provider's `Stalled` turns and `run_race`'s own timers share one virtual
    //! clock, so timer ordering is fixed rather than wall-clock racy.

    use super::*;
    use crate::llm::api::options::base_opts;
    use crate::llm::fake::{install_fake_llm_script, FakeLlmScript, FakeLlmTurn};
    use crate::value::ErrorCategory;
    use std::time::Duration;

    fn link(model: &str) -> ChainLink {
        ChainLink {
            provider: "fake".to_string(),
            model: model.to_string(),
            timeout_ms: None,
            label: Some(model.to_string()),
            region: None,
            overrides: None,
        }
    }

    fn opts(model: &str) -> LlmCallOptions {
        let mut opts = base_opts("fake");
        opts.model = model.to_string();
        opts
    }

    fn policy() -> std::sync::Arc<RoutingPolicyConfig> {
        crate::llm::routing::linear_failover_policy(
            "test".to_string(),
            vec![link("fake-primary"), link("fake-backup")],
            false,
        )
    }

    fn paused_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("paused runtime")
    }

    async fn drive(
        race_after_ms: u64,
        primary_timeout_ms: u64,
    ) -> (
        Result<crate::llm::api::LlmResult, VmError>,
        Vec<RoutingAttempt>,
        TerminalRoute,
    ) {
        let policy = policy();
        let primary_link = link("fake-primary");
        run_race(
            "test",
            &policy,
            0,
            &primary_link,
            "fake-primary",
            &opts("fake-primary"),
            None,
            race_after_ms,
            primary_timeout_ms,
            "fake-backup".to_string(),
            opts("fake-backup"),
        )
        .await
    }

    /// F1 (race arm): the backup resolves first with an error while the primary
    /// is still in flight. The terminal route must name the BACKUP (attempt 2),
    /// not the primary — even though records stay in `[primary, backup]` order.
    #[test]
    fn backup_wins_race_marks_terminal_as_backup() {
        let runtime = paused_runtime();
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Stalled(Duration::from_mins(1)))
                .push(FakeLlmTurn::error(
                    ErrorCategory::CircuitOpen,
                    "backup failed first",
                )),
        );

        let (result, records, terminal) = runtime.block_on(drive(10, 50));

        assert!(result.is_err(), "backup errored, so the race result is Err");
        assert_eq!(
            terminal,
            TerminalRoute::Attempt(2),
            "terminal route names the backup attempt"
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].model, "fake-primary");
        assert!(matches!(records[0].status, AttemptStatus::RaceLost));
        assert_eq!(records[1].model, "fake-backup");
        assert!(matches!(records[1].status, AttemptStatus::Failed));
    }

    /// F2 (producer): both racers hit the primary deadline. No single route is
    /// responsible, so the terminal marker is `Composite` and both records fail.
    #[test]
    fn dual_deadline_marks_composite() {
        let runtime = paused_runtime();
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Stalled(Duration::from_mins(1)))
                .push(FakeLlmTurn::Stalled(Duration::from_mins(1))),
        );

        let (result, records, terminal) = runtime.block_on(drive(10, 20));

        assert!(result.is_err(), "an exhausted race returns Err");
        assert_eq!(terminal, TerminalRoute::Composite);
        assert_eq!(records.len(), 2);
        assert!(matches!(records[0].status, AttemptStatus::Failed));
        assert!(matches!(records[1].status, AttemptStatus::Failed));
    }

    /// The common primary-terminal case: the primary resolves (here with an
    /// error) before the race even starts, so the marker names attempt 1.
    #[test]
    fn primary_terminal_marks_attempt_one() {
        let runtime = paused_runtime();
        let _guard = install_fake_llm_script(FakeLlmScript::new().push(FakeLlmTurn::error(
            ErrorCategory::CircuitOpen,
            "primary failed",
        )));

        let (result, records, terminal) = runtime.block_on(drive(10, 50));

        assert!(result.is_err());
        assert_eq!(terminal, TerminalRoute::Attempt(1));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "fake-primary");
        assert!(matches!(records[0].status, AttemptStatus::Failed));
    }
}
