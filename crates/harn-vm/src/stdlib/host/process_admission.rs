//! Test-host process admission without product-runtime policy leakage.
//!
//! Embedders that fan out independent VMs may install one shared gate per
//! worker pool. The local `process.exec` path consults it after command policy
//! has resolved the request. Proven read-only commands bypass the gate; every
//! other subprocess acquires the bounded lane before spawn. Waiting for that
//! host resource pauses the VM's outer execution safety rail and is reported
//! separately from user-code execution.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
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
struct ProcessAdmissionContext {
    gate: Arc<ProcessAdmissionGate>,
    waited_ms: Arc<AtomicU64>,
}

thread_local! {
    static PROCESS_ADMISSION_CONTEXT: RefCell<Option<ProcessAdmissionContext>> =
        const { RefCell::new(None) };
}

pub struct ProcessAdmissionScope {
    previous: Option<ProcessAdmissionContext>,
    waited_ms: Arc<AtomicU64>,
}

/// Install a case-local receipt over a run-shared process gate.
pub fn scope_process_admission(gate: Arc<ProcessAdmissionGate>) -> ProcessAdmissionScope {
    let waited_ms = Arc::new(AtomicU64::new(0));
    let current = ProcessAdmissionContext {
        gate,
        waited_ms: Arc::clone(&waited_ms),
    };
    let previous = PROCESS_ADMISSION_CONTEXT.with(|slot| slot.replace(Some(current)));
    ProcessAdmissionScope {
        previous,
        waited_ms,
    }
}

impl ProcessAdmissionScope {
    pub fn waited_ms(&self) -> u64 {
        self.waited_ms.load(Ordering::Acquire)
    }
}

impl Drop for ProcessAdmissionScope {
    fn drop(&mut self) {
        PROCESS_ADMISSION_CONTEXT.with(|slot| {
            slot.replace(self.previous.take());
        });
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
    let started_ms = clock.monotonic_ms();
    let permit = Arc::clone(&admission.gate.semaphore)
        .acquire_owned()
        .await
        .map_err(|_| VmError::Runtime("process admission gate closed".to_string()))?;
    let waited_ms = clock.monotonic_ms().saturating_sub(started_ms);
    admission.waited_ms.fetch_add(
        u64::try_from(waited_ms).unwrap_or_default(),
        Ordering::AcqRel,
    );
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
}
