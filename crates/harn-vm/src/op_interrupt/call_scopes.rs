//! Cancellation identities projected onto the existing native cleanup registry.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::{
    register_process_cleanup_entry, signal_active_process_cleanups_matching,
    signal_registered_process, terminate_pid_tree_group_and_token_with_report,
    ActiveProcessCleanup, ActiveProcessCleanupGuard, ProcessCleanupReport, ACTIVE_PROCESS_CLEANUPS,
};
#[cfg(unix)]
use super::{wait_for_report_children_to_exit, SUBPROCESS_TERM_GRACE};

/// A foreground call reserves its launch lifetime before it has a native PID.
pub trait ActiveProcessCleanupScope: std::fmt::Debug + Send + Sync {
    fn signal(&self, signal: i32) -> ProcessCleanupReport;
    fn terminate(&self) -> Option<ProcessCleanupReport>;
}

pub fn register_active_process_cleanup_scope(
    owner_cancel_token: Arc<AtomicBool>,
    scope: Arc<dyn ActiveProcessCleanupScope>,
) -> ActiveProcessCleanupGuard {
    register_process_cleanup_entry(ActiveProcessCleanup {
        pid: None,
        cleanup_token: String::new(),
        owner_cancel_token: Some(owner_cancel_token),
        scope: Some(scope),
    })
}

pub fn signal_active_process_cleanups_for_cancel_token(
    signal: i32,
    cancel_token: &Arc<AtomicBool>,
) -> ProcessCleanupReport {
    signal_active_process_cleanups_matching(signal, |entry| {
        entry
            .owner_cancel_token
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, cancel_token))
    })
}

/// Retire one cancelled foreground call before its async waiter can disappear.
/// `None` means that call has no enrolled process, not a successful OS census.
pub fn terminate_active_process_cleanups_for_cancel_token(
    cancel_token: &Arc<AtomicBool>,
) -> Option<ProcessCleanupReport> {
    let entries = ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .values()
        .filter(|entry| {
            entry
                .owner_cancel_token
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, cancel_token))
        })
        .cloned()
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return None;
    }
    let mut report = ProcessCleanupReport::default();
    let mut measured = false;
    for entry in entries {
        if let Some(scope) = &entry.scope {
            if let Some(retired) = scope.terminate() {
                measured = true;
                report.merge(retired);
            }
        } else if let Some(pid) = entry.pid {
            measured = true;
            report.merge(terminate_pid_tree_group_and_token_with_report(
                pid,
                Some(&entry.cleanup_token),
            ));
        } else {
            measured = true;
            let mut retired = signal_registered_process(&entry, 15);
            #[cfg(unix)]
            wait_for_report_children_to_exit(&retired, SUBPROCESS_TERM_GRACE);
            retired.merge(signal_registered_process(&entry, 9));
            retired.refresh_survivor_status();
            report.merge(retired);
        }
    }
    measured.then_some(report)
}
