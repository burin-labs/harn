//! Scoped cache of immutable, hydrated module bytecode.
//!
//! Prepared modules deliberately stop before runtime instantiation. Each VM
//! still receives fresh closures, function registries, module state, and init
//! execution; only the serialized-to-runtime bytecode conversion is reused.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use harn_modules::DefKind;
use quick_cache::sync::{Cache, GuardResult};
use quick_cache::{DefaultHashBuilder, Lifecycle, UnitWeighter};

use crate::chunk::{Chunk, CompiledFunction};
use crate::module_artifact::{
    compile_module_artifact_from_source_with_context,
    compile_trusted_host_dispatch_module_artifact_from_source_with_context,
    module_compilation_context_for_source, ModuleArtifact, ModuleCompilationContext,
    ModuleImportSpec, ModuleProvenance,
};
use crate::module_source::ModuleSource;
use crate::{ModulePhaseRecorder, ModulePhaseStats, VmError};
const DEFAULT_MAX_ENTRIES: usize = 512;

/// Immutable runtime form of one compiled module artifact.
pub(crate) struct PreparedModuleArtifact {
    pub(crate) provenance: ModuleProvenance,
    pub(crate) imports: Vec<ModuleImportSpec>,
    pub(crate) type_schema_init_chunks: Vec<Arc<Chunk>>,
    pub(crate) init_chunk: Option<Arc<Chunk>>,
    pub(crate) functions: BTreeMap<String, Arc<CompiledFunction>>,
    pub(crate) public_exports: BTreeMap<String, DefKind>,
    pub(crate) public_value_names: std::collections::HashSet<String>,
    pub(crate) public_type_names: std::collections::HashSet<String>,
}

impl PreparedModuleArtifact {
    pub(crate) fn from_cached(artifact: ModuleArtifact) -> Self {
        let ModuleArtifact {
            provenance,
            imports,
            type_schema_init_chunks,
            init_chunk,
            functions,
            public_exports,
            public_value_names,
            public_type_names,
        } = artifact;
        let type_schema_init_chunks = type_schema_init_chunks
            .into_iter()
            .map(|chunk| Arc::new(Chunk::from_cached(chunk)))
            .collect();
        let init_chunk = init_chunk.map(|chunk| Arc::new(Chunk::from_cached(chunk)));
        let functions = functions
            .into_iter()
            .map(|(name, function)| (name, Arc::new(CompiledFunction::from_cached(function))))
            .collect();
        Self {
            provenance,
            imports,
            type_schema_init_chunks,
            init_chunk,
            functions,
            public_exports,
            public_value_names,
            public_type_names,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PreparedModuleCacheKey {
    canonical_path: PathBuf,
    source_hash: [u8; 32],
    provenance: ModuleProvenance,
    harn_version: &'static str,
    codegen_fingerprint: &'static str,
    optimizations_enabled: bool,
    compilation_context_digest: [u8; 32],
}

impl PreparedModuleCacheKey {
    /// `source_hash` is the same SHA-256 that names the module's on-disk
    /// artifact. Keying on it rather than a second digest of the same bytes
    /// means a warm module load hashes its source once, and lets a caller
    /// holding a recorded digest find a prepared artifact without the bytes.
    #[cfg(test)]
    fn new(canonical_path: PathBuf, source_hash: [u8; 32], provenance: ModuleProvenance) -> Self {
        Self::with_context(
            canonical_path,
            source_hash,
            provenance,
            &ModuleCompilationContext::default(),
        )
    }

    fn with_context(
        canonical_path: PathBuf,
        source_hash: [u8; 32],
        provenance: ModuleProvenance,
        compilation_context: &ModuleCompilationContext,
    ) -> Self {
        Self {
            canonical_path,
            source_hash,
            provenance,
            harn_version: crate::bytecode_cache::HARN_VERSION,
            codegen_fingerprint: crate::bytecode_cache::CODEGEN_FINGERPRINT,
            optimizations_enabled: crate::compiler::CompilerOptions::from_env()
                .optimizations_enabled(),
            compilation_context_digest: compilation_context.digest(),
        }
    }
}

#[derive(Default)]
struct PreparedModuleCacheCounters {
    hits: AtomicU64,
    misses: AtomicU64,
    insertions: AtomicU64,
    evictions: AtomicU64,
}

fn saturating_increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

#[derive(Clone)]
struct PreparedModuleCacheLifecycle {
    counters: Arc<PreparedModuleCacheCounters>,
}

impl Lifecycle<PreparedModuleCacheKey, Arc<PreparedModuleArtifact>>
    for PreparedModuleCacheLifecycle
{
    type RequestState = ();

    fn on_evict(
        &self,
        _state: &mut Self::RequestState,
        _key: PreparedModuleCacheKey,
        _artifact: Arc<PreparedModuleArtifact>,
    ) {
        saturating_increment(&self.counters.evictions);
    }
}

type PreparedArtifactCache = Cache<
    PreparedModuleCacheKey,
    Arc<PreparedModuleArtifact>,
    UnitWeighter,
    DefaultHashBuilder,
    PreparedModuleCacheLifecycle,
>;

/// Typed counters for a [`PreparedModuleCache`] lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreparedModuleCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    /// Artifacts discarded by the bounded cache, including cold scan
    /// candidates rejected by S3-FIFO admission before becoming residents.
    pub evictions: u64,
    pub entries: usize,
}

/// A bounded, shareable cache of immutable module bytecode templates.
///
/// The handle is explicit so embedders can scope reuse to one test suite,
/// worker, watch generation, or VM baseline. Dropping the final handle releases
/// every prepared artifact; historical user source never accumulates globally.
/// Concurrent misses for one exact key share a single preparation owner, while
/// unrelated modules remain independently preparable.
#[derive(Clone)]
pub struct PreparedModuleCache {
    entries: Arc<PreparedArtifactCache>,
    counters: Arc<PreparedModuleCacheCounters>,
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
        let counters = Arc::new(PreparedModuleCacheCounters::default());
        let lifecycle = PreparedModuleCacheLifecycle {
            counters: Arc::clone(&counters),
        };
        let capacity = max_entries.get();
        Self {
            entries: Arc::new(Cache::with(
                capacity,
                capacity as u64,
                UnitWeighter,
                DefaultHashBuilder::default(),
                lifecycle,
            )),
            counters,
        }
    }

    pub fn stats(&self) -> PreparedModuleCacheStats {
        PreparedModuleCacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            insertions: self.counters.insertions.load(Ordering::Relaxed),
            evictions: self.counters.evictions.load(Ordering::Relaxed),
            entries: self.entries.len(),
        }
    }

    /// Prepare every import reachable from `roots` without instantiating or
    /// executing module state.
    ///
    /// Root files themselves are entry programs, not runtime imports, so only
    /// their transitive import closure is prepared. Invalid modules are left
    /// uncached for the canonical VM load to diagnose.
    pub fn prepare_import_graph(&self, roots: &[PathBuf]) -> ModulePhaseStats {
        self.prepare_import_graph_with_provenance(roots, ModuleProvenance::User)
    }

    /// Prepare a Rust-embedder-selected host-dispatch graph without making its
    /// bytecode visible to ordinary user imports. The in-memory cache key
    /// retains provenance, and fresh VMs still instantiate independent module
    /// state from the immutable artifacts.
    pub fn prepare_trusted_host_dispatch_import_graph(
        &self,
        roots: &[PathBuf],
    ) -> ModulePhaseStats {
        self.prepare_import_graph_with_provenance(roots, ModuleProvenance::TrustedHostDispatch)
    }

    fn prepare_import_graph_with_provenance(
        &self,
        roots: &[PathBuf],
        provenance: ModuleProvenance,
    ) -> ModulePhaseStats {
        if roots.is_empty() {
            return ModulePhaseStats::default();
        }

        let graph = harn_modules::build(roots);
        let root_paths = roots
            .iter()
            .map(|path| harn_modules::canonical_path(path))
            .collect::<std::collections::HashSet<_>>();
        let recorder = ModulePhaseRecorder::new();

        for path in graph.module_paths() {
            if root_paths.contains(&harn_modules::canonical_path(&path)) {
                continue;
            }
            if path.to_str().is_some_and(|path| path.starts_with("<std>/")) {
                let _ = crate::vm::prepare_stdlib_module_artifact(&path, Some(&recorder));
                continue;
            }

            let source = {
                let _load_span = recorder.load_span();
                match crate::module_source::read(&path) {
                    Ok(source) => source,
                    Err(_) => continue,
                }
            };
            let Ok(compilation_context) =
                ModuleCompilationContext::for_source_in_graph(&graph, &path, source.as_str())
            else {
                continue;
            };
            let canonical = harn_modules::canonical_path(&path);
            let _ = self.prepare(
                &path,
                &canonical,
                &source,
                Some(&compilation_context),
                Some(&recorder),
                provenance,
            );
        }

        recorder.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn get(
        &self,
        canonical_path: &Path,
        source_hash: [u8; 32],
        provenance: ModuleProvenance,
    ) -> Option<Arc<PreparedModuleArtifact>> {
        self.get_with_context(
            canonical_path,
            source_hash,
            provenance,
            &ModuleCompilationContext::default(),
        )
    }

    pub(crate) fn get_with_context(
        &self,
        canonical_path: &Path,
        source_hash: [u8; 32],
        provenance: ModuleProvenance,
        compilation_context: &ModuleCompilationContext,
    ) -> Option<Arc<PreparedModuleArtifact>> {
        let key = PreparedModuleCacheKey::with_context(
            canonical_path.to_path_buf(),
            source_hash,
            provenance,
            compilation_context,
        );
        let artifact = self.entries.get(&key);
        if artifact.is_some() {
            saturating_increment(&self.counters.hits);
        } else {
            saturating_increment(&self.counters.misses);
        }
        artifact
    }

    #[cfg(test)]
    pub(crate) fn insert(
        &self,
        canonical_path: PathBuf,
        source_hash: [u8; 32],
        artifact: Arc<PreparedModuleArtifact>,
    ) -> Arc<PreparedModuleArtifact> {
        self.insert_with_context(
            canonical_path,
            source_hash,
            &ModuleCompilationContext::default(),
            artifact,
        )
    }

    pub(crate) fn insert_with_context(
        &self,
        canonical_path: PathBuf,
        source_hash: [u8; 32],
        compilation_context: &ModuleCompilationContext,
        artifact: Arc<PreparedModuleArtifact>,
    ) -> Arc<PreparedModuleArtifact> {
        let key = PreparedModuleCacheKey::with_context(
            canonical_path,
            source_hash,
            artifact.provenance,
            compilation_context,
        );
        match self.entries.get_value_or_guard(&key, None) {
            GuardResult::Value(existing) => existing,
            GuardResult::Guard(guard) => {
                if guard.insert(Arc::clone(&artifact)).is_ok() {
                    saturating_increment(&self.counters.insertions);
                }
                artifact
            }
            GuardResult::Timeout => unreachable!("an unbounded cache wait cannot time out"),
        }
    }

    fn prepare_exact_key(
        &self,
        key: &PreparedModuleCacheKey,
        recorder: Option<&ModulePhaseRecorder>,
        prepare: impl FnOnce() -> Result<Arc<PreparedModuleArtifact>, VmError>,
    ) -> Result<Arc<PreparedModuleArtifact>, VmError> {
        let prepared = {
            let _load_span = recorder.map(ModulePhaseRecorder::load_span);
            self.entries.get(key)
        };
        if let Some(prepared) = prepared {
            saturating_increment(&self.counters.hits);
            return Ok(prepared);
        }
        saturating_increment(&self.counters.misses);

        let guarded = {
            let _load_span = recorder.map(ModulePhaseRecorder::load_span);
            self.entries.get_value_or_guard(key, None)
        };
        match guarded {
            GuardResult::Value(prepared) => Ok(prepared),
            GuardResult::Guard(guard) => {
                let prepared = prepare()?;
                if guard.insert(Arc::clone(&prepared)).is_ok() {
                    saturating_increment(&self.counters.insertions);
                }
                Ok(prepared)
            }
            GuardResult::Timeout => unreachable!("an unbounded cache wait cannot time out"),
        }
    }

    pub(crate) fn prepare(
        &self,
        source_path: &Path,
        canonical_path: &Path,
        source: &ModuleSource,
        compilation_context: Option<&ModuleCompilationContext>,
        recorder: Option<&ModulePhaseRecorder>,
        provenance: ModuleProvenance,
    ) -> Result<Arc<PreparedModuleArtifact>, VmError> {
        let source_hash = {
            let _load_span = recorder.map(ModulePhaseRecorder::load_span);
            source.sha256()
        };
        let compilation_context = match compilation_context {
            Some(context) => context.clone(),
            None => module_compilation_context_for_source(source_path, source.as_str())?,
        };
        let key = PreparedModuleCacheKey::with_context(
            canonical_path.to_path_buf(),
            source_hash,
            provenance,
            &compilation_context,
        );
        self.prepare_exact_key(&key, recorder, || {
            // Disk cache hits skip parse + compile. The scoped prepared cache
            // additionally skips deserialization and chunk hydration on later
            // fresh VMs without sharing any runtime module state.
            // Every provenance shares one cache path. The cache key carries the
            // authority, so the on-disk identity already separates a trusted
            // artifact from an ordinary one: they hash to different shared-cache
            // filenames, and an adjacent artifact found by path fails the other
            // authority's header check. Before the key had that field, the only
            // thing keeping privileged bytecode out of an ordinary reader's
            // reach was this branch skipping the cache entirely, which also
            // meant a trusted graph recompiled from source on every process.
            let cached = {
                let lookup = {
                    let _load_span = recorder.map(ModulePhaseRecorder::load_span);
                    crate::bytecode_cache::load_module(
                        source_path,
                        source,
                        &compilation_context,
                        provenance,
                    )
                };
                if let Some(artifact) = lookup.artifact {
                    artifact
                } else {
                    let mut compile_span = recorder.map(ModulePhaseRecorder::compile_span);
                    // Same `provenance` that keyed the lookup above, so the
                    // artifact stored on a miss can only be found by a reader
                    // asking for the authority it was compiled under.
                    let compiled = if provenance == ModuleProvenance::TrustedHostDispatch {
                        compile_trusted_host_dispatch_module_artifact_from_source_with_context(
                            source_path,
                            source.as_str(),
                            &compilation_context,
                        )?
                    } else {
                        compile_module_artifact_from_source_with_context(
                            source_path,
                            source.as_str(),
                            &compilation_context,
                        )?
                    };
                    if let Some(span) = &mut compile_span {
                        span.mark_compile_succeeded();
                    }
                    drop(compile_span);
                    if let Err(err) = crate::bytecode_cache::store_module(&lookup.key, &compiled) {
                        if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                            eprintln!(
                                "[harn] module cache write skipped for {}: {err}",
                                source_path.display()
                            );
                        }
                    }
                    compiled
                }
            };
            let prepared = {
                let _load_span = recorder.map(ModulePhaseRecorder::load_span);
                Arc::new(PreparedModuleArtifact::from_cached(cached))
            };
            Ok(prepared)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_artifact::{compile_module_artifact_from_source, ModuleImportBinding};
    use crate::module_source::ModuleSource;
    use harn_parser::TypeExpr;
    use std::sync::Barrier;

    fn named_list_element(type_expr: &Option<TypeExpr>) -> &str {
        match type_expr {
            Some(TypeExpr::List(inner)) => match inner.as_ref() {
                TypeExpr::Named(name) => name,
                other => panic!("expected named list element, got {other:?}"),
            },
            other => panic!("expected list parameter type, got {other:?}"),
        }
    }

    fn empty_artifact_with_provenance(provenance: ModuleProvenance) -> Arc<PreparedModuleArtifact> {
        Arc::new(PreparedModuleArtifact::from_cached(ModuleArtifact {
            provenance,
            imports: Vec::new(),
            type_schema_init_chunks: Vec::new(),
            init_chunk: None,
            functions: BTreeMap::new(),
            public_exports: BTreeMap::new(),
            public_value_names: Default::default(),
            public_type_names: Default::default(),
        }))
    }

    fn empty_artifact() -> Arc<PreparedModuleArtifact> {
        empty_artifact_with_provenance(ModuleProvenance::User)
    }

    #[test]
    fn ordinary_lookup_cannot_reuse_privileged_wire_bytecode() {
        let cache = PreparedModuleCache::default();
        let source = ModuleSource::from_text("const value = 1");
        let _ = cache.insert(
            PathBuf::from("same.harn"),
            source.sha256(),
            empty_artifact_with_provenance(ModuleProvenance::PrivilegedWire),
        );
        assert!(
            cache
                .get(
                    Path::new("same.harn"),
                    source.sha256(),
                    ModuleProvenance::User,
                )
                .is_none(),
            "user module lookup must be provenance-separated"
        );
        assert!(cache
            .get(
                Path::new("same.harn"),
                source.sha256(),
                ModuleProvenance::PrivilegedWire,
            )
            .is_some());
    }

    #[test]
    fn bounded_cache_rejects_a_one_off_scan_without_leaking_artifacts() {
        let cache = PreparedModuleCache::with_capacity(NonZeroUsize::new(1).unwrap());
        let first_source = ModuleSource::from_text("pub fn first() { 1 }");
        let second_source = ModuleSource::from_text("pub fn second() { 2 }");
        let first = empty_artifact();
        let first_weak = Arc::downgrade(&first);
        drop(cache.insert(
            PathBuf::from("first.harn"),
            first_source.sha256(),
            Arc::clone(&first),
        ));
        drop(first);

        // quick_cache's scan-resistant admission deliberately preserves the
        // resident hot key when a new key appears only once at capacity.
        let scanned = empty_artifact();
        let scanned_weak = Arc::downgrade(&scanned);
        drop(cache.insert(
            PathBuf::from("second.harn"),
            second_source.sha256(),
            Arc::clone(&scanned),
        ));
        drop(scanned);

        assert!(cache
            .get(
                Path::new("first.harn"),
                first_source.sha256(),
                ModuleProvenance::User,
            )
            .is_some());
        assert!(cache
            .get(
                Path::new("second.harn"),
                second_source.sha256(),
                ModuleProvenance::User,
            )
            .is_none());
        assert!(first_weak.upgrade().is_some());
        assert!(scanned_weak.upgrade().is_none());
        assert_eq!(cache.stats().insertions, 2);
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().entries, 1);

        drop(cache);
        assert!(first_weak.upgrade().is_none());
    }

    #[test]
    fn cache_key_separates_compiler_configuration() {
        let path = PathBuf::from("module.harn");
        let key = PreparedModuleCacheKey::new(
            path,
            ModuleSource::from_text("pub fn value() { 1 }").sha256(),
            ModuleProvenance::User,
        );
        let mut other_compiler = key.clone();
        other_compiler.optimizations_enabled = !key.optimizations_enabled;

        assert_ne!(key, other_compiler);
    }

    #[test]
    fn cache_counters_saturate_instead_of_wrapping() {
        let counter = AtomicU64::new(u64::MAX);
        saturating_increment(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn cache_key_separates_imported_symbol_compilation_context() {
        let source_path = PathBuf::from("context-sensitive.harn");
        let source = ModuleSource::from_text(
            r#"
import "./library"

pub fn exercise(value: any) -> string {
  match value {
    Color.Ready(message) -> { return message }
    _ -> { return "fallback" }
  }
}
"#,
        );
        let without_imported_enum = compile_module_artifact_from_source_with_context(
            &source_path,
            source.as_str(),
            &ModuleCompilationContext::default(),
        )
        .expect("compile dynamically-resolved pattern");
        let imported_enum_context =
            ModuleCompilationContext::new(["Color".to_string()], Vec::<String>::new());
        let with_imported_enum = compile_module_artifact_from_source_with_context(
            &source_path,
            source.as_str(),
            &imported_enum_context,
        )
        .expect("compile imported-enum-resolved pattern");
        assert_ne!(
            postcard::to_allocvec(&without_imported_enum.functions["exercise"])
                .expect("serialize dynamically-resolved function"),
            postcard::to_allocvec(&with_imported_enum.functions["exercise"])
                .expect("serialize imported-enum-resolved function"),
            "the imported enum projection must demonstrably alter bytecode"
        );

        let cache = PreparedModuleCache::default();
        let without_imported_enum = cache
            .prepare(
                &source_path,
                &source_path,
                &source,
                Some(&ModuleCompilationContext::default()),
                None,
                ModuleProvenance::User,
            )
            .expect("prepare dynamically-resolved artifact");
        let with_imported_enum = cache
            .prepare(
                &source_path,
                &source_path,
                &source,
                Some(&imported_enum_context),
                None,
                ModuleProvenance::User,
            )
            .expect("prepare imported-enum-resolved artifact");

        assert!(
            !Arc::ptr_eq(&without_imported_enum, &with_imported_enum),
            "one source/path/provenance with distinct imported projections must not alias"
        );
        assert_ne!(
            postcard::to_allocvec(&without_imported_enum.functions["exercise"].freeze_for_cache(),)
                .expect("serialize cached dynamically-resolved function"),
            postcard::to_allocvec(&with_imported_enum.functions["exercise"].freeze_for_cache(),)
                .expect("serialize cached imported-enum-resolved function")
        );
        assert_eq!(cache.stats().insertions, 2);
    }

    #[test]
    fn dropping_last_cache_handle_releases_prepared_artifacts() {
        let cache = PreparedModuleCache::default();
        let path = PathBuf::from("module.harn");
        let source = ModuleSource::from_text("pub fn value() { 1 }");
        let artifact = empty_artifact();
        let weak = Arc::downgrade(&artifact);
        let _ = cache.insert(path, source.sha256(), artifact);
        let clone = cache.clone();

        drop(cache);
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn concurrent_identical_misses_compile_one_immutable_artifact() {
        const WORKERS: usize = 8;

        let cache = PreparedModuleCache::default();
        let source = Arc::new(ModuleSource::from_text(
            (0..128)
                .map(|index| format!("pub fn value_{index}() {{ return {index} }}\n"))
                .collect::<String>(),
        ));
        let source_path = Arc::new(PathBuf::from("shared-runtime-module.harn"));
        let start = Arc::new(Barrier::new(WORKERS + 1));
        let mut handles = Vec::with_capacity(WORKERS);

        for _ in 0..WORKERS {
            let cache = cache.clone();
            let source = Arc::clone(&source);
            let source_path = Arc::clone(&source_path);
            let start = Arc::clone(&start);
            handles.push(std::thread::spawn(move || {
                let recorder = ModulePhaseRecorder::new();
                start.wait();
                let artifact = cache
                    .prepare(
                        &source_path,
                        &source_path,
                        &source,
                        None,
                        Some(&recorder),
                        ModuleProvenance::TrustedHostDispatch,
                    )
                    .expect("compile shared immutable module");
                (artifact, recorder.snapshot())
            }));
        }

        start.wait();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("module compiler worker joins"))
            .collect::<Vec<_>>();
        let first = &outcomes[0].0;

        assert!(
            outcomes
                .iter()
                .all(|(artifact, _)| Arc::ptr_eq(first, artifact)),
            "all workers must consume the same immutable prepared artifact"
        );
        assert_eq!(
            outcomes
                .iter()
                .map(|(_, phases)| phases.modules_compiled)
                .sum::<u64>(),
            1,
            "one exact cache key must have one compilation owner regardless of worker count"
        );
        assert_eq!(cache.stats().insertions, 1);
    }

    #[test]
    fn failed_preparation_is_not_cached_or_poisoned() {
        let cache = PreparedModuleCache::default();
        let key = PreparedModuleCacheKey::new(
            PathBuf::from("recoverable.harn"),
            ModuleSource::from_text("pub fn value() { return 1 }").sha256(),
            ModuleProvenance::TrustedHostDispatch,
        );

        let failed = cache.prepare_exact_key(&key, None, || {
            Err(VmError::Runtime(
                "synthetic compilation failure".to_string(),
            ))
        });
        assert!(
            matches!(failed, Err(VmError::Runtime(message)) if message == "synthetic compilation failure")
        );
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().insertions, 0);

        let expected = empty_artifact_with_provenance(ModuleProvenance::TrustedHostDispatch);
        let prepared = cache
            .prepare_exact_key(&key, None, || Ok(Arc::clone(&expected)))
            .expect("a failed owner must release the exact-key preparation slot");

        assert!(Arc::ptr_eq(&prepared, &expected));
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().insertions, 1);
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn hydration_moves_module_owned_storage() {
        let source = r#"
import { assert_eq } from "std/testing"
pub type Result = {value: int}
pub const value = 1
pub fn answer(items: list<string>) {
  fn nested() { return 42 }
  return items
}
"#;
        let artifact = compile_module_artifact_from_source(Path::new("owned.harn"), source)
            .expect("compile typed module artifact");

        let imports = artifact.imports.as_ptr();
        let import_path = artifact.imports[0].path.as_ptr();
        let ModuleImportBinding::Selected(selected) = &artifact.imports[0].binding else {
            panic!("expected selective import");
        };
        let selected_names = selected.as_ptr();
        let selected_name = selected[0].as_ptr();
        let init_code = artifact.init_chunk.as_ref().unwrap().code.as_ptr();
        let schema_init_codes = artifact
            .type_schema_init_chunks
            .iter()
            .map(|chunk| chunk.code.as_ptr())
            .collect::<Vec<_>>();
        let (function_key, function) = artifact.functions.first_key_value().unwrap();
        let function_key = function_key.as_ptr();
        let function_name = function.name.clone();
        let function_code = function.chunk.code.as_ptr();
        let param_name = function.params[0].name.as_ptr();
        let param_type_name = named_list_element(&function.params[0].type_expr).as_ptr();
        let nested_name = function.chunk.functions[0].name.clone();
        let nested_code = function.chunk.functions[0].chunk.code.as_ptr();
        let public_export_name = artifact
            .public_exports
            .get_key_value("answer")
            .unwrap()
            .0
            .as_ptr();
        let public_export_kind = *artifact.public_exports.get("answer").unwrap();
        let public_value_name = artifact.public_value_names.get("value").unwrap().as_ptr();
        let public_type_name = artifact.public_type_names.get("Result").unwrap().as_ptr();
        let hydrated = PreparedModuleArtifact::from_cached(artifact);

        assert_eq!(hydrated.imports.as_ptr(), imports);
        assert_eq!(hydrated.imports[0].path.as_ptr(), import_path);
        let ModuleImportBinding::Selected(selected) = &hydrated.imports[0].binding else {
            panic!("expected selective import");
        };
        assert_eq!(selected.as_ptr(), selected_names);
        assert_eq!(selected[0].as_ptr(), selected_name);
        assert_eq!(
            hydrated.init_chunk.as_ref().unwrap().code.as_ptr(),
            init_code
        );
        assert_eq!(
            hydrated
                .type_schema_init_chunks
                .iter()
                .map(|chunk| chunk.code.as_ptr())
                .collect::<Vec<_>>(),
            schema_init_codes
        );
        let (hydrated_function_key, hydrated_function) =
            hydrated.functions.first_key_value().unwrap();
        assert_eq!(hydrated_function_key.as_ptr(), function_key);
        // Function names convert into a shared `HarnStr` at hydration (one
        // short copy) so per-call consumers can share them; compare by value.
        assert_eq!(hydrated_function.name.as_str(), function_name);
        assert_eq!(hydrated_function.chunk.code.as_ptr(), function_code);
        assert_eq!(hydrated_function.params[0].name.as_ptr(), param_name);
        assert_eq!(
            named_list_element(&hydrated_function.params[0].type_expr).as_ptr(),
            param_type_name
        );
        assert_eq!(
            hydrated_function.chunk.functions[0].name.as_str(),
            nested_name
        );
        assert_eq!(
            hydrated_function.chunk.functions[0].chunk.code.as_ptr(),
            nested_code
        );
        assert_eq!(
            hydrated
                .public_exports
                .get_key_value("answer")
                .unwrap()
                .0
                .as_ptr(),
            public_export_name
        );
        assert_eq!(
            hydrated.public_exports.get("answer"),
            Some(&public_export_kind)
        );
        assert_eq!(
            hydrated.public_value_names.get("value").unwrap().as_ptr(),
            public_value_name
        );
        assert_eq!(
            hydrated.public_type_names.get("Result").unwrap().as_ptr(),
            public_type_name
        );
    }
}
