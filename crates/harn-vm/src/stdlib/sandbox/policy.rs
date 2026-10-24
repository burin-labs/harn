use crate::orchestration::{CapabilityPolicy, ProcessNetworkProxy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProcessNetworkGrant {
    Denied,
    Loopback { proxy: Option<ProcessNetworkProxy> },
    Public { proxy: Option<ProcessNetworkProxy> },
}

impl ProcessNetworkGrant {
    pub(super) fn for_policy(policy: &CapabilityPolicy) -> Self {
        // An explicit child loopback grant is narrower than the host tool ceiling.
        if policy.process_sandbox.allow_tcp_loopback {
            Self::Loopback {
                proxy: policy.process_network_proxy,
            }
        } else if allows_network(policy) {
            Self::Public {
                proxy: policy.process_network_proxy,
            }
        } else {
            Self::Denied
        }
    }

    pub(super) fn direct_network(self) -> bool {
        matches!(self, Self::Public { proxy: None })
    }

    pub(super) fn tcp_loopback(self) -> bool {
        matches!(self, Self::Loopback { .. } | Self::Public { proxy: None })
    }

    pub(super) fn managed_proxy(self) -> bool {
        matches!(
            self,
            Self::Loopback { proxy: Some(_) } | Self::Public { proxy: Some(_) }
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) fn loopback_only(self) -> bool {
        matches!(self, Self::Loopback { .. })
    }
}

pub(crate) fn allows_network(policy: &CapabilityPolicy) -> bool {
    policy.allows_network()
}
