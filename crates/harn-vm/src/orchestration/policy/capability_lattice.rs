use super::CapabilityPolicy;

fn grants(policy: &CapabilityPolicy, capability: &str, operation: &str) -> bool {
    policy.capabilities.get(capability).is_some_and(|allowed| {
        allowed.is_empty() || allowed.iter().any(|candidate| candidate == operation)
    })
}

pub(crate) fn operation_is_covered(capability: &str, allowed: &str, requested: &str) -> bool {
    allowed == requested
        || (capability == "authority"
            && !allowed.contains('@')
            && requested
                .split_once('@')
                .is_some_and(|(access, _)| access == allowed))
}

pub(super) fn policy_allows_capability(
    policy: &CapabilityPolicy,
    capability: &str,
    operation: &str,
) -> bool {
    if !policy.capabilities_are_restricted() {
        return true;
    }
    if policy.capabilities_deny_all() {
        return false;
    }
    if policy.capabilities.get(capability).is_some_and(|allowed| {
        allowed.is_empty()
            || allowed
                .iter()
                .any(|candidate| operation_is_covered(capability, candidate, operation))
    }) {
        return true;
    }

    // Resource-scoped authority requires its exact operation unless the host
    // intentionally grants the broader access family.
    // Reading or listing a workspace already reveals the weaker existence
    // probe used by safe preflight paths.
    if capability == "workspace" && operation == "exists" {
        return grants(policy, "workspace", "read_text") || grants(policy, "workspace", "list");
    }

    // An LLM call necessarily resolves the catalog first.
    capability == "llm" && operation == "catalog" && grants(policy, "llm", "call")
}
