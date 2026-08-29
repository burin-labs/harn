//! Resolution-backed Harn reference facts for embedding graph surfaces.
//!
//! `harn-modules` remains the sole name resolver. This module is the narrow
//! runtime adapter for consumers which already depend on `harn-vm` and must
//! not acquire a reverse dependency on the module system.

use std::collections::HashMap;
use std::path::PathBuf;

/// One resolved use-to-definition fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCodeReference {
    /// Canonical path containing the use.
    pub source_file: PathBuf,
    /// Canonical path containing the definition.
    pub target_file: PathBuf,
    /// Resolved definition name.
    pub target_name: String,
    /// One-based declaration line.
    pub target_line: usize,
}

/// Resolution result with visible coverage provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedCodeReferences {
    /// Canonical files whose retained ASTs were actually walked.
    pub indexed_files: Vec<PathBuf>,
    /// Resolved facts from those files.
    pub edges: Vec<ResolvedCodeReference>,
}

/// Resolve Harn references through the same module graph used by the LSP and
/// `harn graph`. `source_overrides` supplies branch/editor bytes for exact
/// paths while every other file is read from disk.
pub fn resolve_harn_code_references(
    files: &[PathBuf],
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> ResolvedCodeReferences {
    if files.is_empty() {
        return ResolvedCodeReferences::default();
    }
    let build = harn_modules::build_for_reference_index(files, source_overrides);
    let unsaved = source_overrides
        .map(|overrides| overrides.keys().cloned().collect())
        .unwrap_or_default();
    let references = harn_modules::index_references(&build.graph, &build.parsed_sources, &unsaved);
    ResolvedCodeReferences {
        indexed_files: references.files.clone(),
        edges: references
            .edges()
            .into_iter()
            .map(|edge| ResolvedCodeReference {
                source_file: edge.from.file,
                target_file: edge.to_file,
                target_name: edge.to_name,
                target_line: edge.to_line,
            })
            .collect(),
    }
}
