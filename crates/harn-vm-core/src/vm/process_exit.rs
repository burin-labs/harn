use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Terminal exit requested by any VM in one execution tree. A process exit is
/// global control flow, so child VMs share this latch with their parent rather
/// than relying on a task being explicitly awaited.
pub(crate) struct ProcessExitRequest {
    code: Mutex<Option<i32>>,
    requested: AtomicBool,
}

impl ProcessExitRequest {
    pub(super) fn new() -> Self {
        Self {
            code: Mutex::new(None),
            requested: AtomicBool::new(false),
        }
    }

    fn request(&self, code: i32) {
        let mut recorded = self
            .code
            .lock()
            .expect("process exit request lock poisoned");
        if recorded.is_none() {
            *recorded = Some(code);
            self.requested.store(true, Ordering::Release);
        }
    }

    #[inline]
    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn code(&self) -> Option<i32> {
        if !self.is_requested() {
            return None;
        }
        *self
            .code
            .lock()
            .expect("process exit request lock poisoned")
    }
}

impl super::Vm {
    pub(crate) fn request_process_exit(&self, code: i32) {
        self.process_exit_request.request(code);
    }

    pub(crate) fn requested_process_exit(&self) -> Option<i32> {
        self.process_exit_request.code()
    }

    /// Lock-free form of [`Self::requested_process_exit`] for the per-instruction
    /// interrupt check.
    #[inline]
    pub(crate) fn process_exit_requested(&self) -> bool {
        self.process_exit_request.is_requested()
    }
}
