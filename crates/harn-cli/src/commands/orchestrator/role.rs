use super::errors::OrchestratorError;
use std::path::Path;

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OrchestratorRole {
    SingleTenant,
    MultiTenant,
}

impl OrchestratorRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SingleTenant => "single-tenant",
            Self::MultiTenant => "multi-tenant",
        }
    }

    /// Describe how trigger/connector registries are partitioned under this
    /// role. This string is printed in the startup banner, so it must describe
    /// what `build_vm` actually constructs.
    ///
    /// Multi-tenant said "per-tenant registries" here for as long as `build_vm`
    /// had a single shared arm for both roles, which made the banner assert an
    /// isolation boundary the process had never built. Tenant isolation is
    /// real, but it lives at ingress, event-log topics, and secret namespaces —
    /// not in the registry, which stays shared until `build_vm` grows a
    /// per-tenant arm.
    pub(crate) fn registry_mode(self) -> &'static str {
        match self {
            Self::SingleTenant | Self::MultiTenant => "one shared trigger/connector registry",
        }
    }

    /// The isolation this role's name implies but does not yet deliver, or
    /// `None` when the role is fully backed.
    ///
    /// A deployer picks `--role multi-tenant` to get tenant isolation, and most
    /// of it arrives: ingress resolves an API key or path tenant, unknown keys
    /// get 403, suspended tenants get 402, event-log topics are tenant-prefixed,
    /// and secret lookups are rescoped per tenant with cross-tenant ids denied.
    /// What does not arrive is registry partitioning — every tenant shares one
    /// trigger/connector registry, because `build_vm` builds one VM per process
    /// regardless of role.
    ///
    /// That gap is worth saying out loud rather than leaving to a reader of
    /// `build_vm`: the role is usable, but it is not the whole boundary its name
    /// suggests, and a deployer sizing a blast radius needs to know which half
    /// they got.
    pub(crate) fn unproven_isolation(self) -> Option<&'static str> {
        match self {
            Self::SingleTenant => None,
            Self::MultiTenant => Some(
                "multi-tenant does not partition the trigger/connector registry: \
                 every tenant shares one registry in this process. Tenant isolation \
                 covers ingress, event-log topics, and secret namespaces only. \
                 See https://github.com/burin-labs/harn/issues/6792",
            ),
        }
    }

    /// Build the orchestrator VM with its persistent state rooted at
    /// `state_dir`.
    ///
    /// `state_dir` is passed to each registrar directly rather than exported as
    /// `HARN_STATE_DIR`. It used to be exported, which made an orchestrator's
    /// configured state dir the answer every *other* component in the process
    /// got from `runtime_paths::state_root()` — including ones handed an
    /// explicit base dir, since an absolute env value replaces that argument
    /// outright. That is wrong on three counts: this function is a reusable
    /// builder (a config reload calls it again, and a multi-tenant host may
    /// have several VMs), `set_var` races concurrent readers in any threaded
    /// process, and the export outlived the VM it was meant to configure.
    pub(crate) fn build_vm(
        self,
        workspace_root: &Path,
        source_dir: &Path,
        state_dir: &Path,
    ) -> Result<harn_vm::Vm, OrchestratorError> {
        match self {
            Self::SingleTenant | Self::MultiTenant => {
                // `register_store_builtins` would install this lazily off the
                // ambient state root; the orchestrator knows its own, so it
                // installs the log itself before the registrars run.
                harn_vm::event_log::install_default_at_state_root(state_dir)
                    .map_err(|error| OrchestratorError::from(error.to_string()))?;
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                crate::install_default_hostlib(&mut vm);
                harn_vm::register_persistent_state_builtins_at_root(
                    &mut vm,
                    workspace_root,
                    harn_vm::PersistentStateRoot::new(state_dir),
                    "orchestrator",
                );
                vm.set_project_root(workspace_root);
                vm.set_source_dir(source_dir);
                Ok(vm)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::event_log::EventLog as _;

    /// `build_vm` must route its state dir through arguments only.
    ///
    /// Exporting it instead pointed every `state_root()` caller in the process
    /// at this orchestrator's directory — including callers that passed their
    /// own base dir, which an absolute env value overrides. The visible symptom
    /// was unrelated in-process tests reading each other's event logs; the
    /// same aliasing applies to any host that builds a VM beside other work.
    #[tokio::test]
    async fn build_vm_roots_state_without_exporting_the_state_dir() {
        let _guard = crate::tests::common::harn_state_lock::lock_harn_state_async().await;
        harn_vm::event_log::reset_active_event_log();

        let workspace = tempfile::TempDir::new().unwrap();
        let state_dir = workspace.path().join("orchestrator-state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let _vm = OrchestratorRole::SingleTenant
            .build_vm(workspace.path(), workspace.path(), &state_dir)
            .expect("build orchestrator vm");

        assert_eq!(
            std::env::var(harn_vm::runtime_paths::HARN_STATE_DIR_ENV).ok(),
            None,
            "build_vm must not publish its state dir to the process environment",
        );

        // The state dir still wins for the orchestrator's own log, which is the
        // contract `--state-dir` documents.
        let location = harn_vm::event_log::active_event_log()
            .expect("build_vm installs the active event log")
            .describe()
            .location;
        assert_eq!(
            location.as_deref(),
            Some(state_dir.join("events.sqlite").as_path()),
            "event log must be rooted at the configured state dir",
        );

        harn_vm::event_log::reset_active_event_log();
    }

    /// The banner must not promise an isolation boundary `build_vm` never
    /// builds.
    ///
    /// Multi-tenant advertised "per-tenant registries" while `build_vm` had one
    /// shared arm for both roles, so the startup line asserted registry
    /// partitioning that no code path produced. This pins the banner to what is
    /// actually constructed; it fails the moment the description drifts back
    /// ahead of the implementation.
    #[test]
    fn registry_mode_does_not_claim_isolation_build_vm_never_builds() {
        for role in [
            OrchestratorRole::SingleTenant,
            OrchestratorRole::MultiTenant,
        ] {
            assert_eq!(
                role.registry_mode(),
                "one shared trigger/connector registry",
                "{} must describe the single registry build_vm constructs",
                role.as_str(),
            );
        }
    }

    /// Selecting multi-tenant must say which half of the boundary is missing.
    ///
    /// The role is genuinely useful — ingress, event-log topics, and secret
    /// namespaces are all tenant-scoped — so it stays selectable. What it must
    /// not do is let a deployer infer registry isolation from the role name in
    /// silence.
    #[test]
    fn multi_tenant_warns_about_the_shared_registry_and_single_tenant_does_not() {
        let gap = OrchestratorRole::MultiTenant
            .unproven_isolation()
            .expect("multi-tenant must disclose the shared registry");
        assert!(
            gap.contains("shares one registry"),
            "warning must name the shared registry: {gap}",
        );
        assert!(
            gap.contains("6792"),
            "warning must point at the tracking issue: {gap}",
        );

        assert_eq!(
            OrchestratorRole::SingleTenant.unproven_isolation(),
            None,
            "single-tenant delivers the boundary its name implies",
        );
    }
}
