//! Shared agent/run lifecycle registry (harn#6049).
//!
//! One compile-time owner for the coarse agent/run/worker status vocabulary
//! that crosses runtime reducers, session persistence, replay, ACP, A2A, and
//! protocol artifacts. Tool-call, task-list, provider-job, session-retention,
//! and host-lease states stay in their own owners — this module is only the
//! shared agent/run meaning.
//!
//! Projections:
//! - `WorkerEvent` maps 1:1 onto [`AgentLifecycleEvent`] (minus join);
//! - protocol dumps enumerate [`AgentLifecycleState::ALL`];
//! - adapters may map overlapping A2A task states through
//!   [`AgentLifecycleState::a2a_task_state`].

use serde::{Deserialize, Serialize};

/// Coarse agent/run lifecycle state. Wire names are stable; aliases parse
/// into these variants without becoming new canonical states.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleState {
    /// Active execution (spawned or resumed).
    Running,
    /// Non-terminal milestone while still active.
    Progressed,
    /// Retriggerable park awaiting the next host trigger payload.
    AwaitingInput,
    /// Cooperative mid-loop park; resumable.
    Suspended,
    /// Natural successful completion.
    Completed,
    /// Terminal failure.
    Failed,
    /// Graceful stop with typed handoff.
    Stopped,
    /// Hard cancel / abort.
    Cancelled,
}

/// Lifecycle event that advances [`AgentLifecycle`].
///
/// Worker bridge events are a strict subset; [`Self::Joined`] records that a
/// parent observed a terminal delegated worker and sealed join evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AgentLifecycleEvent {
    Spawned,
    Progressed,
    WaitingForInput,
    Suspended,
    Resumed,
    Completed,
    Failed,
    Stopped,
    Cancelled,
    /// Parent recorded join boundaries against an already-terminal child.
    Joined,
}

/// Projection metadata published for schemas and docs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentLifecycleProjection {
    pub wire_name: &'static str,
    pub terminal: bool,
    /// Cooperative park that may later resume without starting a new run.
    pub resumable: bool,
    /// Overlapping A2A `TaskState` wire value, when one exists.
    pub a2a_task_state: Option<&'static str>,
    /// Run-record / report status projection (same as wire for this vocabulary).
    pub run_record_status: &'static str,
}

/// Why a lifecycle transition was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleTransitionError {
    pub from: Option<AgentLifecycleState>,
    pub event: AgentLifecycleEvent,
    pub reason: &'static str,
}

impl std::fmt::Display for LifecycleTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.from {
            Some(from) => write!(
                f,
                "invalid agent lifecycle transition from {} via {:?}: {}",
                from.wire_name(),
                self.event,
                self.reason
            ),
            None => write!(
                f,
                "invalid agent lifecycle transition from <unstarted> via {:?}: {}",
                self.event, self.reason
            ),
        }
    }
}

impl std::error::Error for LifecycleTransitionError {}

/// Reducer state for one agent/run/worker lifecycle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentLifecycle {
    state: Option<AgentLifecycleState>,
    joined: bool,
}

impl AgentLifecycleState {
    pub const ALL: [Self; 8] = [
        Self::Running,
        Self::Progressed,
        Self::AwaitingInput,
        Self::Suspended,
        Self::Completed,
        Self::Failed,
        Self::Stopped,
        Self::Cancelled,
    ];

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Progressed => "progressed",
            Self::AwaitingInput => "awaiting_input",
            Self::Suspended => "suspended",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Stopped | Self::Cancelled
        )
    }

    /// Suspended work may resume; awaiting-input is also resumable by trigger.
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Suspended | Self::AwaitingInput)
    }

    /// Explicit compatibility aliases accepted by [`Self::from_wire`].
    /// These never appear in [`Self::ALL`] wire projections.
    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::AwaitingInput => &["awaiting"],
            Self::Completed => &["done", "succeeded", "success", "ok"],
            Self::Failed => &["error", "errored", "timeout", "timed_out"],
            Self::Cancelled => &["canceled", "aborted"],
            Self::Running | Self::Progressed | Self::Suspended | Self::Stopped => &[],
        }
    }

    pub const fn projection(self) -> AgentLifecycleProjection {
        AgentLifecycleProjection {
            wire_name: self.wire_name(),
            terminal: self.is_terminal(),
            resumable: self.is_resumable(),
            a2a_task_state: self.a2a_task_state(),
            run_record_status: self.wire_name(),
        }
    }

    /// Overlapping A2A task-state projection. A2A-only states
    /// (`submitted`, `auth-required`, `rejected`) stay adapter-local.
    pub const fn a2a_task_state(self) -> Option<&'static str> {
        match self {
            Self::Running | Self::Progressed => Some("working"),
            Self::AwaitingInput => Some("input-required"),
            // Protocol contribution: docs/src/protocol-contributions/a2a-paused-state.md
            Self::Suspended => Some("paused"),
            Self::Completed => Some("completed"),
            Self::Failed => Some("failed"),
            Self::Cancelled | Self::Stopped => Some("cancelled"),
        }
    }

    pub fn from_wire(status: &str) -> Option<Self> {
        let trimmed = status.trim();
        for &state in &Self::ALL {
            if state.wire_name() == trimmed {
                return Some(state);
            }
            if state.aliases().iter().any(|alias| *alias == trimmed) {
                return Some(state);
            }
        }
        None
    }

    pub fn status_is_terminal(status: &str) -> bool {
        Self::from_wire(status).is_some_and(Self::is_terminal)
    }

    /// Normalize a status string to the canonical wire name when recognized.
    pub fn canonicalize(status: &str) -> Option<&'static str> {
        Self::from_wire(status).map(Self::wire_name)
    }
}

impl AgentLifecycleEvent {
    pub const ALL: [Self; 10] = [
        Self::Spawned,
        Self::Progressed,
        Self::WaitingForInput,
        Self::Suspended,
        Self::Resumed,
        Self::Completed,
        Self::Failed,
        Self::Stopped,
        Self::Cancelled,
        Self::Joined,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "Spawned",
            Self::Progressed => "Progressed",
            Self::WaitingForInput => "WaitingForInput",
            Self::Suspended => "Suspended",
            Self::Resumed => "Resumed",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Cancelled => "Cancelled",
            Self::Joined => "Joined",
        }
    }

    /// State this event installs when the transition is accepted.
    /// [`Self::Joined`] does not change status — only the join flag.
    pub const fn target_state(self) -> Option<AgentLifecycleState> {
        match self {
            Self::Spawned | Self::Resumed => Some(AgentLifecycleState::Running),
            Self::Progressed => Some(AgentLifecycleState::Progressed),
            Self::WaitingForInput => Some(AgentLifecycleState::AwaitingInput),
            Self::Suspended => Some(AgentLifecycleState::Suspended),
            Self::Completed => Some(AgentLifecycleState::Completed),
            Self::Failed => Some(AgentLifecycleState::Failed),
            Self::Stopped => Some(AgentLifecycleState::Stopped),
            Self::Cancelled => Some(AgentLifecycleState::Cancelled),
            Self::Joined => None,
        }
    }
}

impl AgentLifecycle {
    pub const fn new() -> Self {
        Self {
            state: None,
            joined: false,
        }
    }

    pub const fn state(&self) -> Option<AgentLifecycleState> {
        self.state
    }

    pub const fn joined(&self) -> bool {
        self.joined
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(self.state, Some(state) if state.is_terminal())
    }

    /// Apply one lifecycle event, enforcing the shared transition table.
    pub fn apply(&mut self, event: AgentLifecycleEvent) -> Result<(), LifecycleTransitionError> {
        match event {
            AgentLifecycleEvent::Joined => self.apply_join(),
            _ => self.apply_status_event(event),
        }
    }

    /// Replay an event sequence deterministically. Same inputs always yield
    /// the same terminal state or the same rejection.
    pub fn replay(
        events: impl IntoIterator<Item = AgentLifecycleEvent>,
    ) -> Result<Self, LifecycleTransitionError> {
        let mut life = Self::new();
        for event in events {
            life.apply(event)?;
        }
        Ok(life)
    }

    fn apply_join(&mut self) -> Result<(), LifecycleTransitionError> {
        match self.state {
            Some(state) if state.is_terminal() => {
                self.joined = true;
                Ok(())
            }
            from => Err(LifecycleTransitionError {
                from,
                event: AgentLifecycleEvent::Joined,
                reason: "join requires a terminal lifecycle state",
            }),
        }
    }

    fn apply_status_event(
        &mut self,
        event: AgentLifecycleEvent,
    ) -> Result<(), LifecycleTransitionError> {
        let target = event.target_state().expect("status events have targets");
        match self.state {
            None => {
                if matches!(event, AgentLifecycleEvent::Spawned) {
                    self.state = Some(AgentLifecycleState::Running);
                    Ok(())
                } else {
                    Err(LifecycleTransitionError {
                        from: None,
                        event,
                        reason: "unstarted lifecycle accepts only Spawned",
                    })
                }
            }
            Some(current) if current.is_terminal() => {
                if self.joined {
                    return Err(LifecycleTransitionError {
                        from: Some(current),
                        event,
                        reason: "joined terminal lifecycle rejects further status events",
                    });
                }
                if current == target {
                    // Duplicate terminal of the same kind is idempotent.
                    Ok(())
                } else if target.is_terminal() {
                    Err(LifecycleTransitionError {
                        from: Some(current),
                        event,
                        reason: "conflicting terminal event",
                    })
                } else {
                    Err(LifecycleTransitionError {
                        from: Some(current),
                        event,
                        reason: "terminal lifecycle rejects non-terminal events",
                    })
                }
            }
            Some(current) => {
                if !may_transition(current, event) {
                    return Err(LifecycleTransitionError {
                        from: Some(current),
                        event,
                        reason: "transition not permitted",
                    });
                }
                self.state = Some(target);
                Ok(())
            }
        }
    }
}

fn may_transition(from: AgentLifecycleState, event: AgentLifecycleEvent) -> bool {
    use AgentLifecycleEvent as E;
    use AgentLifecycleState as S;
    match (from, event) {
        // Active / progressed share the same outbound edges.
        (S::Running | S::Progressed, E::Spawned | E::Resumed | E::Progressed) => true,
        (S::Running | S::Progressed, E::WaitingForInput | E::Suspended) => true,
        (S::Running | S::Progressed, E::Completed | E::Failed | E::Stopped | E::Cancelled) => true,

        // Retriggerable park.
        (S::AwaitingInput, E::WaitingForInput) => true,
        (S::AwaitingInput, E::Progressed | E::Resumed | E::Spawned) => true,
        (S::AwaitingInput, E::Suspended) => true,
        (S::AwaitingInput, E::Completed | E::Failed | E::Stopped | E::Cancelled) => true,

        // Cooperative suspend: idempotent park, resume, or abandon.
        (S::Suspended, E::Suspended) => true,
        (S::Suspended, E::Resumed | E::Spawned) => true,
        (S::Suspended, E::Failed | E::Stopped | E::Cancelled) => true,

        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_names() -> Vec<&'static str> {
        AgentLifecycleState::ALL
            .iter()
            .map(|state| state.wire_name())
            .collect()
    }

    #[test]
    fn canonical_wire_names_are_unique_and_stable() {
        let names = wire_names();
        assert_eq!(
            names,
            vec![
                "running",
                "progressed",
                "awaiting_input",
                "suspended",
                "completed",
                "failed",
                "stopped",
                "cancelled",
            ]
        );
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn aliases_parse_without_becoming_canonical_states() {
        assert_eq!(
            AgentLifecycleState::from_wire("awaiting"),
            Some(AgentLifecycleState::AwaitingInput)
        );
        assert_eq!(
            AgentLifecycleState::from_wire("canceled"),
            Some(AgentLifecycleState::Cancelled)
        );
        assert_eq!(
            AgentLifecycleState::from_wire("done"),
            Some(AgentLifecycleState::Completed)
        );
        assert_eq!(
            AgentLifecycleState::canonicalize("awaiting"),
            Some("awaiting_input")
        );
        assert_eq!(
            AgentLifecycleState::canonicalize("aborted"),
            Some("cancelled")
        );
        for &state in &AgentLifecycleState::ALL {
            for alias in state.aliases() {
                assert!(
                    !AgentLifecycleState::ALL
                        .iter()
                        .any(|canonical| canonical.wire_name() == *alias),
                    "alias `{alias}` must not be a canonical wire name"
                );
            }
        }
    }

    #[test]
    fn terminal_and_resumable_classification() {
        for state in [
            AgentLifecycleState::Completed,
            AgentLifecycleState::Failed,
            AgentLifecycleState::Stopped,
            AgentLifecycleState::Cancelled,
        ] {
            assert!(state.is_terminal());
            assert!(!state.is_resumable());
        }
        assert!(AgentLifecycleState::Suspended.is_resumable());
        assert!(AgentLifecycleState::AwaitingInput.is_resumable());
        assert!(!AgentLifecycleState::Running.is_terminal());
        assert!(!AgentLifecycleState::Progressed.is_resumable());
    }

    #[test]
    fn normal_completion_path() {
        let life = AgentLifecycle::replay([
            AgentLifecycleEvent::Spawned,
            AgentLifecycleEvent::Progressed,
            AgentLifecycleEvent::Completed,
        ])
        .expect("completion path");
        assert_eq!(life.state(), Some(AgentLifecycleState::Completed));
        assert!(!life.joined());
    }

    #[test]
    fn failure_and_cancellation_paths() {
        let failed =
            AgentLifecycle::replay([AgentLifecycleEvent::Spawned, AgentLifecycleEvent::Failed])
                .unwrap();
        assert_eq!(failed.state(), Some(AgentLifecycleState::Failed));

        let cancelled =
            AgentLifecycle::replay([AgentLifecycleEvent::Spawned, AgentLifecycleEvent::Cancelled])
                .unwrap();
        assert_eq!(cancelled.state(), Some(AgentLifecycleState::Cancelled));
    }

    #[test]
    fn suspend_resume_round_trip() {
        let life = AgentLifecycle::replay([
            AgentLifecycleEvent::Spawned,
            AgentLifecycleEvent::Suspended,
            AgentLifecycleEvent::Suspended, // idempotent
            AgentLifecycleEvent::Resumed,
            AgentLifecycleEvent::Completed,
        ])
        .unwrap();
        assert_eq!(life.state(), Some(AgentLifecycleState::Completed));
    }

    #[test]
    fn duplicate_terminal_is_idempotent_conflict_is_rejected() {
        let mut life =
            AgentLifecycle::replay([AgentLifecycleEvent::Spawned, AgentLifecycleEvent::Completed])
                .unwrap();
        life.apply(AgentLifecycleEvent::Completed)
            .expect("duplicate completed");
        let err = life
            .apply(AgentLifecycleEvent::Failed)
            .expect_err("conflicting terminal");
        assert_eq!(err.reason, "conflicting terminal event");

        let mut life =
            AgentLifecycle::replay([AgentLifecycleEvent::Spawned, AgentLifecycleEvent::Completed])
                .unwrap();
        let err = life
            .apply(AgentLifecycleEvent::Progressed)
            .expect_err("out-of-order non-terminal");
        assert_eq!(err.reason, "terminal lifecycle rejects non-terminal events");
    }

    #[test]
    fn delegated_worker_join_requires_terminal_and_is_idempotent() {
        let err =
            AgentLifecycle::replay([AgentLifecycleEvent::Spawned, AgentLifecycleEvent::Joined])
                .expect_err("join before terminal");
        assert_eq!(err.reason, "join requires a terminal lifecycle state");

        let mut life = AgentLifecycle::replay([
            AgentLifecycleEvent::Spawned,
            AgentLifecycleEvent::Completed,
            AgentLifecycleEvent::Joined,
        ])
        .unwrap();
        assert!(life.joined());
        life.apply(AgentLifecycleEvent::Joined)
            .expect("duplicate join");
        assert!(life.joined());
        let err = life
            .apply(AgentLifecycleEvent::Completed)
            .expect_err("status after join");
        assert_eq!(
            err.reason,
            "joined terminal lifecycle rejects further status events"
        );
    }

    #[test]
    fn replay_is_deterministic() {
        let events = [
            AgentLifecycleEvent::Spawned,
            AgentLifecycleEvent::WaitingForInput,
            AgentLifecycleEvent::Progressed,
            AgentLifecycleEvent::Suspended,
            AgentLifecycleEvent::Resumed,
            AgentLifecycleEvent::Stopped,
            AgentLifecycleEvent::Joined,
        ];
        let a = AgentLifecycle::replay(events).unwrap();
        let b = AgentLifecycle::replay(events).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.state(), Some(AgentLifecycleState::Stopped));
        assert!(a.joined());

        let invalid = [
            AgentLifecycleEvent::Spawned,
            AgentLifecycleEvent::Completed,
            AgentLifecycleEvent::Cancelled,
        ];
        let err_a = AgentLifecycle::replay(invalid).unwrap_err();
        let err_b = AgentLifecycle::replay(invalid).unwrap_err();
        assert_eq!(err_a, err_b);
    }

    #[test]
    fn projections_expose_protocol_metadata() {
        let suspended = AgentLifecycleState::Suspended.projection();
        assert!(suspended.resumable);
        assert!(!suspended.terminal);
        assert_eq!(suspended.a2a_task_state, Some("paused"));
        assert_eq!(suspended.run_record_status, "suspended");

        let cancelled = AgentLifecycleState::Cancelled.projection();
        assert!(cancelled.terminal);
        assert_eq!(cancelled.a2a_task_state, Some("cancelled"));
    }
}
