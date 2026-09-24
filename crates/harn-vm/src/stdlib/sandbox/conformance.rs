//! The sandbox conformance contract.
//!
//! Each operating system renders a capability policy with its own kernel
//! mechanism, and each backend was built separately. Without a shared statement
//! of what confinement means, a backend difference surfaces only as a red on one
//! platform, and a host that cannot enforce anything passes every test that
//! expects a refusal. This module is that statement: the behaviors every
//! backend must render, the route a child takes to reach them, and what the
//! caller observes in each case. A conformance suite runs every case against
//! the live backend and judges it here.
//!
//! Three rules the contract holds across all cases:
//!
//! - An expectation is derived from what the backend itself declares (for
//!   example [`unix_socket_enforcement`]), so a receipt and a behavior cannot
//!   disagree without a case failing.
//! - A refusal with no admitted sibling proves nothing, so the suite carries
//!   admission cases next to refusal cases and an over-refusal fails too.
//! - A host that cannot enforce a case is `not_measured`, never a pass, and it
//!   must show the escape actually happening. A detector stuck on
//!   "unavailable" that still observes a refusal is a contradiction and fails.

use serde::Serialize;

use crate::orchestration::{CapabilityPolicy, UnixSocketEnforcement};

use crate::stdlib::sandbox::unix_socket_enforcement;

/// One behavior every backend must render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceCase {
    /// A write inside the workspace lands. The liveness leg for the write
    /// refusals: a backend that refused every write would pass those.
    WorkspaceWriteAdmitted,
    /// A write outside every writable root is refused.
    OutsideWriteRefused,
    /// A read of a file outside every readable root is refused.
    OutsideReadRefused,
    /// A child re-created by the process-owner guardian is confined like a
    /// direct child. The guardian rebuilds the payload from serialized data,
    /// and anything that did not survive the handover is dropped silently.
    GuardianOutsideWriteRefused,
    /// A name present in the launcher's environment and not declared by the
    /// session does not reach the child.
    UndeclaredEnvironmentNameWithheld,
    /// The same, for a child re-created by the process-owner guardian.
    GuardianUndeclaredEnvironmentNameWithheld,
    /// A Unix socket file binds under a named socket root on a policy that
    /// does not permit networking.
    UnixSocketBindUnderRoot,
    /// A Unix socket file binds under a named socket root that lies outside
    /// every writable root, on a policy that also permits networking. The
    /// network grant must not narrow the path grant.
    UnixSocketBindUnderRootWithNetwork,
    /// A Unix socket file is refused outside every socket root and every
    /// writable root. The negative control for the two binds above: without
    /// it, a blanket grant would pass them.
    UnixSocketBindOutsideRootRefused,
}

impl ConformanceCase {
    /// Every case, in the order the suite reports them.
    pub const ALL: &'static [ConformanceCase] = &[
        Self::WorkspaceWriteAdmitted,
        Self::OutsideWriteRefused,
        Self::OutsideReadRefused,
        Self::GuardianOutsideWriteRefused,
        Self::UndeclaredEnvironmentNameWithheld,
        Self::GuardianUndeclaredEnvironmentNameWithheld,
        Self::UnixSocketBindUnderRoot,
        Self::UnixSocketBindUnderRootWithNetwork,
        Self::UnixSocketBindOutsideRootRefused,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::WorkspaceWriteAdmitted => "fs.workspace_write_admitted",
            Self::OutsideWriteRefused => "fs.outside_write_refused",
            Self::OutsideReadRefused => "fs.outside_read_refused",
            Self::GuardianOutsideWriteRefused => "guardian.outside_write_refused",
            Self::UndeclaredEnvironmentNameWithheld => "env.undeclared_name_withheld",
            Self::GuardianUndeclaredEnvironmentNameWithheld => {
                "guardian.undeclared_env_name_withheld"
            }
            Self::UnixSocketBindUnderRoot => "unix_socket.bind_under_root",
            Self::UnixSocketBindUnderRootWithNetwork => "unix_socket.bind_under_root_with_network",
            Self::UnixSocketBindOutsideRootRefused => "unix_socket.bind_outside_root_refused",
        }
    }

    /// How the child is spawned.
    pub fn route(self) -> SpawnRoute {
        match self {
            Self::GuardianOutsideWriteRefused | Self::GuardianUndeclaredEnvironmentNameWithheld => {
                SpawnRoute::Guardian
            }
            _ => SpawnRoute::Direct,
        }
    }

    /// Whether the case measures a kernel boundary. A case that does can go
    /// unmeasured on a host without the mechanism; one that does not (the
    /// child's environment is built in user space) is measured everywhere.
    pub fn needs_filesystem_enforcement(self) -> bool {
        !matches!(
            self,
            Self::UndeclaredEnvironmentNameWithheld
                | Self::GuardianUndeclaredEnvironmentNameWithheld
        )
    }

    /// What the caller must observe, given the policy the case runs under.
    ///
    /// Socket cases read the backend's own declared disposition, so a backend
    /// that reports a grant it does not render fails here rather than in a
    /// build tool's permission error.
    pub fn expectation(self, policy: &CapabilityPolicy) -> Expectation {
        if self.route() == SpawnRoute::Guardian && !cfg!(unix) {
            return Expectation::NotApplicable(
                "no process-owner guardian on this platform: a process tree is contained \
                 with a Job Object and the command is never re-created",
            );
        }
        match self {
            Self::WorkspaceWriteAdmitted => Expectation::Observe(Observation::Admitted),
            Self::OutsideWriteRefused
            | Self::OutsideReadRefused
            | Self::GuardianOutsideWriteRefused
            | Self::UndeclaredEnvironmentNameWithheld
            | Self::GuardianUndeclaredEnvironmentNameWithheld => {
                Expectation::Observe(Observation::Refused)
            }
            Self::UnixSocketBindUnderRoot | Self::UnixSocketBindUnderRootWithNetwork => {
                match unix_socket_enforcement(policy) {
                    UnixSocketEnforcement::Refused => {
                        Expectation::Observe(Observation::SpawnRefused)
                    }
                    UnixSocketEnforcement::NotRequested => {
                        Expectation::Observe(Observation::Refused)
                    }
                    UnixSocketEnforcement::PathScoped
                    | UnixSocketEnforcement::ServeOnly
                    | UnixSocketEnforcement::SupersededByNetworkGrant => {
                        Expectation::Observe(Observation::Admitted)
                    }
                }
            }
            Self::UnixSocketBindOutsideRootRefused => match unix_socket_enforcement(policy) {
                UnixSocketEnforcement::Refused => Expectation::Observe(Observation::SpawnRefused),
                _ => Expectation::Observe(Observation::Refused),
            },
        }
    }
}

/// The path from a caller's request to the child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnRoute {
    /// The command is confined and spawned in this process.
    Direct,
    /// The command is serialized to the process-owner guardian, which
    /// re-creates it. Background and auto-backgrounded commands take this
    /// route on Unix.
    Guardian,
}

/// What the caller saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    /// The child ran and the operation took effect.
    Admitted,
    /// The child ran and the operation did not take effect.
    Refused,
    /// The spawn itself was refused, because the backend cannot render the
    /// policy and will not widen it.
    SpawnRefused,
}

/// What the contract requires for a case on this host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expectation {
    Observe(Observation),
    /// The case does not exist on this platform, and the reason says why.
    NotApplicable(&'static str),
}

/// The judgement on one case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Conforms,
    /// A refusal was required and the operation took effect.
    Escaped {
        target: String,
    },
    /// An admission was required and the operation was refused.
    Overrefused {
        target: String,
        observed: Observation,
    },
    /// The host cannot enforce this case, and the escape was observed to
    /// prove the detector is not stuck. Not a pass.
    NotMeasured {
        reason: String,
    },
    NotApplicable {
        reason: String,
    },
    /// The host reported no enforcement yet refused the operation. Either the
    /// detector is wrong or the probe is broken; neither is a measurement.
    Contradiction {
        target: String,
        reason: String,
    },
    /// The probe ran but its effect could not be read, so nothing was
    /// measured. A failure, because a probe that cannot see is how a
    /// boundary stops being tested without anyone noticing.
    ProbeBroken {
        reason: String,
    },
}

impl Verdict {
    /// Whether this verdict fails the suite.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Escaped { .. }
                | Self::Overrefused { .. }
                | Self::Contradiction { .. }
                | Self::ProbeBroken { .. }
        )
    }
}

/// Judge one observation against the contract.
///
/// `enforcing` is whether this host's backend reports it can enforce the
/// case. `target` names what the probe touched, so a failure says what
/// escaped rather than only that something did.
pub fn judge(
    case: ConformanceCase,
    expectation: Expectation,
    enforcing: bool,
    observed: Observation,
    target: &str,
) -> Verdict {
    let expected = match expectation {
        Expectation::NotApplicable(reason) => {
            return Verdict::NotApplicable {
                reason: reason.to_string(),
            }
        }
        Expectation::Observe(expected) => expected,
    };
    if !enforcing && case.needs_filesystem_enforcement() {
        return match observed {
            Observation::Admitted => Verdict::NotMeasured {
                reason: "the active backend reports no filesystem enforcement on this host, \
                         and the operation was observed to take effect"
                    .to_string(),
            },
            Observation::Refused | Observation::SpawnRefused => Verdict::Contradiction {
                target: target.to_string(),
                reason: format!(
                    "the active backend reports no filesystem enforcement, yet the operation \
                     was {observed:?} where {expected:?} was required"
                ),
            },
        };
    }
    if observed == expected {
        return Verdict::Conforms;
    }
    match (expected, observed) {
        (Observation::Refused | Observation::SpawnRefused, Observation::Admitted) => {
            Verdict::Escaped {
                target: target.to_string(),
            }
        }
        _ => Verdict::Overrefused {
            target: target.to_string(),
            observed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSE: Expectation = Expectation::Observe(Observation::Refused);
    const ADMIT: Expectation = Expectation::Observe(Observation::Admitted);

    /// The falsifier for the judge itself: an operation that took effect
    /// where a refusal was required fails, and names what escaped.
    #[test]
    fn an_admitted_operation_under_a_refusal_case_escapes_and_names_the_target() {
        let verdict = judge(
            ConformanceCase::OutsideWriteRefused,
            REFUSE,
            true,
            Observation::Admitted,
            "/outside/probe",
        );
        assert_eq!(
            verdict,
            Verdict::Escaped {
                target: "/outside/probe".to_string()
            }
        );
        assert!(verdict.is_failure());
    }

    #[test]
    fn a_refused_admission_case_fails_as_overrefusal() {
        let verdict = judge(
            ConformanceCase::WorkspaceWriteAdmitted,
            ADMIT,
            true,
            Observation::Refused,
            "/ws/probe",
        );
        assert!(verdict.is_failure(), "{verdict:?}");
    }

    /// A host with no enforcement never passes a refusal case.
    #[test]
    fn an_unenforcing_host_is_not_measured_and_never_conforms() {
        let verdict = judge(
            ConformanceCase::OutsideWriteRefused,
            REFUSE,
            false,
            Observation::Admitted,
            "/outside/probe",
        );
        assert!(
            matches!(verdict, Verdict::NotMeasured { .. }),
            "{verdict:?}"
        );
        assert!(!verdict.is_failure());
    }

    /// A detector stuck on "unavailable" that still observes a refusal cannot
    /// read as unmeasured, or it would hide a real boundary change.
    #[test]
    fn an_unenforcing_host_that_refuses_is_a_contradiction() {
        let verdict = judge(
            ConformanceCase::OutsideWriteRefused,
            REFUSE,
            false,
            Observation::Refused,
            "/outside/probe",
        );
        assert!(
            matches!(verdict, Verdict::Contradiction { .. }),
            "{verdict:?}"
        );
    }

    /// The environment is built in user space, so it is measured everywhere.
    #[test]
    fn environment_cases_are_measured_without_filesystem_enforcement() {
        let verdict = judge(
            ConformanceCase::UndeclaredEnvironmentNameWithheld,
            REFUSE,
            false,
            Observation::Admitted,
            "HARN_PROBE",
        );
        assert!(matches!(verdict, Verdict::Escaped { .. }), "{verdict:?}");
    }

    #[test]
    fn case_ids_are_unique() {
        let mut ids: Vec<_> = ConformanceCase::ALL.iter().map(|case| case.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ConformanceCase::ALL.len());
    }
}
