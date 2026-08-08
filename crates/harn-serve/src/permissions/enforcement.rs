//! The runtime arm of the permission primitive: lowering a declared
//! [`PermissionPolicy`] into the types the runtime enforces against.
//!
//! A sandbox is the runtime answer to a permission policy. The policy
//! declares *what an agent may do*; this module turns that declaration
//! into the two enforcement vocabularies the rest of the stack already
//! understands:
//!
//! - [`CapabilityPolicy`] (`harn-vm`) — the in-VM ceiling consulted
//!   before every builtin dispatch and pushed onto the execution-policy
//!   stack. This is what makes `policy { read, write, exec }` gate tool
//!   calls live rather than only at request time.
//! - [`SandboxSpec`](harn_hostlib::sandbox::SandboxSpec) — the spec a
//!   [`SandboxBackend`](harn_hostlib::sandbox::SandboxBackend) provisions
//!   from, carrying the `net` allowlist as an egress policy. This is the
//!   process/filesystem/network isolation arm (the
//!   [`harn_hostlib::sandbox`] module).
//!
//! The `SandboxSpec` lowering is only available with the `hostlib`
//! feature, since it depends on `harn-hostlib`; the `CapabilityPolicy`
//! lowering is always available.

use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};

use super::policy::PermissionPolicy;

#[cfg(feature = "hostlib")]
pub use harn_hostlib::sandbox;

impl PermissionPolicy {
    /// The lowest [`side_effect_level`](CapabilityPolicy::side_effect_level)
    /// the policy needs to permit, derived from which action classes the
    /// policy grants. The ladder mirrors `harn-vm`'s ranking:
    /// `none < read_only < workspace_write < process_exec < network`.
    fn required_side_effect_level(&self) -> &'static str {
        if !self.net.is_empty() {
            "network"
        } else if !self.exec.is_empty() {
            "process_exec"
        } else if !self.write.is_empty() {
            "workspace_write"
        } else if !self.read.is_empty() {
            "read_only"
        } else {
            "none"
        }
    }

    /// Lower the policy into a [`CapabilityPolicy`] for in-VM
    /// enforcement. `workspace_roots` are the host directories the
    /// workload is confined to (the deployment supplies these; the
    /// policy globs constrain access *within* them). `sandbox_profile`
    /// selects the OS-confinement strength applied to spawned
    /// subprocesses.
    ///
    /// The coarse `capabilities`/`side_effect_level` ceiling produced
    /// here is the runtime backstop; fine-grained per-glob matching of a
    /// concrete request still flows through the permission store's
    /// `evaluate`.
    pub fn to_capability_policy(
        &self,
        sandbox_profile: SandboxProfile,
        workspace_roots: Vec<String>,
    ) -> CapabilityPolicy {
        let mut capabilities = std::collections::BTreeMap::new();

        let mut workspace = Vec::new();
        if !self.read.is_empty() {
            workspace.extend(["read_text", "list", "exists"].map(String::from));
        }
        if !self.write.is_empty() {
            workspace.extend(["write_text", "delete"].map(String::from));
        }
        if !workspace.is_empty() {
            capabilities.insert("workspace".to_string(), workspace);
        }
        if !self.exec.is_empty() {
            capabilities.insert("process".to_string(), vec!["exec".to_string()]);
        }

        CapabilityPolicy {
            capabilities,
            workspace_roots,
            side_effect_level: Some(self.required_side_effect_level().to_string()),
            sandbox_profile,
            ..Default::default()
        }
    }

    /// Lower the policy's `net` allowlist into an egress
    /// [`NetworkPolicy`](harn_hostlib::sandbox::NetworkPolicy). An empty
    /// allowlist denies all egress; a wildcard entry (`*` or `**`)
    /// lifts the restriction; anything else is treated as a host
    /// allowlist.
    #[cfg(feature = "hostlib")]
    pub fn to_network_policy(&self) -> sandbox::NetworkPolicy {
        if self.net.iter().any(|host| host == "*" || host == "**") {
            sandbox::NetworkPolicy::Unrestricted
        } else {
            sandbox::NetworkPolicy::Limited {
                allowed_hosts: self.net.clone(),
            }
        }
    }

    /// Lower the policy into a [`SandboxSpec`](harn_hostlib::sandbox::SandboxSpec)
    /// for a [`SandboxBackend`](harn_hostlib::sandbox::SandboxBackend) to
    /// provision. The spec carries the policy's egress allowlist; mounts,
    /// resource limits, and labels are deployment concerns the caller
    /// fills in (the policy does not declare host paths).
    #[cfg(feature = "hostlib")]
    pub fn to_sandbox_spec(&self) -> sandbox::SandboxSpec {
        sandbox::SandboxSpec {
            network_policy: self.to_network_policy(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dev_lowers_to_process_exec_capability_policy() {
        let policy = PermissionPolicy::local_dev();
        let cap =
            policy.to_capability_policy(SandboxProfile::OsHardened, vec!["/work".to_string()]);

        assert_eq!(cap.side_effect_level.as_deref(), Some("process_exec"));
        assert_eq!(cap.sandbox_profile, SandboxProfile::OsHardened);
        assert_eq!(cap.workspace_roots, vec!["/work".to_string()]);
        assert_eq!(
            cap.capabilities.get("process"),
            Some(&vec!["exec".to_string()])
        );
        let workspace = cap.capabilities.get("workspace").expect("workspace caps");
        assert!(workspace.contains(&"read_text".to_string()));
        assert!(workspace.contains(&"write_text".to_string()));
    }

    #[test]
    fn read_only_policy_grants_only_read_capabilities() {
        let policy = PermissionPolicy {
            read: vec!["src/**".to_string()],
            ..PermissionPolicy::empty()
        };
        let cap = policy.to_capability_policy(SandboxProfile::Worktree, Vec::new());

        assert_eq!(cap.side_effect_level.as_deref(), Some("read_only"));
        assert_eq!(cap.capabilities.get("process"), None);
        let workspace = cap.capabilities.get("workspace").expect("workspace caps");
        assert!(workspace.contains(&"read_text".to_string()));
        assert!(!workspace.contains(&"write_text".to_string()));
    }

    #[test]
    fn empty_policy_lowers_to_none_level_with_no_capabilities() {
        let cap =
            PermissionPolicy::empty().to_capability_policy(SandboxProfile::Worktree, Vec::new());
        assert_eq!(cap.side_effect_level.as_deref(), Some("none"));
        assert!(cap.capabilities.is_empty());
    }

    #[cfg(feature = "hostlib")]
    #[test]
    fn net_allowlist_lowers_to_egress_policy() {
        use sandbox::NetworkPolicy;

        let deny_all = PermissionPolicy::empty().to_network_policy();
        assert_eq!(
            deny_all,
            NetworkPolicy::Limited {
                allowed_hosts: Vec::new()
            }
        );

        let allowlist = PermissionPolicy {
            net: vec!["api.github.com".to_string()],
            ..PermissionPolicy::empty()
        }
        .to_network_policy();
        assert_eq!(
            allowlist,
            NetworkPolicy::Limited {
                allowed_hosts: vec!["api.github.com".to_string()]
            }
        );

        let wildcard = PermissionPolicy {
            net: vec!["*".to_string()],
            ..PermissionPolicy::empty()
        }
        .to_network_policy();
        assert_eq!(wildcard, NetworkPolicy::Unrestricted);
    }

    #[cfg(feature = "hostlib")]
    #[test]
    fn sandbox_spec_carries_egress_policy() {
        let spec = PermissionPolicy {
            net: vec!["api.github.com".to_string()],
            ..PermissionPolicy::empty()
        }
        .to_sandbox_spec();
        assert_eq!(
            spec.network_policy,
            sandbox::NetworkPolicy::Limited {
                allowed_hosts: vec!["api.github.com".to_string()]
            }
        );
    }
}
