//! Deterministic tools capability.
//!
//! Provides search (ripgrep via `grep-searcher` + `ignore`), file I/O,
//! listing, file outline, git inspection, and
//! process lifecycle (`run_command`, `wait_command`, `run_test`,
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
//! | `run_test`              | implemented                     |
//! | `run_build_command`     | implemented                     |
//! | `inspect_test_results`  | implemented                     |
//! | `manage_packages`       | implemented                     |
//! | `cancel_handle`         | implemented                     |
//!
//! ### Per-session opt-in
//!
//! All deterministic tools are gated by a per-thread feature flag.
//! Pipelines must call `hostlib_enable("tools:deterministic")` (registered
//! by [`ToolsCapability::register_builtins`]) before any of the tool
//! methods will execute. Until then, calls return
//! [`HostlibError::Backend`] with an explanatory message. The per-session
//! opt-in model keeps the deterministic-tool surface sandbox-friendly.

use harn_vm::VmDictExt;

use harn_vm::VmValue;

use crate::error::HostlibError;
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

pub use permissions::{FEATURE_TERMINAL_SESSION, FEATURE_TOOLS_DETERMINISTIC};

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

        registry.register_gated_fn("tools", "hostlib_tools_search", "search", search::run);
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_read_file",
            "read_file",
            file_io::read_file,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_write_file",
            "write_file",
            file_io::write_file,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_delete_file",
            "delete_file",
            file_io::delete_file,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_list_directory",
            "list_directory",
            file_io::list_directory,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_get_file_outline",
            "get_file_outline",
            outline::run,
        );
        registry.register_gated_fn("tools", "hostlib_tools_git", "git", git::run);

        registry.register_gated_command_fn(
            "tools",
            "hostlib_tools_run_command",
            "run_command",
            run_command::handle,
        );
        registry.register_gated_fn(
            "tools",
            read_command_output::NAME,
            "read_command_output",
            read_command_output::handle,
        );
        registry.register_gated_fn(
            "tools",
            wait_command::NAME,
            "wait_command",
            wait_command::handle,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_run_test",
            "run_test",
            run_test::handle,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_run_build_command",
            "run_build_command",
            run_build_command::handle,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_inspect_test_results",
            "inspect_test_results",
            inspect_test_results::handle,
        );
        registry.register_gated_fn(
            "tools",
            "hostlib_tools_manage_packages",
            "manage_packages",
            manage_packages::handle,
        );
        registry.register_gated_fn(
            "tools",
            cancel_handle::NAME,
            "cancel_handle",
            cancel_handle::handle,
        );
        registry.register_gated_fn(
            "tools",
            list_handles::NAME,
            "list_handles",
            list_handles::handle,
        );
        registry.register_gated_fn(
            "tools",
            toolchain_facts::NAME,
            "toolchain_facts",
            toolchain_facts::handle,
        );

        // The opt-in builtin lives in the `tools` module so embedders that
        // don't compose `ToolsCapability` don't accidentally expose it.
        registry.register_fn("tools", "hostlib_enable", "enable", handle_enable);
    }
}

/// Implementation of the `hostlib_enable` builtin. Accepts either a bare
/// string (`hostlib_enable("tools:deterministic")`) or a dict carrying a
/// `feature` key (`hostlib_enable({feature: "..."})`) so callers can
/// supply structured payloads in the future without breaking back-compat.
fn handle_enable(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let feature = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(VmValue::Dict(dict)) => match dict.get("feature") {
            Some(VmValue::String(s)) => s.to_string(),
            _ => {
                return Err(HostlibError::MissingParameter {
                    builtin: "hostlib_enable",
                    param: "feature",
                });
            }
        },
        _ => {
            return Err(HostlibError::MissingParameter {
                builtin: "hostlib_enable",
                param: "feature",
            });
        }
    };

    let supported = feature == permissions::FEATURE_TOOLS_DETERMINISTIC
        || cfg!(feature = "terminal-session") && feature == permissions::FEATURE_TERMINAL_SESSION;
    if !supported {
        return Err(HostlibError::InvalidParameter {
            builtin: "hostlib_enable",
            param: "feature",
            message: format!(
                "unknown feature `{feature}`; supported: [`tools:deterministic`{}]",
                if cfg!(feature = "terminal-session") {
                    ", `terminal:session`"
                } else {
                    ""
                }
            ),
        });
    }

    let newly_enabled = permissions::enable(&feature);
    let mut map: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
    map.put_str("feature", feature);
    map.insert(harn_vm::value::intern_key("enabled"), VmValue::Bool(true));
    map.insert(
        harn_vm::value::intern_key("newly_enabled"),
        VmValue::Bool(newly_enabled),
    );
    Ok(VmValue::dict(map))
}
