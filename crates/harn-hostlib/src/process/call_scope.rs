use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use harn_vm::op_interrupt::{
    ActiveProcessCleanupGuard, ActiveProcessCleanupScope, ProcessCleanupReport,
};

use super::ProcessHandle;

/// One foreground host call's launch and retirement boundary.
#[derive(Clone, Debug)]
pub(crate) struct CallProcessScope {
    cancel: Arc<AtomicBool>,
    parent_cancel: Option<Arc<AtomicBool>>,
    launching: Arc<Mutex<()>>,
}

impl CallProcessScope {
    pub(crate) fn new(cancel: Arc<AtomicBool>, parent_cancel: Option<Arc<AtomicBool>>) -> Self {
        Self {
            cancel,
            parent_cancel,
            launching: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn enter_launch(&self) -> Option<MutexGuard<'_, ()>> {
        let permit = self
            .launching
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (!self.cancel_requested()).then_some(permit)
    }

    pub(crate) fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
            || self
                .parent_cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::SeqCst))
    }

    pub(crate) fn register(&self, child: &dyn ProcessHandle) -> Option<ActiveProcessCleanupGuard> {
        child.register_scope_cleanup(Arc::clone(&self.cancel))
    }

    pub(crate) fn cancel_and_drain(&self) -> Option<ProcessCleanupReport> {
        self.cancel.store(true, Ordering::SeqCst);
        // An admitted launch must enroll before retirement takes its census.
        let _permit = self
            .launching
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let report =
            harn_vm::op_interrupt::terminate_active_process_cleanups_for_cancel_token(&self.cancel);
        if let Some(report) = &report {
            if report.survivor_count() != Some(0) {
                harn_vm::events::log_status(&format!(
                    "foreground command cleanup remains unqualified: root_pid={:?} observed_children={} survivors={:?}",
                    report.root_pid,
                    report.children.len(),
                    report.survivor_count(),
                ));
            }
        }
        report
    }
}

impl ActiveProcessCleanupScope for CallProcessScope {
    fn signal(&self, signal: i32) -> ProcessCleanupReport {
        self.cancel.store(true, Ordering::SeqCst);
        let _permit = self
            .launching
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        harn_vm::op_interrupt::signal_active_process_cleanups_for_cancel_token(signal, &self.cancel)
    }

    fn terminate(&self) -> Option<ProcessCleanupReport> {
        self.cancel_and_drain()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    struct HeldProcess(Child);

    impl Drop for HeldProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn held_process(token: &str) -> HeldProcess {
        HeldProcess(
            Command::new("cat")
                .env(harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV, token)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .expect("start an actually held process"),
        )
    }

    #[test]
    fn cancelling_one_call_retires_its_process_and_preserves_another_call() {
        let own_token = harn_vm::op_interrupt::new_process_cleanup_token();
        let foreign_token = harn_vm::op_interrupt::new_process_cleanup_token();
        let mut own = held_process(&own_token);
        let mut foreign = held_process(&foreign_token);
        let parent = Arc::new(AtomicBool::new(false));
        let scope =
            CallProcessScope::new(Arc::new(AtomicBool::new(false)), Some(Arc::clone(&parent)));
        let foreign_scope =
            CallProcessScope::new(Arc::new(AtomicBool::new(false)), Some(Arc::clone(&parent)));
        let _own = harn_vm::op_interrupt::register_active_process_cleanup(
            Some(own.0.id()),
            &own_token,
            Some(Arc::clone(&scope.cancel)),
        );
        let _foreign = harn_vm::op_interrupt::register_active_process_cleanup(
            Some(foreign.0.id()),
            &foreign_token,
            Some(Arc::clone(&foreign_scope.cancel)),
        );
        assert!(own.0.try_wait().unwrap().is_none());
        assert!(foreign.0.try_wait().unwrap().is_none());

        let _ = scope.cancel_and_drain();

        assert!(
            own.0.try_wait().unwrap().is_some(),
            "owned process must exit"
        );
        assert!(
            foreign.0.try_wait().unwrap().is_none(),
            "an unrelated call in the same process must remain live"
        );
        assert!(!foreign_scope.cancel.load(Ordering::SeqCst));
        assert!(
            !parent.load(Ordering::SeqCst),
            "one call must not cancel its VM"
        );
    }
}
