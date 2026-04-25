//! Deterministic tools capability.
//!
//! Ports the Swift `CoreToolExecutor` surface: search (ripgrep via
//! `grep-searcher` + `ignore`), file I/O, listing, file outline, git via
//! `gix`, and process lifecycle (`run_command`, `run_test`,
//! `run_build_command`, `inspect_test_results`, `manage_packages`).
//!
//! Process-lifecycle tools (issue #568) are implemented in this module
//! today; the search / fs / git surface stays scaffolded under
//! [`HostlibError::Unimplemented`] until issues C1/C2 land.

use std::sync::Arc;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

mod diagnostics;
mod inspect_test_results;
mod lang;
mod manage_packages;
mod payload;
mod proc;
mod response;
mod run_build_command;
mod run_command;
mod run_test;
mod test_parsers;

/// Tools capability handle.
#[derive(Default)]
pub struct ToolsCapability;

impl HostlibCapability for ToolsCapability {
    fn module_name(&self) -> &'static str {
        "tools"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        // Still scaffold — implemented under issues C1 (search/fs) / C2 (git).
        registry.register_unimplemented("hostlib_tools_search", "tools", "search");
        registry.register_unimplemented("hostlib_tools_read_file", "tools", "read_file");
        registry.register_unimplemented("hostlib_tools_write_file", "tools", "write_file");
        registry.register_unimplemented("hostlib_tools_delete_file", "tools", "delete_file");
        registry.register_unimplemented("hostlib_tools_list_directory", "tools", "list_directory");
        registry.register_unimplemented(
            "hostlib_tools_get_file_outline",
            "tools",
            "get_file_outline",
        );
        registry.register_unimplemented("hostlib_tools_git", "tools", "git");

        // Implemented (issue #568): process lifecycle tools.
        registry.register(builtin(
            run_command::NAME,
            "run_command",
            run_command::handle,
        ));
        registry.register(builtin(run_test::NAME, "run_test", run_test::handle));
        registry.register(builtin(
            run_build_command::NAME,
            "run_build_command",
            run_build_command::handle,
        ));
        registry.register(builtin(
            inspect_test_results::NAME,
            "inspect_test_results",
            inspect_test_results::handle,
        ));
        registry.register(builtin(
            manage_packages::NAME,
            "manage_packages",
            manage_packages::handle,
        ));
    }
}

fn builtin(
    name: &'static str,
    method: &'static str,
    handler: fn(&[harn_vm::VmValue]) -> Result<harn_vm::VmValue, HostlibError>,
) -> RegisteredBuiltin {
    let arc: SyncHandler = Arc::new(handler);
    RegisteredBuiltin {
        name,
        module: "tools",
        method,
        handler: arc,
    }
}
