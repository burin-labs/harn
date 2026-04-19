use std::path::Path;

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum OrchestratorRole {
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

    pub(crate) fn build_vm(
        self,
        workspace_root: &Path,
        source_dir: &Path,
        state_dir: &Path,
    ) -> Result<harn_vm::Vm, String> {
        match self {
            Self::SingleTenant => {
                std::env::set_var(
                    harn_vm::runtime_paths::HARN_STATE_DIR_ENV,
                    state_dir.display().to_string(),
                );
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                harn_vm::register_store_builtins(&mut vm, workspace_root);
                harn_vm::register_metadata_builtins(&mut vm, workspace_root);
                harn_vm::register_checkpoint_builtins(&mut vm, workspace_root, "orchestrator");
                vm.set_project_root(workspace_root);
                vm.set_source_dir(source_dir);
                Ok(vm)
            }
            Self::MultiTenant => Err(
                "multi-tenant orchestrator role is not yet implemented (see O-12 #190)".to_string(),
            ),
        }
    }
}
