use crate::orchestration::CapabilityPolicy;

pub(crate) fn allows_network(policy: &CapabilityPolicy) -> bool {
    use crate::tool_annotations::SideEffectLevel;
    policy
        .side_effect_level
        .as_ref()
        .map(|level| SideEffectLevel::rank_str(level) >= SideEffectLevel::Network.rank())
        .unwrap_or(true)
}
