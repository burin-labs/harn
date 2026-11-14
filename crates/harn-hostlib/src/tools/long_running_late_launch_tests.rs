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
    assert_late_launch_is_retired(false);
}

#[test]
fn a_parallel_background_launch_observes_session_cancellation_before_its_child_token() {
    assert_late_launch_is_retired(true);
}

fn assert_late_launch_is_retired(parallel_child: bool) {
    let _environment = crate::process::test_support::declare_inherited();
    let session = format!("late-launch-{}", uuid::Uuid::new_v4());
    let _cleanup = Cleanup(session.clone());
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
    let controller = mock.enqueue(MockProcessConfig::running());
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
        session,
    )
    .unwrap();
    assert!(
        controller.was_killed(),
        "late registration escaped the cancellation sweep"
    );
    let settled = register_result_notifier(&child.handle_id).unwrap();
    let _ = harn_clock::test_support::recv_within("late launch retires", &settled);
}
