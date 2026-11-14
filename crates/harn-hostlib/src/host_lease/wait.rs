use std::sync::mpsc;

use notify::{RecursiveMode, Watcher};

use super::admission::WaitProgressSchedule;
use super::*;

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(20);

struct AcquireScope<'a> {
    store: &'a HostLeaseStore,
    waiter_id: &'a str,
    queued: bool,
    grant: Option<HostLeaseHandle>,
}

impl<'a> AcquireScope<'a> {
    fn new(store: &'a HostLeaseStore, waiter_id: &'a str) -> Self {
        Self {
            store,
            waiter_id,
            queued: false,
            grant: None,
        }
    }

    fn observe(&mut self, receipt: &HostLeaseAcquireReceipt) {
        if receipt.status == HostLeaseAcquireStatus::Acquired {
            self.queued = false;
        } else {
            self.queued |= receipt.queue.is_some();
        }
        self.grant = receipt.handle.clone();
    }

    fn deliver(
        mut self,
        receipt: HostLeaseAcquireReceipt,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        if self.queued {
            self.store.remove_waiter_owned(self.waiter_id)?;
            self.queued = false;
        }
        self.grant = None;
        Ok(receipt)
    }
}

impl Drop for AcquireScope<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.grant.take() {
            if let Err(error) = self.store.release_owned(&handle) {
                tracing::warn!(lease_id = %handle.lease_id, %error, "host lease grant cleanup remains unconfirmed");
            }
        }
        if self.queued {
            if let Err(error) = self.store.remove_waiter_owned(self.waiter_id) {
                tracing::warn!(waiter_id = self.waiter_id, %error, "host lease waiter cleanup remains unconfirmed");
            }
        }
    }
}

fn retry_registry_cleanup<T>(
    mut operation: impl FnMut() -> Result<T, HostLeaseError>,
) -> Result<T, HostLeaseError> {
    loop {
        match operation() {
            // The blocking acquire owner retains cleanup across lock windows.
            // Each attempt still uses SQLite's normal busy backoff.
            Err(HostLeaseError::Database(error)) if sqlite_is_busy(&error) => {}
            result => return result,
        }
    }
}

impl HostLeaseStore {
    pub(super) fn remove_waiter_owned(&self, waiter_id: &str) -> Result<(), HostLeaseError> {
        retry_registry_cleanup(|| self.remove_waiter(waiter_id))
    }

    pub(crate) fn try_release_owned(
        &self,
        handle: &HostLeaseHandle,
    ) -> Result<Option<HostLeaseReleaseReceipt>, HostLeaseError> {
        match self.release_for_domain_with_registry_timeout(
            &handle.host,
            handle.resource_class,
            &handle.domain,
            &handle.lease_id,
            Duration::ZERO,
        ) {
            Err(HostLeaseError::Database(error)) if sqlite_is_busy(&error) => Ok(None),
            result => result.map(Some),
        }
    }

    pub(crate) fn release_owned(
        &self,
        handle: &HostLeaseHandle,
    ) -> Result<HostLeaseReleaseReceipt, HostLeaseError> {
        retry_registry_cleanup(|| {
            self.release_for_domain(
                &handle.host,
                handle.resource_class,
                &handle.domain,
                &handle.lease_id,
            )
        })
    }

    pub(super) fn acquire_once_interruptible(
        &self,
        request: HostLeaseRequest,
        identity: WaiterIdentity,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let mut scope = AcquireScope::new(self, &identity.waiter_id);
        if interrupted() {
            return Err(HostLeaseError::Interrupted);
        }
        let receipt = self.try_acquire_once_with_registry_timeout(
            request,
            None,
            Some(identity.requested_at_ms.saturating_add(60_000)),
            &identity,
            INTERRUPT_POLL_INTERVAL,
        )?;
        scope.observe(&receipt);
        if interrupted() {
            return Err(HostLeaseError::Interrupted);
        }
        scope.deliver(receipt)
    }

    pub(super) fn acquire_wait_ordered(
        &self,
        request: HostLeaseRequest,
        wait_timeout: Duration,
        identity: WaiterIdentity,
        on_progress: Option<&mut dyn FnMut(&HostLeaseAcquireReceipt)>,
        interrupted: Option<&dyn Fn() -> bool>,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(|error| HostLeaseError::Watch(error.to_string()))?;
        watcher
            .watch(&self.wake_path, RecursiveMode::NonRecursive)
            .map_err(|error| HostLeaseError::Watch(error.to_string()))?;
        self.acquire_wait_notified(
            request,
            wait_timeout,
            identity,
            on_progress,
            interrupted,
            &rx,
        )
    }

    pub(super) fn acquire_wait_notified(
        &self,
        request: HostLeaseRequest,
        wait_timeout: Duration,
        identity: WaiterIdentity,
        mut on_progress: Option<&mut dyn FnMut(&HostLeaseAcquireReceipt)>,
        interrupted: Option<&dyn Fn() -> bool>,
        notifications: &mpsc::Receiver<notify::Result<notify::Event>>,
    ) -> Result<HostLeaseAcquireReceipt, HostLeaseError> {
        // The deadline is decided on the injected clock's monotonic counter.
        // The watcher's `recv_timeout` below only bounds how long one blocking
        // wait lasts; a wake that arrives late or never merely re-attempts.
        let started_at_ms = self.now_ms()?;
        let started_monotonic_ms = self.clock.monotonic_ms();
        let wait_timeout_ms = duration_ms_i64(wait_timeout);
        let deadline_monotonic_ms = started_monotonic_ms
            .checked_add(wait_timeout_ms)
            .ok_or_else(|| {
                HostLeaseError::InvalidRequest(
                    "wait timeout exceeds the monotonic clock".to_string(),
                )
            })?;
        let deadline_at_ms = started_at_ms.saturating_add(wait_timeout_ms);
        let mut progress_schedule = on_progress.as_ref().map(|_| WaitProgressSchedule::new());
        let mut scope = AcquireScope::new(self, &identity.waiter_id);

        loop {
            if interrupted.is_some_and(|check| check()) {
                return Err(HostLeaseError::Interrupted);
            }
            let receipt = self.try_acquire_once_with_registry_timeout(
                request.clone(),
                Some(started_monotonic_ms),
                Some(deadline_at_ms),
                &identity,
                if interrupted.is_some() {
                    INTERRUPT_POLL_INTERVAL
                } else {
                    SQLITE_MUTATION_BUSY_TIMEOUT
                },
            )?;
            scope.observe(&receipt);
            if interrupted.is_some_and(|check| check()) {
                return Err(HostLeaseError::Interrupted);
            }
            let elapsed = self.monotonic_elapsed(started_monotonic_ms);
            if receipt.status == HostLeaseAcquireStatus::Deferred
                && progress_schedule
                    .as_mut()
                    .is_some_and(|schedule| schedule.should_report(elapsed))
            {
                if let Some(observer) = on_progress.as_deref_mut() {
                    observer(&receipt);
                }
            }
            // A progress callback can outlive this admission snapshot. Decide
            // expiry from the receipt, so a later clock read cannot return it stale.
            if receipt.status == HostLeaseAcquireStatus::Acquired
                || receipt.waited_ms >= wait_timeout_ms.unsigned_abs()
            {
                return scope.deliver(receipt);
            }
            let wake_at = receipt
                .defer
                .as_ref()
                .and_then(|defer| defer.next_wake_at_ms)
                .map(|wake| wake.min(deadline_at_ms))
                .unwrap_or(deadline_at_ms);
            let wake_duration =
                Duration::from_millis(wake_at.saturating_sub(receipt.observed_at_ms).max(1) as u64);
            let remaining = Duration::from_millis(
                deadline_monotonic_ms
                    .saturating_sub(self.clock.monotonic_ms())
                    .max(0) as u64,
            );
            if remaining.is_zero() {
                // Re-observe expiry and remove the waiter through the terminal path.
                continue;
            }
            let mut wait_duration = wake_duration.min(remaining);
            if interrupted.is_some() {
                wait_duration = wait_duration.min(INTERRUPT_POLL_INTERVAL);
            }
            if let Some(schedule) = progress_schedule {
                wait_duration = wait_duration
                    .min(schedule.until_next(self.monotonic_elapsed(started_monotonic_ms)));
            }
            match notifications.recv_timeout(wait_duration) {
                Ok(Ok(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Err(error)) => return Err(HostLeaseError::Watch(error.to_string())),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(HostLeaseError::Watch(
                        "host lease watcher disconnected".to_string(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "wait_cleanup_tests.rs"]
mod cleanup_tests;
