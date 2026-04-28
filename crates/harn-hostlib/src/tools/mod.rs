//! Deterministic tools capability.
//!
//! Provides search (ripgrep via `grep-searcher` + `ignore`), file I/O,
//! listing, file outline, git inspection, and
//! process lifecycle (`run_command`, `run_test`, `run_build_command`,
//! `inspect_test_results`, `manage_packages`, `cancel_handle`).
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

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

pub(crate) mod args;
mod cancel_handle;
mod diagnostics;
mod file_io;
mod git;
mod inspect_test_results;
mod lang;
pub mod long_running;
mod manage_packages;
mod outline;
mod payload;
pub mod permissions;
mod proc;
mod response;
mod run_build_command;
mod run_command;
mod run_test;
mod search;
mod test_parsers;

pub use permissions::FEATURE_TOOLS_DETERMINISTIC;

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

        register_gated(registry, "hostlib_tools_search", "search", search::run);
        register_gated(
            registry,
            "hostlib_tools_read_file",
            "read_file",
            file_io::read_file,
        );
        register_gated(
            registry,
            "hostlib_tools_write_file",
            "write_file",
            file_io::write_file,
        );
        register_gated(
            registry,
            "hostlib_tools_delete_file",
            "delete_file",
            file_io::delete_file,
        );
        register_gated(
            registry,
            "hostlib_tools_list_directory",
            "list_directory",
            file_io::list_directory,
        );
        register_gated(
            registry,
            "hostlib_tools_get_file_outline",
            "get_file_outline",
            outline::run,
        );
        register_gated(registry, "hostlib_tools_git", "git", git::run);

        register_gated(
            registry,
            "hostlib_tools_run_command",
            "run_command",
            run_command::handle,
        );
        register_gated(
            registry,
            "hostlib_tools_run_test",
            "run_test",
            run_test::handle,
        );
        register_gated(
            registry,
            "hostlib_tools_run_build_command",
            "run_build_command",
            run_build_command::handle,
        );
        register_gated(
            registry,
            "hostlib_tools_inspect_test_results",
            "inspect_test_results",
            inspect_test_results::handle,
        );
        register_gated(
            registry,
            "hostlib_tools_manage_packages",
            "manage_packages",
            manage_packages::handle,
        );
        register_gated(
            registry,
            cancel_handle::NAME,
            "cancel_handle",
            cancel_handle::handle,
        );

        // The opt-in builtin lives in the `tools` module so embedders that
        // don't compose `ToolsCapability` don't accidentally expose it.
        let handler: SyncHandler = Arc::new(handle_enable);
        registry.register(RegisteredBuiltin {
            name: "hostlib_enable",
            module: "tools",
            method: "enable",
            handler,
        });
    }
}

/// Register a builtin whose handler runs only when the deterministic-tools
/// feature has been enabled on the current thread.
fn register_gated(
    registry: &mut BuiltinRegistry,
    name: &'static str,
    method: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) {
    let handler: SyncHandler = Arc::new(move |args: &[VmValue]| {
        if !permissions::is_enabled(permissions::FEATURE_TOOLS_DETERMINISTIC) {
            return Err(HostlibError::Backend {
                builtin: name,
                message: format!(
                    "feature `{}` is not enabled in this session — call \
                     `hostlib_enable(\"{}\")` before invoking deterministic tools",
                    permissions::FEATURE_TOOLS_DETERMINISTIC,
                    permissions::FEATURE_TOOLS_DETERMINISTIC
                ),
            });
        }
        runner(args)
    });
    registry.register(RegisteredBuiltin {
        name,
        module: "tools",
        method,
        handler,
    });
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

    match feature.as_str() {
        permissions::FEATURE_TOOLS_DETERMINISTIC => {
            let newly_enabled = permissions::enable(&feature);
            let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
            map.insert("feature".to_string(), VmValue::String(Rc::from(feature)));
            map.insert("enabled".to_string(), VmValue::Bool(true));
            map.insert("newly_enabled".to_string(), VmValue::Bool(newly_enabled));
            Ok(VmValue::Dict(Rc::new(map)))
        }
        other => Err(HostlibError::InvalidParameter {
            builtin: "hostlib_enable",
            param: "feature",
            message: format!("unknown feature `{other}`; supported: [`tools:deterministic`]"),
        }),
    }
}
