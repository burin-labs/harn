//! Process-owned recovery for cancelled agent sessions.
//!
//! A live transcript journal is a durable-write obligation. Capacity is
//! reserved when the journal is claimed, before the agent session is admitted,
//! so cancellation never discovers that recovery is full after work started.
//! Detached recovery scopes only the two runtimes that own session state; it
//! does not retain the VM's full ambient policy, prompt, bridge, and sink state.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use pin_project_lite::pin_project;

use crate::value::VmError;

/// Process-wide ceiling on accepted live agent lifecycles. Admission fails
/// before the journal is claimed, so cleanup itself has no overflow path.
const MAX_LIFECYCLE_RESERVATIONS: usize = 1_024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LifecycleKey {
    execution_id: String,
    session_runtime: usize,
    host_runtime: usize,
    session_id: String,
    run_id: String,
}

#[derive(Clone, Debug)]
struct LifecycleEntry {
    task_id: String,
    cleanup_scheduled: bool,
}

#[derive(Default)]
struct LifecycleRegistry {
    entries: HashMap<LifecycleKey, LifecycleEntry>,
}

impl LifecycleRegistry {
    fn reserve(
        &mut self,
        key: LifecycleKey,
        task_id: String,
    ) -> Result<(), LifecycleAdmissionError> {
        if self.entries.contains_key(&key) {
            return Err(LifecycleAdmissionError::Duplicate);
        }
        if self.entries.len() >= MAX_LIFECYCLE_RESERVATIONS {
            return Err(LifecycleAdmissionError::AtCapacity {
                pending: self.entries.len(),
            });
        }
        self.entries.insert(
            key,
            LifecycleEntry {
                task_id,
                cleanup_scheduled: false,
            },
        );
        Ok(())
    }

    fn release(&mut self, key: &LifecycleKey) {
        self.entries.remove(key);
    }

    fn activate_task(&mut self, runtime: &RuntimeKey, task_id: &str) -> CleanupActivation {
        let mut matched = 0;
        let mut already_scheduled = true;
        for (key, entry) in &mut self.entries {
            if RuntimeKey::from(key) != *runtime || entry.task_id != task_id {
                continue;
            }
            matched += 1;
            already_scheduled &= entry.cleanup_scheduled;
            entry.cleanup_scheduled = true;
        }
        match (matched, already_scheduled) {
            (0, _) => CleanupActivation::NoSessions,
            (_, true) => CleanupActivation::AlreadyScheduled,
            (sessions, false) => CleanupActivation::Scheduled { sessions },
        }
    }

    fn task_reservation_count(&self, runtime: &RuntimeKey, task_id: &str) -> usize {
        self.entries
            .iter()
            .filter(|(key, entry)| RuntimeKey::from(*key) == *runtime && entry.task_id == task_id)
            .count()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleAdmissionError {
    Duplicate,
    AtCapacity { pending: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupActivation {
    NoSessions,
    AlreadyScheduled,
    Scheduled { sessions: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeKey {
    execution_id: String,
    session_runtime: usize,
    host_runtime: usize,
}

impl From<&LifecycleKey> for RuntimeKey {
    fn from(key: &LifecycleKey) -> Self {
        Self {
            execution_id: key.execution_id.clone(),
            session_runtime: key.session_runtime,
            host_runtime: key.host_runtime,
        }
    }
}

fn registry() -> &'static Mutex<LifecycleRegistry> {
    static REGISTRY: OnceLock<Mutex<LifecycleRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(LifecycleRegistry::default()))
}

fn lock_registry() -> std::sync::MutexGuard<'static, LifecycleRegistry> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII proof that one accepted journal owns process cleanup capacity.
pub(crate) struct LifecycleReservation {
    key: LifecycleKey,
}

impl Drop for LifecycleReservation {
    fn drop(&mut self) {
        lock_registry().release(&self.key);
    }
}

pub(crate) fn reserve(
    execution_id: &str,
    session_id: &str,
    run_id: &str,
    task_id: &str,
) -> Result<LifecycleReservation, VmError> {
    let session_runtime = crate::agent_sessions::active_session_runtime();
    let host_runtime = crate::llm::agent_session_host::active_agent_host_session_runtime();
    let key = LifecycleKey {
        execution_id: execution_id.to_string(),
        session_runtime: Arc::as_ptr(&session_runtime) as usize,
        host_runtime: Arc::as_ptr(&host_runtime) as usize,
        session_id: session_id.to_string(),
        run_id: run_id.to_string(),
    };
    lock_registry()
        .reserve(key.clone(), task_id.to_string())
        .map_err(|error| match error {
            LifecycleAdmissionError::Duplicate => VmError::Runtime(format!(
                "agent lifecycle reservation already exists for session `{session_id}` run `{run_id}`"
            )),
            LifecycleAdmissionError::AtCapacity { pending } => VmError::Runtime(format!(
                "agent lifecycle admission refused before session start: pending={pending} limit={MAX_LIFECYCLE_RESERVATIONS}"
            )),
        })?;
    Ok(LifecycleReservation { key })
}

#[derive(Clone)]
pub(crate) struct CleanupRuntimes {
    execution_id: String,
    session: Arc<crate::agent_sessions::AgentSessionRuntime>,
    host: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
}

impl CleanupRuntimes {
    pub(crate) fn new(
        execution_id: String,
        session: Arc<crate::agent_sessions::AgentSessionRuntime>,
        host: Arc<crate::llm::agent_session_host::AgentHostSessionRuntime>,
    ) -> Self {
        Self {
            execution_id,
            session,
            host,
        }
    }

    fn key(&self) -> RuntimeKey {
        RuntimeKey {
            execution_id: self.execution_id.clone(),
            session_runtime: Arc::as_ptr(&self.session) as usize,
            host_runtime: Arc::as_ptr(&self.host) as usize,
        }
    }
}

pin_project! {
    struct ScopedCleanup<F> {
        runtimes: CleanupRuntimes,
        #[pin]
        inner: F,
    }
}

impl<F: Future> Future for ScopedCleanup<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let previous_session =
            crate::agent_sessions::swap_active_session_runtime(this.runtimes.session.clone());
        let previous_host = crate::llm::agent_session_host::swap_active_agent_host_session_runtime(
            this.runtimes.host.clone(),
        );
        let result = this.inner.poll(context);
        let _ =
            crate::llm::agent_session_host::swap_active_agent_host_session_runtime(previous_host);
        let _ = crate::agent_sessions::swap_active_session_runtime(previous_session);
        result
    }
}

/// Activate the reservation already owned by `task_id` and retry terminal
/// persistence until it commits. The reservation is released only by the
/// canonical journal-clear operation.
pub(crate) fn schedule(task_id: String, runtimes: CleanupRuntimes) {
    match lock_registry().activate_task(&runtimes.key(), &task_id) {
        CleanupActivation::NoSessions | CleanupActivation::AlreadyScheduled => return,
        CleanupActivation::Scheduled { .. } => {}
    }

    let runtime_key = runtimes.key();
    crate::vm::subtask::spawn_lifecycle_cleanup(ScopedCleanup {
        runtimes,
        inner: async move {
            let cleanup_execution_id = runtime_key.execution_id.clone();
            let cleanup_task_id = task_id.clone();
            retry_task_cleanup(
                task_id,
                runtime_key,
                move || {
                    let execution_id = cleanup_execution_id.clone();
                    let task_id = cleanup_task_id.clone();
                    async move {
                        crate::llm::agent_session_host::cancellation::abandon_task_sessions(
                            &execution_id,
                            &task_id,
                        )
                        .await
                    }
                },
                tokio::time::sleep,
            )
            .await;
        },
    });
}

async fn retry_task_cleanup<Cleanup, CleanupFuture, Sleep, SleepFuture>(
    task_id: String,
    runtime_key: RuntimeKey,
    mut cleanup: Cleanup,
    mut sleep: Sleep,
) where
    Cleanup: FnMut() -> CleanupFuture,
    CleanupFuture: Future<Output = Result<(), VmError>>,
    Sleep: FnMut(Duration) -> SleepFuture,
    SleepFuture: Future<Output = ()>,
{
    let mut attempt = 0_u64;
    let mut retry_delay = Duration::from_millis(10);
    loop {
        attempt = attempt.saturating_add(1);
        match cleanup().await {
            Ok(()) => {
                let pending = lock_registry().task_reservation_count(&runtime_key, &task_id);
                if pending == 0 {
                    return;
                }
                crate::events::log_warn(
                    "agent.task_cancel_cleanup_incomplete",
                    &format!(
                        "task={task_id} cleanup returned without releasing every lifecycle owner: pending={pending}"
                    ),
                );
            }
            Err(error) => {
                if attempt <= 16 || attempt.is_power_of_two() {
                    let pending = lock_registry().task_reservation_count(&runtime_key, &task_id);
                    crate::events::log_warn(
                        "agent.task_cancel_cleanup",
                        &format!(
                            "task={task_id} durable cleanup remains pending: attempt={attempt} pending={pending} error={error}"
                        ),
                    );
                }
            }
        }
        sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
#[path = "agent_lifecycle_cleanup/tests.rs"]
mod tests;
