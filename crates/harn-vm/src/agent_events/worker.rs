use serde::{Deserialize, Serialize};

/// Structured terminal outcome for one delegated sub-agent run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentTerminalStatus {
    Success,
    Failure,
    Cancellation,
    Timeout,
}

/// One agent run and the session whose transcript owns it.
///
/// Session and run identifiers are deliberately separate: a session may host
/// multiple runs, while lifecycle correlation must name the exact run.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AgentRunRef {
    pub session_id: String,
    pub run_id: String,
}

/// Authoritative parent/child identity for one delegated run.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DelegatedRunLineage {
    pub parent: AgentRunRef,
    pub child: AgentRunRef,
}

/// The measurable boundaries of one parent/child join, in one place.
///
/// Three intervals fall out of these four points, and a run report that
/// carries only `completed_at_ms` and `joined_at_ms` cannot separate them:
///
/// - scheduler wait: `joined_at_ms - wait_started_at_ms`;
/// - collection lag: `joined_at_ms - completed_at_ms`;
/// - result processing: `result_processing_completed_at_ms -
///   result_processing_started_at_ms`.
///
/// The optional boundaries stay `Option` rather than defaulting to the join
/// instant, because a report that cannot distinguish "the parent never waited"
/// from "the parent waited zero milliseconds" is worse than one that says it
/// does not know. A path that never waited (`agent_start` without
/// `wait_for_terminal`) and a path that collected without collapsing a result
/// both project explicit nulls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegatedJoinBoundaries {
    /// When the parent began waiting on this child, if it ever did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_started_at_ms: Option<i64>,
    /// When the parent began collapsing the child's result, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_processing_started_at_ms: Option<i64>,
    /// When that collapse finished. Recorded even when it failed, so a failed
    /// collapse is a measured interval rather than a missing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_processing_completed_at_ms: Option<i64>,
}

impl DelegatedJoinBoundaries {
    /// How long the parent was blocked, from the moment it began waiting to the
    /// moment it observed the child terminal.
    ///
    /// `None` when the parent never waited. Also `None` when the clock ran
    /// backwards between the two reads, because a negative duration is a
    /// broken measurement, not a fast one.
    #[must_use]
    pub fn wait_ms(&self, joined_at_ms: i64) -> Option<u64> {
        u64::try_from(joined_at_ms.checked_sub(self.wait_started_at_ms?)?).ok()
    }

    /// How long the parent spent collapsing the child's result. `None` unless
    /// both boundaries were recorded.
    #[must_use]
    pub fn result_processing_ms(&self) -> Option<u64> {
        u64::try_from(
            self.result_processing_completed_at_ms?
                .checked_sub(self.result_processing_started_at_ms?)?,
        )
        .ok()
    }
}

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
/// `Spawned`/`Completed`/`Failed`/`Stopped`/`Cancelled` are the terminal-or-start
/// states. `Progressed` is fired on intermediate milestones (e.g. a
/// retriggerable worker resuming from `awaiting_input`, or a workflow
/// stage completing without ending the worker). `WaitingForInput` covers
/// retriggerable workers that finish a cycle but stay alive pending the
/// next host-supplied trigger payload. `Suspended`/`Resumed` cover
/// cooperative mid-loop pause and warm resume (harn#1836); the
/// `agent_loop` honors the pause signal at the next turn boundary,
/// distinct from a graceful `Stopped` handoff or hard `Cancelled` interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum WorkerEvent {
    WorkerSpawned,
    WorkerProgressed,
    WorkerWaitingForInput,
    WorkerSuspended,
    WorkerResumed,
    WorkerCompleted,
    WorkerFailed,
    WorkerStopped,
    WorkerCancelled,
}

impl WorkerEvent {
    /// The full set of `WorkerEvent` variants in canonical order. Mirrors
    /// the pattern used by `ToolCallStatus::ALL` so the protocol-artifact
    /// dumper can enumerate worker status wire values without
    /// special-casing each lifecycle event.
    pub const ALL: [Self; 9] = [
        Self::WorkerSpawned,
        Self::WorkerProgressed,
        Self::WorkerWaitingForInput,
        Self::WorkerSuspended,
        Self::WorkerResumed,
        Self::WorkerCompleted,
        Self::WorkerFailed,
        Self::WorkerStopped,
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
            Self::WorkerStopped => "stopped",
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
            Self::WorkerStopped => "WorkerStopped",
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
            Self::WorkerCompleted
                | Self::WorkerFailed
                | Self::WorkerStopped
                | Self::WorkerCancelled
        )
    }

    /// Interpret a persisted worker status through the lifecycle owner.
    /// `running` is represented by the spawn variant because both spawn and
    /// resume intentionally project to the same non-terminal wire state.
    pub fn from_status(status: &str) -> Option<Self> {
        match status {
            "running" => Some(Self::WorkerSpawned),
            "progressed" => Some(Self::WorkerProgressed),
            "awaiting_input" => Some(Self::WorkerWaitingForInput),
            "suspended" => Some(Self::WorkerSuspended),
            "completed" => Some(Self::WorkerCompleted),
            "failed" => Some(Self::WorkerFailed),
            "stopped" => Some(Self::WorkerStopped),
            "cancelled" | "canceled" => Some(Self::WorkerCancelled),
            _ => None,
        }
    }

    pub fn status_is_terminal(status: &str) -> bool {
        Self::from_status(status).is_some_and(Self::is_terminal)
    }
}
