use serde::{Deserialize, Serialize};

use crate::orchestration::CapabilityPolicy;

use super::ProcessSandboxDenialReporting;

/// Network grants captured for one child, after host policy normalization.
/// A grant is permission, not evidence that a destination can be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProcessSandboxNetwork {
    Policy {
        direct_network: bool,
        tcp_loopback: bool,
        managed_proxy: bool,
    },
    Inherited,
    Unavailable,
    Unknown,
}

impl ProcessSandboxNetwork {
    pub(in crate::stdlib::sandbox) fn capture(
        reporting: ProcessSandboxDenialReporting,
        policy: Option<&CapabilityPolicy>,
        inherited: bool,
    ) -> Self {
        match (reporting, policy) {
            (ProcessSandboxDenialReporting::BackendUnavailable, _) => Self::Unavailable,
            (ProcessSandboxDenialReporting::InferredOnly, _) if inherited => Self::Inherited,
            (ProcessSandboxDenialReporting::InferredOnly, Some(policy)) => {
                let grant = super::super::policy::ProcessNetworkGrant::for_policy(policy);
                Self::Policy {
                    direct_network: grant.direct_network(),
                    tcp_loopback: grant.tcp_loopback(),
                    managed_proxy: grant.managed_proxy(),
                }
            }
            _ => Self::Unknown,
        }
    }
}

/// The sandbox metadata retained by command results and background handles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSandboxReport {
    pub kind: String,
    pub enforced: bool,
    pub denial_reporting: ProcessSandboxDenialReporting,
    pub network: ProcessSandboxNetwork,
    /// Id of the embedder spawn broker that ran this child outside the
    /// active confinement. Absent for every ordinary spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_policy_does_not_fabricate_network_grants() {
        use ProcessSandboxDenialReporting::{BackendUnavailable, InferredOnly, NotEnforced};
        for (reporting, inherited, expected) in [
            (
                BackendUnavailable,
                false,
                ProcessSandboxNetwork::Unavailable,
            ),
            (InferredOnly, true, ProcessSandboxNetwork::Inherited),
            (InferredOnly, false, ProcessSandboxNetwork::Unknown),
            (NotEnforced, false, ProcessSandboxNetwork::Unknown),
        ] {
            let report = ProcessSandboxNetwork::capture(reporting, None, inherited);
            assert_eq!(report, expected);
            let value = serde_json::to_value(report).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 1);
            assert!(value["status"].is_string());
        }
        let policy = CapabilityPolicy::default();
        assert_eq!(
            ProcessSandboxNetwork::capture(BackendUnavailable, Some(&policy), false),
            ProcessSandboxNetwork::Unavailable,
        );
    }

    #[test]
    fn inherited_confinement_does_not_claim_the_active_policy_grants() {
        let policy = CapabilityPolicy {
            side_effect_level: Some("network".into()),
            ..CapabilityPolicy::default()
        };
        let reporting = ProcessSandboxDenialReporting::InferredOnly;
        assert_eq!(
            ProcessSandboxNetwork::capture(reporting, Some(&policy), false),
            ProcessSandboxNetwork::Policy {
                direct_network: true,
                tcp_loopback: true,
                managed_proxy: false,
            },
        );
        let inherited = ProcessSandboxNetwork::capture(reporting, Some(&policy), true);
        assert_eq!(inherited, ProcessSandboxNetwork::Inherited);
        assert_eq!(
            serde_json::to_value(inherited).unwrap(),
            serde_json::json!({"status": "inherited"}),
        );
        assert_eq!(
            ProcessSandboxNetwork::capture(
                ProcessSandboxDenialReporting::BackendUnavailable,
                Some(&policy),
                true,
            ),
            ProcessSandboxNetwork::Unavailable,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn receipt_uses_the_normalized_policy_when_loopback_is_withheld() {
        for (ceiling, remaining_network) in [("process_exec", false), ("network", true)] {
            let mut requested = CapabilityPolicy {
                sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
                side_effect_level: Some(ceiling.into()),
                ..CapabilityPolicy::default()
            };
            requested.process_sandbox.allow_tcp_loopback = true;
            assert_eq!(
                ProcessSandboxNetwork::capture(
                    ProcessSandboxDenialReporting::InferredOnly,
                    Some(&requested),
                    false
                ),
                ProcessSandboxNetwork::Policy {
                    direct_network: false,
                    tcp_loopback: true,
                    managed_proxy: false
                },
            );
            crate::orchestration::push_execution_policy(requested);
            let (normalized, _) =
                crate::stdlib::sandbox::active_sandbox_policy().expect("active policy");
            crate::orchestration::pop_execution_policy();
            assert!(!normalized.process_sandbox.allow_tcp_loopback);
            assert_eq!(
                ProcessSandboxNetwork::capture(
                    ProcessSandboxDenialReporting::InferredOnly,
                    Some(&normalized),
                    false
                ),
                ProcessSandboxNetwork::Policy {
                    direct_network: remaining_network,
                    tcp_loopback: remaining_network,
                    managed_proxy: false
                },
            );
        }
    }
}
