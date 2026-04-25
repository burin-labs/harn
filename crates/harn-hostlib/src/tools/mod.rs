//! Deterministic tools capability.
//!
//! Ports the Swift `CoreToolExecutor` surface: search (ripgrep via
//! `grep-searcher` + `ignore`), file I/O, listing, file outline, git via
//! `gix`, and process lifecycle (`run_command`, `run_test`,
//! `run_build_command`, `inspect_test_results`, `manage_packages`).
//!
//! Implementation is split across follow-up issues C2/C3.

use crate::registry::{BuiltinRegistry, HostlibCapability};

/// Tools capability handle.
#[derive(Default)]
pub struct ToolsCapability;

impl HostlibCapability for ToolsCapability {
    fn module_name(&self) -> &'static str {
        "tools"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
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
        registry.register_unimplemented("hostlib_tools_run_command", "tools", "run_command");
        registry.register_unimplemented("hostlib_tools_run_test", "tools", "run_test");
        registry.register_unimplemented(
            "hostlib_tools_run_build_command",
            "tools",
            "run_build_command",
        );
        registry.register_unimplemented(
            "hostlib_tools_inspect_test_results",
            "tools",
            "inspect_test_results",
        );
        registry.register_unimplemented(
            "hostlib_tools_manage_packages",
            "tools",
            "manage_packages",
        );
    }
}
