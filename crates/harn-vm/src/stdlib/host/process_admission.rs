//! Test-host process admission without product-runtime policy leakage.
//!
//! Embedders that fan out independent VMs may install one shared gate per
//! worker pool. The local `process.exec` path consults it after command policy
//! has resolved the request. Proven read-only commands bypass the gate; every
//! other subprocess acquires the bounded lane before spawn. Waiting for that
//! host resource pauses the VM's outer execution safety rail and is reported
//! separately from user-code execution.

use std::cell::RefCell;
use std::sync::Arc;

use harn_clock::Clock;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::value::VmError;
use crate::vm::AsyncBuiltinCtx;

pub(super) fn process_requires_admission(command_policy_context: &serde_json::Value) -> bool {
    crate::orchestration::command_workspace_effect_json(command_policy_context)["effect"]
        != "read_effect"
}

#[derive(Debug)]
pub struct ProcessAdmissionGate {
    semaphore: Arc<Semaphore>,
    clock: Arc<dyn Clock>,
}

impl ProcessAdmissionGate {
    pub fn new(max_concurrent: usize, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            clock,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ProcessAdmissionContext {
    gate: Arc<ProcessAdmissionGate>,
    receipt: Arc<ProcessAdmissionReceipt>,
}

#[derive(Debug)]
struct ProcessAdmissionReceipt {
    clock: Arc<dyn Clock>,
    state: parking_lot::Mutex<ProcessAdmissionReceiptState>,
}

#[derive(Debug, Default)]
struct ProcessAdmissionReceiptState {
    active_waiters: usize,
    interval_started_ms: i64,
    waited_ms: u64,
}

impl ProcessAdmissionReceipt {
    fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            state: parking_lot::Mutex::new(ProcessAdmissionReceiptState::default()),
        })
    }

    fn begin(self: &Arc<Self>) -> ProcessAdmissionWaitGuard {
        let mut state = self.state.lock();
        if state.active_waiters == 0 {
            state.interval_started_ms = self.clock.monotonic_ms();
        }
        state.active_waiters = state.active_waiters.saturating_add(1);
        drop(state);
        ProcessAdmissionWaitGuard {
            receipt: Arc::clone(self),
        }
    }

    fn waited_ms(&self) -> u64 {
        let state = self.state.lock();
        let active_ms = (state.active_waiters != 0)
            .then(|| {
                self.clock
                    .monotonic_ms()
                    .saturating_sub(state.interval_started_ms)
            })
            .and_then(|elapsed| u64::try_from(elapsed).ok())
            .unwrap_or_default();
        state.waited_ms.saturating_add(active_ms)
    }
}

struct ProcessAdmissionWaitGuard {
    receipt: Arc<ProcessAdmissionReceipt>,
}

impl Drop for ProcessAdmissionWaitGuard {
    fn drop(&mut self) {
        let mut state = self.receipt.state.lock();
        debug_assert!(state.active_waiters > 0, "process admission wait underflow");
        state.active_waiters = state.active_waiters.saturating_sub(1);
        if state.active_waiters == 0 {
            let elapsed = self
                .receipt
                .clock
                .monotonic_ms()
                .saturating_sub(state.interval_started_ms);
            state.waited_ms = state
                .waited_ms
                .saturating_add(u64::try_from(elapsed).unwrap_or_default());
        }
    }
}

thread_local! {
    static PROCESS_ADMISSION_CONTEXT: RefCell<Option<ProcessAdmissionContext>> =
        const { RefCell::new(None) };
}

pub(crate) fn swap_process_admission_context(
    new: Option<ProcessAdmissionContext>,
) -> Option<ProcessAdmissionContext> {
    PROCESS_ADMISSION_CONTEXT.with(|slot| slot.replace(new))
}

pub struct ProcessAdmissionScope {
    previous: Option<ProcessAdmissionContext>,
    receipt: Arc<ProcessAdmissionReceipt>,
}

/// Install a case-local receipt over a run-shared process gate.
pub fn scope_process_admission(gate: Arc<ProcessAdmissionGate>) -> ProcessAdmissionScope {
    let receipt = ProcessAdmissionReceipt::new(Arc::clone(&gate.clock));
    let current = ProcessAdmissionContext {
        gate,
        receipt: Arc::clone(&receipt),
    };
    let previous = swap_process_admission_context(Some(current));
    ProcessAdmissionScope { previous, receipt }
}

impl ProcessAdmissionScope {
    pub fn waited_ms(&self) -> u64 {
        self.receipt.waited_ms()
    }
}

impl Drop for ProcessAdmissionScope {
    fn drop(&mut self) {
        swap_process_admission_context(self.previous.take());
    }
}

pub(super) async fn acquire_process_admission(
    ctx: Option<&AsyncBuiltinCtx>,
) -> Result<Option<OwnedSemaphorePermit>, VmError> {
    let Some(admission) = PROCESS_ADMISSION_CONTEXT.with(|slot| slot.borrow().as_ref().cloned())
    else {
        return Ok(None);
    };
    let clock = Arc::clone(&admission.gate.clock);
    let deadline_pause = ctx.and_then(|ctx| ctx.pause_execution_deadline(Arc::clone(&clock)));
    let receipt_wait = admission.receipt.begin();
    let permit = Arc::clone(&admission.gate.semaphore)
        .acquire_owned()
        .await
        .map_err(|_| VmError::Runtime("process admission gate closed".to_string()))?;
    drop(receipt_wait);
    drop(deadline_pause);
    Ok(Some(permit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn resolved_effect_is_the_single_admission_classifier() {
        let context = |argv: &[&str]| {
            serde_json::json!({
                "request": { "mode": "argv", "argv": argv },
            })
        };

        assert!(!process_requires_admission(&context(&[
            "git", "status", "--short"
        ])));
        assert!(process_requires_admission(&context(&[
            "rustfmt",
            "--emit",
            "stdout",
            "source.rs"
        ])));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_receipt_uses_the_injected_monotonic_clock() {
        let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
        let gate_clock: Arc<dyn Clock> = clock.clone();
        let scope = scope_process_admission(ProcessAdmissionGate::new(1, gate_clock));
        let first = acquire_process_admission(None)
            .await
            .expect("gate is open")
            .expect("scoped admission returns a permit");
        let second = acquire_process_admission(None);
        tokio::pin!(second);
        assert!(futures::poll!(&mut second).is_pending());

        clock.advance(Duration::from_millis(35));
        drop(first);
        let _second = second.await.expect("gate remains open");

        assert_eq!(scope.waited_ms(), 35);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_receipt_counts_overlapping_waits_as_one_wall_interval() {
        let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
        let gate_clock: Arc<dyn Clock> = clock.clone();
        let scope = scope_process_admission(ProcessAdmissionGate::new(1, gate_clock));
        let first = acquire_process_admission(None)
            .await
            .expect("gate is open")
            .expect("scoped admission returns a permit");
        let second = acquire_process_admission(None);
        let third = acquire_process_admission(None);
        tokio::pin!(second, third);
        assert!(futures::poll!(&mut second).is_pending());
        assert!(futures::poll!(&mut third).is_pending());

        clock.advance(Duration::from_millis(20));
        drop(first);
        let second = second.await.expect("gate remains open");
        clock.advance(Duration::from_millis(15));
        drop(second);
        let _third = third.await.expect("gate remains open");

        assert_eq!(scope.waited_ms(), 35, "overlapping waits form one interval");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_receipt_closes_a_cancelled_wait_interval() {
        let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
        let gate_clock: Arc<dyn Clock> = clock.clone();
        let scope = scope_process_admission(ProcessAdmissionGate::new(1, gate_clock));
        let first = acquire_process_admission(None)
            .await
            .expect("gate is open")
            .expect("scoped admission returns a permit");
        let mut pending = Box::pin(acquire_process_admission(None));
        assert!(futures::poll!(&mut pending).is_pending());

        clock.advance(Duration::from_millis(12));
        assert_eq!(scope.waited_ms(), 12, "live intervals remain observable");
        drop(pending);
        assert_eq!(scope.waited_ms(), 12);

        drop(first);
        let _next = acquire_process_admission(None)
            .await
            .expect("cancelled waiter releases its receipt depth");
        assert_eq!(scope.waited_ms(), 12);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_subtask_carries_the_case_admission_context() {
        let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
        let gate_clock: Arc<dyn Clock> = clock.clone();
        let scope = scope_process_admission(ProcessAdmissionGate::new(1, gate_clock));
        let first = acquire_process_admission(None)
            .await
            .expect("gate is open")
            .expect("outer task receives a permit");

        let second = crate::orchestration::scope_inline_subtask(acquire_process_admission(None));
        tokio::pin!(second);
        assert!(
            futures::poll!(&mut second).is_pending(),
            "the inline task must observe the occupied case-local gate"
        );

        clock.advance(Duration::from_millis(17));
        drop(first);
        let permit = second
            .await
            .expect("gate remains open")
            .expect("inline task receives the shared permit");
        drop(permit);
        assert_eq!(scope.waited_ms(), 17);
    }
}
