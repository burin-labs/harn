//! Scoped cache of immutable, hydrated module bytecode.
//!
//! Prepared modules deliberately stop before runtime instantiation. Each VM
//! still receives fresh closures, function registries, module state, and init
//! execution; only the serialized-to-runtime bytecode conversion is reused.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::chunk::{Chunk, CompiledFunction};
use crate::module_artifact::{ModuleArtifact, ModuleImportSpec};

const DEFAULT_MAX_ENTRIES: usize = 512;

/// Immutable runtime form of one compiled module artifact.
pub(crate) struct PreparedModuleArtifact {
    pub(crate) imports: Vec<ModuleImportSpec>,
    pub(crate) init_chunk: Option<Arc<Chunk>>,
    pub(crate) functions: BTreeMap<String, Arc<CompiledFunction>>,
    pub(crate) public_names: std::collections::HashSet<String>,
    pub(crate) public_value_names: std::collections::HashSet<String>,
    pub(crate) public_type_names: std::collections::HashSet<String>,
    pub(crate) public_type_schemas: BTreeMap<String, String>,
}

impl PreparedModuleArtifact {
    pub(crate) fn from_cached(artifact: ModuleArtifact) -> Self {
        let init_chunk = artifact
            .init_chunk
            .as_ref()
            .map(|chunk| Arc::new(Chunk::from_cached(chunk)));
        let functions = artifact
            .functions
            .iter()
            .map(|(name, function)| {
                (
                    name.clone(),
                    Arc::new(CompiledFunction::from_cached(function)),
                )
            })
            .collect();
        Self {
            imports: artifact.imports,
            init_chunk,
            functions,
            public_names: artifact.public_names,
            public_value_names: artifact.public_value_names,
            public_type_names: artifact.public_type_names,
            public_type_schemas: artifact.public_type_schemas,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PreparedModuleCacheKey {
    canonical_path: PathBuf,
    source_hash: [u8; 32],
    harn_version: &'static str,
    codegen_fingerprint: &'static str,
    optimizations_enabled: bool,
}

impl PreparedModuleCacheKey {
    fn new(canonical_path: PathBuf, source: &str) -> Self {
        Self {
            canonical_path,
            source_hash: *blake3::hash(source.as_bytes()).as_bytes(),
            harn_version: crate::bytecode_cache::HARN_VERSION,
            codegen_fingerprint: crate::bytecode_cache::CODEGEN_FINGERPRINT,
            optimizations_enabled: crate::compiler::CompilerOptions::from_env()
                .optimizations_enabled(),
        }
    }
}

#[derive(Default)]
struct PreparedModuleCacheInner {
    entries: BTreeMap<PreparedModuleCacheKey, Arc<PreparedModuleArtifact>>,
    insertion_order: VecDeque<PreparedModuleCacheKey>,
    hits: u64,
    misses: u64,
    insertions: u64,
    evictions: u64,
}

/// Typed counters for a [`PreparedModuleCache`] lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreparedModuleCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
    pub entries: usize,
}

/// A bounded, shareable cache of immutable module bytecode templates.
///
/// The handle is explicit so embedders can scope reuse to one test suite,
/// worker, watch generation, or VM baseline. Dropping the final handle releases
/// every prepared artifact; historical user source never accumulates globally.
#[derive(Clone)]
pub struct PreparedModuleCache {
    max_entries: NonZeroUsize,
    inner: Arc<Mutex<PreparedModuleCacheInner>>,
}

impl Default for PreparedModuleCache {
    fn default() -> Self {
        Self::with_capacity(
            NonZeroUsize::new(DEFAULT_MAX_ENTRIES).expect("non-zero cache capacity"),
        )
    }
}

impl PreparedModuleCache {
    pub fn with_capacity(max_entries: NonZeroUsize) -> Self {
        Self {
            max_entries,
            inner: Arc::new(Mutex::new(PreparedModuleCacheInner::default())),
        }
    }

    pub fn stats(&self) -> PreparedModuleCacheStats {
        let inner = self.inner.lock();
        PreparedModuleCacheStats {
            hits: inner.hits,
            misses: inner.misses,
            insertions: inner.insertions,
            evictions: inner.evictions,
            entries: inner.entries.len(),
        }
    }

    pub(crate) fn get(
        &self,
        canonical_path: &Path,
        source: &str,
    ) -> Option<Arc<PreparedModuleArtifact>> {
        let key = PreparedModuleCacheKey::new(canonical_path.to_path_buf(), source);
        let mut inner = self.inner.lock();
        let artifact = inner.entries.get(&key).cloned();
        if artifact.is_some() {
            inner.hits = inner.hits.saturating_add(1);
        } else {
            inner.misses = inner.misses.saturating_add(1);
        }
        artifact
    }

    pub(crate) fn insert(
        &self,
        canonical_path: PathBuf,
        source: &str,
        artifact: Arc<PreparedModuleArtifact>,
    ) -> Arc<PreparedModuleArtifact> {
        let key = PreparedModuleCacheKey::new(canonical_path, source);
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.entries.get(&key) {
            return Arc::clone(existing);
        }
        while inner.entries.len() >= self.max_entries.get() {
            let Some(oldest) = inner.insertion_order.pop_front() else {
                break;
            };
            if inner.entries.remove(&oldest).is_some() {
                inner.evictions = inner.evictions.saturating_add(1);
            }
        }
        inner.insertion_order.push_back(key.clone());
        inner.entries.insert(key, Arc::clone(&artifact));
        inner.insertions = inner.insertions.saturating_add(1);
        artifact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_artifact() -> Arc<PreparedModuleArtifact> {
        Arc::new(PreparedModuleArtifact::from_cached(ModuleArtifact {
            imports: Vec::new(),
            init_chunk: None,
            functions: BTreeMap::new(),
            public_names: Default::default(),
            public_value_names: Default::default(),
            public_type_names: Default::default(),
            public_type_schemas: BTreeMap::new(),
        }))
    }

    #[test]
    fn bounded_cache_evicts_oldest_exact_key() {
        let cache = PreparedModuleCache::with_capacity(NonZeroUsize::new(1).unwrap());
        let first_source = "pub fn first() { 1 }";
        let second_source = "pub fn second() { 2 }";
        let first = empty_artifact();
        let _ = cache.insert(PathBuf::from("first.harn"), first_source, first);
        let _ = cache.insert(
            PathBuf::from("second.harn"),
            second_source,
            empty_artifact(),
        );

        assert!(cache.get(Path::new("first.harn"), first_source).is_none());
        assert!(cache.get(Path::new("second.harn"), second_source).is_some());
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn cache_key_separates_compiler_configuration() {
        let path = PathBuf::from("module.harn");
        let key = PreparedModuleCacheKey::new(path, "pub fn value() { 1 }");
        let mut other_compiler = key.clone();
        other_compiler.optimizations_enabled = !key.optimizations_enabled;

        assert_ne!(key, other_compiler);
    }

    #[test]
    fn dropping_last_cache_handle_releases_prepared_artifacts() {
        let cache = PreparedModuleCache::default();
        let path = PathBuf::from("module.harn");
        let source = "pub fn value() { 1 }";
        let artifact = empty_artifact();
        let weak = Arc::downgrade(&artifact);
        let _ = cache.insert(path, source, artifact);
        let clone = cache.clone();

        drop(cache);
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(weak.upgrade().is_none());
    }
}
