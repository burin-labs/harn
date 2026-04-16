//! Embedded stdlib sources, mirrored from `harn-vm` so the module graph
//! can resolve `import "std/<module>"` without taking a hard dependency
//! on the runtime crate.
//!
//! The files are pulled in via `include_str!` from `harn-vm/src/`, which
//! makes them a single source of truth — changes to stdlib `.harn`
//! files automatically propagate to the type-checker's symbol view on
//! the next build.

use std::path::PathBuf;

/// Return the embedded stdlib source for `module` (the part after
/// `std/`), or `None` if no stdlib module with that name exists.
pub(crate) fn get_stdlib_source(module: &str) -> Option<&'static str> {
    match module {
        "text" => Some(include_str!("../../harn-vm/src/stdlib_text.harn")),
        "collections" => Some(include_str!("../../harn-vm/src/stdlib_collections.harn")),
        "math" => Some(include_str!("../../harn-vm/src/stdlib_math.harn")),
        "path" => Some(include_str!("../../harn-vm/src/stdlib_path.harn")),
        "json" => Some(include_str!("../../harn-vm/src/stdlib_json.harn")),
        "schema" => Some(include_str!("../../harn-vm/src/stdlib_schema.harn")),
        "testing" => Some(include_str!("../../harn-vm/src/stdlib_testing.harn")),
        "context" => Some(include_str!("../../harn-vm/src/stdlib_context.harn")),
        "runtime" => Some(include_str!("../../harn-vm/src/stdlib_runtime.harn")),
        "project" => Some(include_str!("../../harn-vm/src/stdlib_project.harn")),
        "async" => Some(include_str!("../../harn-vm/src/stdlib_async.harn")),
        "agents" => Some(include_str!("../../harn-vm/src/stdlib_agents.harn")),
        "checkpoint" => Some(include_str!("../../harn-vm/src/stdlib_checkpoint.harn")),
        "worktree" => Some(include_str!("../../harn-vm/src/stdlib_worktree.harn")),
        "acp" => Some(include_str!("../../harn-vm/src/stdlib_acp.harn")),
        _ => None,
    }
}

/// Sentinel path used to key embedded stdlib modules in the module
/// graph. Real files never resolve to this path, so collisions are
/// impossible.
pub(crate) fn stdlib_virtual_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<std>/{module}"))
}
