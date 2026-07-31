//! Deterministic tools capability.
//!
//! Provides search (ripgrep via `grep-searcher` + `ignore`), file I/O,
//! listing, file outline, git inspection, and
//! process lifecycle (`run_command`, `wait_command`, `wait_command_output`, `run_test`,
//! `run_build_command`, `inspect_test_results`, `manage_packages`,
//! `cancel_handle`).
//!
//! Implementation status:
//!
//! | Method                  | Status                          |
//! |-------------------------|---------------------------------|
//! | `search`                | implemented                     |
//! | `read_file`             | implemented                     |
//! | `write_file`            | implemented                     |
//! | `delete_file`           | implemented                     |
//! | `list_directory`        | implemented                     |
//! | `get_file_outline`      | implemented (regex extractor)   |
//! | `git`                   | implemented (system git CLI)    |
//! | `run_command`           | implemented                     |
//! | `wait_command`          | implemented                     |
//! | `wait_command_output`   | implemented                     |
//! | `run_test`              | implemented                     |
//! | `run_build_command`     | implemented                     |
//! | `inspect_test_results`  | implemented                     |
//! | `manage_packages`       | implemented                     |
//! | `cancel_handle`         | implemented                     |
//!
use crate::registry::{BuiltinRegistry, HostlibCapability};

pub(crate) mod args;
mod cancel_handle;
mod diagnostics;
mod file_io;
mod git;
pub(crate) mod inspect_test_results;
mod lang;
mod list_handles;
pub mod long_running;
mod manage_packages;
mod outline;
pub(crate) mod payload;
pub mod permissions;
mod proc;
mod read_command_output;
mod response;
mod run_build_command;
mod run_command;
pub(crate) use run_command::policy_blocked_response as policy_blocked_run_command_response;
pub(crate) use run_command::request_is_background as run_command_request_is_background;
pub(crate) mod run_test;
mod search;
mod test_parsers;
mod toolchain_facts;
mod wait_command;
mod wait_command_output;

/// Tools capability handle.
#[derive(Default)]
pub struct ToolsCapability;

impl HostlibCapability for ToolsCapability {
    fn module_name(&self) -> &'static str {
        "tools"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        // Register the session-cleanup hook once per process so long-running
        // tool handles are killed when the agent-loop session ends.
        long_running::register_cleanup_hook();

        registry.register_fn("tools", "hostlib_tools_search", "search", search::run);
        registry.register_fn(
            "tools",
            "hostlib_tools_read_file",
            "read_file",
            file_io::read_file,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_write_file",
            "write_file",
            file_io::write_file,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_delete_file",
            "delete_file",
            file_io::delete_file,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_list_directory",
            "list_directory",
            file_io::list_directory,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_get_file_outline",
            "get_file_outline",
            outline::run,
        );
        registry.register_fn("tools", "hostlib_tools_git", "git", git::run);

        registry.register_command_fn(
            "tools",
            "hostlib_tools_run_command",
            "run_command",
            run_command::handle,
        );
        registry.register_fn(
            "tools",
            read_command_output::NAME,
            "read_command_output",
            read_command_output::handle,
        );
        registry.register_fn(
            "tools",
            wait_command::NAME,
            "wait_command",
            wait_command::handle,
        );
        registry.register_async_fn(
            "tools",
            wait_command_output::NAME,
            "wait_command_output",
            wait_command_output::handle,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_run_test",
            "run_test",
            run_test::handle,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_run_build_command",
            "run_build_command",
            run_build_command::handle,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_inspect_test_results",
            "inspect_test_results",
            inspect_test_results::handle,
        );
        registry.register_fn(
            "tools",
            "hostlib_tools_manage_packages",
            "manage_packages",
            manage_packages::handle,
        );
        registry.register_fn(
            "tools",
            cancel_handle::NAME,
            "cancel_handle",
            cancel_handle::handle,
        );
        registry.register_fn(
            "tools",
            list_handles::NAME,
            "list_handles",
            list_handles::handle,
        );
        registry.register_fn(
            "tools",
            toolchain_facts::NAME,
            "toolchain_facts",
            toolchain_facts::handle,
        );
    }
}
