use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

use super::{AuthorityRequirement, ProcessSocketKind, ProcessSocketRequirement};

pub(super) fn normalize_policy(policy: &mut CapabilityPolicy) {
    sort_dedup(&mut policy.tools);
    sort_dedup(&mut policy.workspace_roots);
    sort_dedup(&mut policy.read_only_roots);
    for operations in policy.capabilities.values_mut() {
        sort_dedup(operations);
    }
    sort_dedup(&mut policy.process_sandbox.read_roots);
    sort_dedup(&mut policy.process_sandbox.write_roots);
    sort_dedup(&mut policy.process_sandbox.unix_socket_roots);
    if let Some(presets) = policy.process_sandbox.presets.as_mut() {
        presets.sort();
        presets.dedup();
    }
}

fn sort_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

pub(super) fn policy_requirements(policy: &CapabilityPolicy) -> Vec<AuthorityRequirement> {
    let mut requirements = Vec::new();
    requirements.extend(
        policy
            .read_only_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::FilesystemRead { root }),
    );
    requirements.extend(
        policy
            .workspace_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::FilesystemWrite { root }),
    );
    requirements.extend(
        policy
            .process_sandbox
            .read_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::ProcessReadRoot { root }),
    );
    requirements.extend(
        policy
            .process_sandbox
            .write_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::ProcessWriteRoot { root }),
    );
    requirements.extend(
        policy
            .process_sandbox
            .unix_socket_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::ProcessUnixSocketRoot { root }),
    );
    if let Some(endpoint) = &policy.process_sandbox.ssh_agent_socket {
        requirements.push(AuthorityRequirement::ProcessSocket(
            ProcessSocketRequirement {
                socket_kind: ProcessSocketKind::SshAgent,
                endpoint: Some(endpoint.clone()),
            },
        ));
    }
    let presets = policy.process_sandbox.effective_presets();
    let presets = if presets.is_empty() {
        vec!["none".to_string()]
    } else {
        presets
            .into_iter()
            .map(preset_name)
            .map(str::to_string)
            .collect()
    };
    requirements.extend(
        presets
            .into_iter()
            .map(|preset| AuthorityRequirement::ProcessSandbox {
                profile: policy.sandbox_profile.as_str().to_string(),
                preset,
            }),
    );
    requirements.extend(
        policy
            .tools
            .iter()
            .cloned()
            .map(|pattern| AuthorityRequirement::Tool { pattern }),
    );
    for (capability, operations) in &policy.capabilities {
        let operations = if operations.is_empty() {
            vec!["*".to_string()]
        } else {
            operations.clone()
        };
        requirements.extend(operations.into_iter().map(|operation| {
            AuthorityRequirement::HostCapability {
                capability: capability.clone(),
                operation,
            }
        }));
    }
    if let Some(level) = &policy.side_effect_level {
        requirements.push(AuthorityRequirement::SideEffectCeiling {
            level: level.clone(),
        });
    }
    if let Some(depth) = policy.recursion_limit {
        requirements.push(AuthorityRequirement::RecursionLimit { depth });
    }
    requirements
}

fn preset_name(preset: ProcessSandboxPreset) -> &'static str {
    match preset {
        ProcessSandboxPreset::SystemRuntime => "system_runtime",
        ProcessSandboxPreset::DeveloperToolchains => "developer_toolchains",
        ProcessSandboxPreset::PackageManagerConfig => "package_manager_config",
        ProcessSandboxPreset::UserTemp => "user_temp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_agent_policy_requires_the_exact_host_socket_without_key_read_authority() {
        let mut policy = CapabilityPolicy::default();
        let before = policy_requirements(&policy);
        policy.process_sandbox.ssh_agent_socket = Some("/run/agent.sock".into());
        let after = policy_requirements(&policy);
        let added = after
            .iter()
            .filter(|item| !before.contains(item))
            .collect::<Vec<_>>();
        assert_eq!(
            added,
            vec![&AuthorityRequirement::ProcessSocket(
                ProcessSocketRequirement {
                    socket_kind: ProcessSocketKind::SshAgent,
                    endpoint: Some("/run/agent.sock".into()),
                }
            )]
        );
    }
}
