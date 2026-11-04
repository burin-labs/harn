use crate::orchestration::{CapabilityPolicy, ProcessNetworkProxy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProcessNetworkGrant {
    Denied,
    Loopback { proxy: Option<ProcessNetworkProxy> },
    Public { proxy: Option<ProcessNetworkProxy> },
}

impl ProcessNetworkGrant {
    pub(super) fn for_policy(policy: &CapabilityPolicy) -> Self {
        if policy.process_sandbox.deny_network {
            return Self::Denied;
        }
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

    /// Linux has no filter that limits a socket's destination, so a child whose
    /// network stops at loopback or at the proxy needs a private namespace.
    #[cfg(target_os = "linux")]
    pub(super) fn needs_private_namespace(self) -> bool {
        matches!(
            self,
            Self::Loopback { .. } | Self::Public { proxy: Some(_) }
        )
    }
}

/// The one answer every backend renders. `deny_network` belongs here and not
/// only in [`ProcessNetworkGrant`], or the filters that read this would give a
/// child sockets while the grant reports none.
pub(crate) fn allows_network(policy: &CapabilityPolicy) -> bool {
    !policy.process_sandbox.deny_network && policy.allows_network()
}
