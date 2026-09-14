//! Restores the caller event-log scope after a native dispatch.
use super::*;

pub(super) struct ActiveEventLogGuard {
    previous: Option<Arc<AnyEventLog>>,
}

impl Drop for ActiveEventLogGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(log) => {
                install_active_event_log(log);
            }
            None => {
                harn_vm::event_log::reset_active_event_log();
            }
        }
    }
}

pub(super) fn install_scoped_event_log(log: Arc<AnyEventLog>) -> ActiveEventLogGuard {
    let previous = active_event_log();
    install_active_event_log(log);
    ActiveEventLogGuard { previous }
}
