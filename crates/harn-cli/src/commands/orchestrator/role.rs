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

    pub(crate) fn registry_mode(self) -> &'static str {
        match self {
            Self::SingleTenant => "one shared trigger/connector registry",
            Self::MultiTenant => "per-tenant registries",
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
}
