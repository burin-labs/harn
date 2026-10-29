//! The context one hostlib builtin call runs under: the caller's deadline and
//! cancel signal, and a typed progress channel on the dispatching tool call.
//!
//! The registry installs a context around every builtin that may wait on an
//! external process; [`HostCallContext::current`] hands any other caller an
//! empty one. The tool-call handle is retained across the worker handoff so
//! the worker sees a stop immediately, alongside the VM interrupt sources.

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, MutexGuard, PoisonError};
#[cfg(feature = "ast")]
use std::time::Duration;
use std::time::Instant;

#[cfg(feature = "ast")]
use harn_vm::agent_events::{AgentEvent, ToolCallStatus, ToolMutationStatus};

/// How often a wait that a caller can stop rechecks its interrupt sources.
#[cfg(feature = "ast")]
pub(crate) const INTERRUPT_POLL: Duration = Duration::from_millis(50);
/// How often a long wait reports that it is still in the same phase.
#[cfg(feature = "ast")]
pub(crate) const PROGRESS_HEARTBEAT: Duration = Duration::from_secs(1);

static WORKSPACE_WRITES: Mutex<()> = Mutex::new(());

/// Proof that the holder is the only hostlib builtin writing files in this
/// process. Every writing builtin holds one for its whole write, so a builtin
/// running off the VM's thread never interleaves its writes with another's.
/// Take it after the code-index lock, never before.
pub(crate) struct WorkspaceWrites(#[allow(dead_code)] MutexGuard<'static, ()>);

/// Wait until no other builtin is writing, then hold the write lease. Every
/// holder writes without waiting on anything a caller can stop, so this wait
/// is bounded by one write.
pub(crate) fn workspace_writes() -> WorkspaceWrites {
    WorkspaceWrites(
        WORKSPACE_WRITES
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// Why a call stopped before finishing its work.
#[cfg(feature = "ast")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interrupt {
    /// The tool call, its VM, or its session was cancelled.
    Cancelled,
    /// The caller's deadline or the request's own budget passed.
    DeadlineExceeded,
}

/// What a call is doing, reported on its tool call's progress channel.
#[cfg(feature = "ast")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Starting a language server and waiting until it can answer.
    ServerStart,
    /// Waiting for a server to finish loading the project.
    Indexing,
    /// Asking a server for edits or what names refer to.
    Resolving,
    /// Writing the planned edits.
    Applying,
}

#[cfg(feature = "ast")]
impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::ServerStart => "server_start",
            Self::Indexing => "indexing",
            Self::Resolving => "resolving",
            Self::Applying => "applying",
        }
    }
}

/// The tool call a builtin runs for, so progress reaches its host.
#[cfg_attr(
    not(feature = "ast"),
    expect(dead_code, reason = "only the code index reads call progress state")
)]
#[derive(Clone, Debug)]
pub(crate) struct CallAddress {
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
}

impl CallAddress {
    /// The tool call dispatching the current task, if any. Read it on the
    /// VM's thread: the dispatch scope is task-local.
    pub(crate) fn current() -> Option<Self> {
        let session_id = harn_vm::agent_sessions::current_session_id()?;
        let tool_call_id = harn_vm::agent_sessions::current_tool_call_id()?;
        let tool_name =
            harn_vm::tool_call_cancellations::lookup(&session_id, &tool_call_id)?.tool_name;
        Some(Self {
            session_id,
            tool_call_id,
            tool_name,
        })
    }
}

/// Deadline, cancellation, and progress for one builtin call.
#[cfg_attr(
    not(feature = "ast"),
    expect(dead_code, reason = "only the code index reads call progress state")
)]
#[derive(Clone, Debug)]
pub(crate) struct HostCallContext {
    call: Option<CallAddress>,
    /// The live tool handle is signalled before asynchronous dispatch unwinds.
    cancellation: Option<harn_vm::tool_call_cancellations::Handle>,
    /// The request's own budget, beside the caller's deadline.
    budget: Option<Instant>,
    started: Instant,
    last_report: Cell<Option<Instant>>,
}

thread_local! {
    static CURRENT: RefCell<Option<HostCallContext>> = const { RefCell::new(None) };
}

/// Restores the previous context on drop.
pub(crate) struct HostCallGuard(Option<HostCallContext>);

impl Drop for HostCallGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|slot| *slot.borrow_mut() = previous);
    }
}

impl HostCallContext {
    pub(crate) fn new(call: Option<CallAddress>) -> Self {
        let cancellation = call.as_ref().and_then(|call| {
            harn_vm::tool_call_cancellations::lookup(&call.session_id, &call.tool_call_id)
        });
        Self {
            call,
            cancellation,
            budget: None,
            started: Instant::now(),
            last_report: Cell::new(None),
        }
    }

    #[cfg(feature = "ast")]
    /// The context the registry installed for this call, or an empty one.
    pub(crate) fn current() -> Self {
        CURRENT
            .with(|slot| slot.borrow().clone())
            .unwrap_or_else(|| Self::new(None))
    }

    /// Install this context on the current thread until the guard drops.
    pub(crate) fn enter(self) -> HostCallGuard {
        HostCallGuard(CURRENT.with(|slot| slot.borrow_mut().replace(self)))
    }

    /// A stopped call must not continue with later actions in its handler.
    pub(crate) fn tool_call_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(harn_vm::tool_call_cancellations::Handle::is_cancelled)
    }

    #[cfg(feature = "ast")]
    /// This context, also bounded by a request's own `budget`.
    pub(crate) fn with_budget(mut self, budget: Duration) -> Self {
        let until = Instant::now() + budget;
        self.budget = Some(self.budget.map_or(until, |earlier| earlier.min(until)));
        self
    }

    #[cfg(feature = "ast")]
    /// Why the call must stop now, if it must.
    pub(crate) fn interrupted(&self) -> Option<Interrupt> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(harn_vm::tool_call_cancellations::Handle::is_cancelled)
            || harn_vm::op_interrupt::cancel_requested()
        {
            return Some(Interrupt::Cancelled);
        }
        let expired = harn_vm::op_interrupt::requested()
            || self.budget.is_some_and(|budget| Instant::now() >= budget);
        expired.then_some(Interrupt::DeadlineExceeded)
    }

    #[cfg(feature = "ast")]
    /// The commit point, reached once the write lease is held. A stop
    /// requested before it, including while waiting for another writer,
    /// returns the interrupt and `write` never runs. Past it, `write` runs to
    /// completion under the lease, so a later stop cannot leave a partial
    /// write and no other builtin's writes interleave with it.
    pub(crate) fn commit<T>(
        &self,
        detail: &str,
        write: impl FnOnce(&WorkspaceWrites) -> T,
    ) -> Result<T, Interrupt> {
        let writes = workspace_writes();
        let _commit = self
            .cancellation
            .as_ref()
            .map(|handle| handle.begin_write_commit());
        if let Some(interrupt) = self.interrupted() {
            return Err(interrupt);
        }
        self.report(Phase::Applying, detail);
        Ok(write(&writes))
    }

    #[cfg(feature = "ast")]
    /// Report entering `phase`.
    pub(crate) fn report(&self, phase: Phase, detail: &str) {
        let now = Instant::now();
        self.last_report.set(Some(now));
        let elapsed_ms = now.duration_since(self.started).as_millis() as u64;
        tracing::debug!(
            target: "harn_hostlib::host_call",
            phase = phase.as_str(),
            detail,
            elapsed_ms,
            "host call progress",
        );
        let Some(call) = &self.call else {
            return;
        };
        harn_vm::agent_events::emit_event(&AgentEvent::ToolCallUpdate {
            session_id: call.session_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
            tool_name: call.tool_name.clone(),
            status: ToolCallStatus::InProgress,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: ToolMutationStatus::Unknown,
            changed_paths: None,
            data: Some(serde_json::json!({
                "progress": {
                    "phase": phase.as_str(),
                    "detail": detail,
                    "elapsed_ms": elapsed_ms,
                    "cancellable": !matches!(phase, Phase::Applying),
                },
            })),
            health: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
    }

    #[cfg(feature = "ast")]
    /// Report `phase` again when a wait has been quiet for a heartbeat.
    pub(crate) fn heartbeat(&self, phase: Phase, detail: &str) {
        let quiet = self
            .last_report
            .get()
            .is_none_or(|last| last.elapsed() >= PROGRESS_HEARTBEAT);
        if quiet {
            self.report(phase, detail);
        }
    }
}

#[cfg(all(test, feature = "ast"))]
mod tests {
    use super::*;

    #[test]
    fn tool_stop_is_observed_before_worker_dispatch_unwinds() {
        let (handle, _guard) =
            harn_vm::tool_call_cancellations::register("host-call-stop-test", "call", "rename")
                .unwrap();
        let context = HostCallContext::new(Some(CallAddress {
            session_id: "host-call-stop-test".into(),
            tool_call_id: "call".into(),
            tool_name: "rename".into(),
        }));
        assert_eq!(context.interrupted(), None);
        assert!(handle.cancel("stop", false));
        assert_eq!(context.interrupted(), Some(Interrupt::Cancelled));
    }
}
