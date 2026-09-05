//! Deciding whether a source's artifacts are already on disk.
//!
//! Precompiling recompiled every source it walked, so callers compensated with
//! a stamp over a whole tree and one edited file rebuilt every artifact beside
//! it. Nothing new identifies an artifact here: `CacheKey` already folds a
//! source hash with a hash of the transitive imports, the harn version, the
//! codegen fingerprint, the compiler options tag, and the module provenance,
//! and it is the same key the writers stamp into the artifacts. This module
//! derives it once, before any compile work, so the two cannot disagree.

use std::path::{Path, PathBuf};

use harn_vm::module_artifact::ModuleCompilationContext;

use crate::compiler_context::{imported_symbols_for_source, SourceCompilerAuthority};

/// The keys the two artifacts for one source will be stored under.
pub(super) struct ArtifactKeys {
    pub(super) entry: harn_vm::bytecode_cache::CacheKey,
    pub(super) module: harn_vm::bytecode_cache::CacheKey,
}

impl ArtifactKeys {
    /// True when both artifacts already on disk carry exactly these keys.
    ///
    /// Every field is compared, provenance and compiler tag included, through
    /// the same header comparison a cache load uses, so a reuse can never
    /// accept an artifact a load would reject.
    ///
    /// Requiring both is deliberate. A source whose module view does not
    /// compile writes no module artifact, and from outside a compile "absent"
    /// and "stale" are the same observation, so it recompiles rather than
    /// guess.
    pub(super) fn match_artifacts_at(&self, entry_dest: &Path, module_dest: &Path) -> bool {
        harn_vm::bytecode_cache::entry_artifact_at_matches(entry_dest, &self.entry)
            && harn_vm::bytecode_cache::module_artifact_at_matches(module_dest, &self.module)
    }
}

/// Derive both keys, and return the import graph projection they were derived
/// from so the compile that may follow does not walk it a second time.
pub(super) fn artifact_keys(
    source_path: &Path,
    source: &str,
    source_root: Option<&Path>,
    authority: &SourceCompilerAuthority,
) -> (ArtifactKeys, ModuleCompilationContext) {
    let entry = if source_root.is_some() {
        harn_vm::bytecode_cache::CacheKey::from_relocatable_source(source_path, source)
    } else {
        harn_vm::bytecode_cache::CacheKey::from_source(source_path, source)
    };
    let compilation_context = imported_symbols_for_source(source_path, source);
    let module_source = harn_vm::module_source::ModuleSource::from_text(source);
    let module = harn_vm::bytecode_cache::CacheKey::from_module_source(
        &module_source,
        &compilation_context,
        authority.module_provenance(),
    );
    (ArtifactKeys { entry, module }, compilation_context)
}

/// What one source cost this invocation.
///
/// `Reused` is a success that did no compile work, kept distinct from
/// `Compiled` because "the tree is up to date" and "the tree was rebuilt" are
/// the two states a caller of an incremental compiler needs told apart, and
/// because a reuse count is the only direct evidence the reuse path was
/// reached at all.
pub(super) enum Outcome {
    Compiled(PathBuf),
    Reused(PathBuf),
}

impl Outcome {
    pub(super) fn destination(&self) -> &PathBuf {
        match self {
            Outcome::Compiled(path) | Outcome::Reused(path) => path,
        }
    }
}
