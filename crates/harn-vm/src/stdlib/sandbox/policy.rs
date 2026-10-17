use crate::orchestration::CapabilityPolicy;

pub(crate) fn allows_network(policy: &CapabilityPolicy) -> bool {
    policy.allows_network()
}
