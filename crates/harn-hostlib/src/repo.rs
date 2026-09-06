//! Typed repository measurements shared by scripts and the CLI.

use harn_vm::VmValue;
use serde::Deserialize;

use crate::{repo_loc, BuiltinRegistry, HostlibCapability, HostlibError};

const LOC: &str = "hostlib_repo_loc";

/// Opt-in read-only repository capability.
pub struct RepoCapability;

impl HostlibCapability for RepoCapability {
    fn module_name(&self) -> &'static str {
        "repo"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_fn("repo", LOC, "loc", loc);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: String,
    registry: repo_loc::LocRegistry,
}

fn loc(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = crate::tools::args::dict_arg(LOC, args)?;
    let request: Request =
        serde_json::from_value(crate::json::vm_dict_to_json(&raw)).map_err(|error| {
            HostlibError::InvalidParameter {
                builtin: LOC,
                param: "params",
                message: error.to_string(),
            }
        })?;
    let root = crate::tools::args::resolve_host_path(&request.root);
    crate::tools::permissions::enforce_path_scope(
        LOC,
        &root,
        harn_vm::process_sandbox::FsAccess::Read,
    )?;
    let report =
        repo_loc::measure(&root, &request.registry).map_err(|error| HostlibError::Backend {
            builtin: LOC,
            message: error.to_string(),
        })?;
    let json = serde_json::to_value(report).map_err(|error| HostlibError::Backend {
        builtin: LOC,
        message: error.to_string(),
    })?;
    Ok(harn_vm::json_to_vm_value(&json))
}
