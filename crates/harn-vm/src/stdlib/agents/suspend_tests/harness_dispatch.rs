use super::*;

#[tokio::test(flavor = "current_thread")]
async fn auto_resume_timeout_dispatches_synthetic_resume_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _guard = suspend_test_lock().await;
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
            let log = crate::event_log::install_memory_for_current_thread(128);
            drop(log);
            let clock = crate::triggers::test_util::clock::MockClock::at_wall_ms(0);
            let _clock_guard = crate::triggers::test_util::clock::install_override(clock.clone());
            let (worker_id, dir) = seed_test_worker("worker-auto-resume-timeout");

            let mut base_vm = Vm::new();
            crate::register_vm_stdlib(&mut base_vm);
            let harness_clock: std::sync::Arc<dyn harn_clock::Clock> = clock.clone();
            base_vm.set_harness(crate::Harness::with_clock(harness_clock));
            let suspended = suspend_agent_builtin(
                crate::vm::AsyncBuiltinCtx::for_test(base_vm),
                vec![
                    handle_value(&worker_id),
                    VmValue::String(arcstr::ArcStr::from("waiting for review or timeout")),
                    suspend_options(auto_resume_conditions_with_timeout(
                        "review.approved",
                        "resume_with_input",
                    )),
                ],
            )
            .await
            .expect("suspend with auto-resume timeout");
            let trigger_id = auto_resume_trigger_id(&suspended);

            tokio::task::yield_now().await;
            clock.advance_std(std::time::Duration::from_mins(1)).await;
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            let (status, task) = worker_status_and_task(&worker_id);
            // The worker has resumed; whether it is still mid-turn
            // ("running") or has already driven to completion depends on
            // task scheduling, so don't pin the exact intermediate state
            // (that was wall-clock-racy). The dispatch wiring below is the
            // real assertion.
            assert!(
                matches!(status.as_str(), "running" | "completed"),
                "timeout should have resumed the worker, got status: {status}"
            );
            assert!(
                task.contains("auto_resume.timeout"),
                "timeout resume input should name synthetic event, got: {task}"
            );
            let snapshot = crate::triggers::snapshot_trigger_bindings()
                .into_iter()
                .find(|binding| binding.id == trigger_id)
                .expect("auto-resume binding snapshot");
            assert_eq!(snapshot.state, crate::triggers::TriggerState::Terminated);

            teardown(&dir, &worker_id);
            crate::triggers::clear_trigger_registry();
            crate::stdlib::triggers_stdlib::reset_auto_resume_timeouts();
        })
        .await;
}
