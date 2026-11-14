use super::*;
use crate::process::call_scope::CallProcessScope;
use crate::process::{MockProcessConfig, MockSpawner, ProcessError, ProcessSpawner};

struct CancelDuringSpawn {
    mock: MockSpawner,
    cancel: Arc<AtomicBool>,
    session: String,
}

impl ProcessSpawner for CancelDuringSpawn {
    fn spawn(&self, spec: SpawnSpec) -> Result<Box<dyn ProcessHandle>, ProcessError> {
        let child = self.mock.spawn(spec)?;
        self.cancel.store(true, Ordering::SeqCst);
        cancel_session_handles(&self.session);
        Ok(child)
    }
}

struct Cleanup(String);

impl Drop for Cleanup {
    fn drop(&mut self) {
        cancel_session_handles(&self.0);
    }
}

#[test]
fn a_background_launch_that_finishes_after_session_cancellation_is_retired() {
    assert_late_launch_is_retired(false, false);
}

#[test]
fn a_parallel_background_launch_observes_session_cancellation_before_its_child_token() {
    assert_late_launch_is_retired(true, false);
}

#[test]
fn a_measured_zero_survivor_report_confirms_late_launch_cleanup() {
    assert_late_launch_is_retired(false, true);
}

struct ResetEventSinks;

impl Drop for ResetEventSinks {
    fn drop(&mut self) {
        harn_vm::events::reset_event_sinks();
    }
}

fn assert_late_launch_is_retired(parallel_child: bool, confirmed: bool) {
    let _environment = crate::process::test_support::declare_inherited();
    let session = format!("late-launch-{}", uuid::Uuid::new_v4());
    let _cleanup = Cleanup(session.clone());
    let logs = std::rc::Rc::new(harn_vm::events::CollectorSink::new());
    let _reset = ResetEventSinks;
    harn_vm::events::add_event_sink(logs.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let scope = if parallel_child {
        CallProcessScope::new(
            Arc::new(AtomicBool::new(false)),
            Some(Arc::new(AtomicBool::new(false))),
        )
        .with_execution_cancel(Some(Arc::clone(&cancel)))
    } else {
        CallProcessScope::new(Arc::clone(&cancel), None)
    };
    let _call = HostCallContext::new(None).with_process_scope(scope).enter();
    let mock = MockSpawner::new();
    let mut config = MockProcessConfig::running();
    if confirmed {
        let mut report = process_handle::ProcessCleanupReport::for_signal(Some(42), 9);
        report.census = harn_vm::op_interrupt::ProcessCleanupCensus {
            control_read: true,
            scanned_processes: 1,
            unreadable_processes: 0,
        };
        config.cleanup_report = Some(report);
    }
    let controller = mock.enqueue(config);
    let _spawner = process_handle::install_spawner(Arc::new(CancelDuringSpawn {
        mock,
        cancel,
        session: session.clone(),
    }));
    let child = spawn_long_running(
        "run",
        "held-child".to_string(),
        vec![],
        Some(std::env::temp_dir()),
        BTreeMap::new(),
        session.clone(),
    )
    .unwrap();
    assert!(
        controller.was_killed(),
        "late registration escaped the cancellation sweep"
    );
    let warnings = logs
        .logs
        .borrow()
        .iter()
        .filter(|event| {
            event.category == "process.session_cleanup_unconfirmed"
                && event.message.contains(&session)
                && event.level == harn_vm::events::EventLevel::Warn
        })
        .count();
    assert_eq!(
        warnings,
        usize::from(!confirmed),
        "cleanup qualification must be reported exactly once"
    );
    let settled = register_result_notifier(&child.handle_id).unwrap();
    let _ = harn_clock::test_support::recv_within("late launch retires", &settled);
}
