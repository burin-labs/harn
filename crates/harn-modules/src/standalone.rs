use std::collections::HashMap;
use std::path::Path;

use super::{build_inner, normalize_path, ModuleGraph, ParsedSourceRetention};

/// Whether module resolution may consult package metadata discovered around
/// the seed files. Standalone callers retain relative and standard-library
/// imports while making package resolution independent of ambient projects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PackageContext {
    #[default]
    Project,
    Standalone,
}

/// Build a module graph using caller-owned source without discovering package
/// metadata around the entry file.
pub fn build_with_standalone_source(file: &Path, source: &str) -> ModuleGraph {
    let file = normalize_path(file);
    let source_overrides = HashMap::from([(file.clone(), source.to_string())]);
    build_inner(
        &[file],
        ParsedSourceRetention::None,
        Some(&source_overrides),
        PackageContext::Standalone,
    )
    .graph
}
