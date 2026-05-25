use serde::{Deserialize, Serialize};

/// One coalesced filesystem notification from a hostlib `fs_watch`
/// subscription.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FsWatchEvent {
    pub kind: String,
    pub paths: Vec<String>,
    pub relative_paths: Vec<String>,
    pub raw_kind: String,
    pub error: Option<String>,
}

/// Typed worker lifecycle events emitted by delegated/background agent
/// execution. Bridge-facing worker updates still derive a string status
/// from these variants, but the runtime no longer passes raw status
/// strings around internally.
///
/// `Spawned`/`Completed`/`Failed`/`Cancelled` are the four terminal-or-start
/// states. `Progressed` is fired on intermediate milestones (e.g. a
/// retriggerable worker resuming from `awaiting_input`, or a workflow
/// stage completing without ending the worker). `WaitingForInput` covers
/// retriggerable workers that finish a cycle but stay alive pending the
/// next host-supplied trigger payload. `Suspended`/`Resumed` cover
/// cooperative mid-loop pause and warm resume (harn#1836); the
/// `agent_loop` honors the pause signal at the next turn boundary,
/// distinct from a hard `Cancelled` interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum WorkerEvent {
    WorkerSpawned,
    WorkerProgressed,
    WorkerWaitingForInput,
    WorkerSuspended,
    WorkerResumed,
    WorkerCompleted,
    WorkerFailed,
    WorkerCancelled,
}

impl WorkerEvent {
    /// The full set of `WorkerEvent` variants in canonical order. Mirrors
    /// the pattern used by `ToolCallStatus::ALL` so the protocol-artifact
    /// dumper can enumerate worker status wire values without
    /// special-casing each lifecycle event.
    pub const ALL: [Self; 8] = [
        Self::WorkerSpawned,
        Self::WorkerProgressed,
        Self::WorkerWaitingForInput,
        Self::WorkerSuspended,
        Self::WorkerResumed,
        Self::WorkerCompleted,
        Self::WorkerFailed,
        Self::WorkerCancelled,
    ];

    /// Wire-level status string used by bridge `worker_update` payloads
    /// and ACP `worker_update` session updates. The four canonical
    /// states are mirrored from harn's internal worker `status` field
    /// (`running`/`completed`/`failed`/`cancelled`), and the four newer
    /// lifecycle states pick names that don't collide with any existing
    /// status string.
    pub fn as_status(self) -> &'static str {
        match self {
            Self::WorkerSpawned => "running",
            Self::WorkerProgressed => "progressed",
            Self::WorkerWaitingForInput => "awaiting_input",
            Self::WorkerSuspended => "suspended",
            Self::WorkerResumed => "running",
            Self::WorkerCompleted => "completed",
            Self::WorkerFailed => "failed",
            Self::WorkerCancelled => "cancelled",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkerSpawned => "WorkerSpawned",
            Self::WorkerProgressed => "WorkerProgressed",
            Self::WorkerWaitingForInput => "WorkerWaitingForInput",
            Self::WorkerSuspended => "WorkerSuspended",
            Self::WorkerResumed => "WorkerResumed",
            Self::WorkerCompleted => "WorkerCompleted",
            Self::WorkerFailed => "WorkerFailed",
            Self::WorkerCancelled => "WorkerCancelled",
        }
    }

    /// True for lifecycle events that mean the worker has reached a
    /// final, non-resumable state. Retriggerable awaiting, progressed,
    /// and cooperative suspend/resume milestones are *not* terminal —
    /// the worker keeps running, is waiting for a trigger, or is parked
    /// awaiting an external resume.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::WorkerCompleted | Self::WorkerFailed | Self::WorkerCancelled
        )
    }
}
