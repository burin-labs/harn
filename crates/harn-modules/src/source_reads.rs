//! Where module sources come from: disk, the embedded stdlib, or a caller's
//! unsaved buffers, plus the count of disk reads that editor caching is
//! measured against.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    build_inner, normalize_path, stdlib, stdlib_module_name, PackageContext, ParsedSourceRetention,
};
use crate::ModuleGraph;

/// Return the source for a resolved module path.
///
/// Real paths are read from disk. `<std>/<module>` virtual paths are backed by
/// the embedded stdlib source table, so callers can parse resolved stdlib
/// modules without knowing about the stdlib mirror layout.
pub fn read_module_source(path: &Path) -> Option<String> {
    if let Some(stdlib_module) = stdlib_module_name(path) {
        return stdlib::get_stdlib_source(stdlib_module).map(ToString::to_string);
    }
    MODULE_DISK_READS.with(|reads| reads.set(reads.get() + 1));
    std::fs::read_to_string(path).ok()
}

thread_local! {
    static MODULE_DISK_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many module sources this thread has read from disk while building
/// module graphs. Embedded stdlib modules and caller-supplied sources are not
/// disk reads.
pub fn module_source_disk_reads() -> usize {
    MODULE_DISK_READS.with(std::cell::Cell::get)
}

/// Build a module graph using caller-owned source for one root file.
///
/// Imported modules still resolve from their normal filesystem or embedded
/// stdlib locations. This keeps editor diagnostics aligned with unsaved root
/// buffers without creating a second module resolver.
pub fn build_with_source(file: &Path, source: &str) -> ModuleGraph {
    let file = normalize_path(file);
    let source_overrides = HashMap::from([(file.clone(), source.to_string())]);
    build_inner(
        &[file],
        ParsedSourceRetention::None,
        Some(&source_overrides),
        PackageContext::Project,
    )
    .graph
}

/// Build a module graph for one root file using caller-owned source for any
/// module in `sources`, such as an editor's unsaved buffers. Modules without an
/// entry resolve from their filesystem or embedded stdlib locations.
pub fn build_with_sources(file: &Path, sources: &HashMap<PathBuf, String>) -> ModuleGraph {
    let file = normalize_path(file);
    let source_overrides: HashMap<PathBuf, String> = sources
        .iter()
        .map(|(path, source)| (normalize_path(path), source.clone()))
        .collect();
    build_inner(
        &[file],
        ParsedSourceRetention::None,
        Some(&source_overrides),
        PackageContext::Project,
    )
    .graph
}
