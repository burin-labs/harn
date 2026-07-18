//! Embedded stdlib source access for the module graph.

use std::path::PathBuf;

/// Return the embedded stdlib source for `module` (the part after
/// `std/`), or `None` if no stdlib module with that name exists.
pub(crate) fn get_stdlib_source(module: &str) -> Option<&'static str> {
    harn_stdlib::get_stdlib_source(module)
}

/// Rust builtins exported through an embedded stdlib module's public surface.
pub(crate) fn builtin_reexports(module: &str) -> &'static [&'static str] {
    harn_stdlib::builtin_reexports(module)
}

/// Sentinel path used to key embedded stdlib modules in the module
/// graph. Real files never resolve to this path, so collisions are
/// impossible.
pub(crate) fn stdlib_virtual_path(module: &str) -> PathBuf {
    PathBuf::from(format!("<std>/{module}"))
}
