//! Scoped cache of immutable, hydrated module bytecode.
//!
//! Prepared modules deliberately stop before runtime instantiation. Each VM
//! still receives fresh closures, function registries, module state, and init
//! execution; only the serialized-to-runtime bytecode conversion is reused.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_modules::DefKind;
use parking_lot::Mutex;

use crate::chunk::{Chunk, CompiledFunction};
use crate::module_artifact::{
    compile_module_artifact_from_source, compile_module_artifact_from_source_with_imported_enums,
    compile_trusted_host_dispatch_module_artifact_from_source,
    compile_trusted_host_dispatch_module_artifact_from_source_with_imported_enums, ModuleArtifact,
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PreparedModuleCacheKey {
    canonical_path: PathBuf,
    source_hash: [u8; 32],
    provenance: ModuleProvenance,
    harn_version: &'static str,
    codegen_fingerprint: &'static str,
    optimizations_enabled: bool,
}

impl PreparedModuleCacheKey {
    /// `source_hash` is the same SHA-256 that names the module's on-disk
    /// artifact. Keying on it rather than a second digest of the same bytes
    /// means a warm module load hashes its source once, and lets a caller
    /// holding a recorded digest find a prepared artifact without the bytes.
    fn new(canonical_path: PathBuf, source_hash: [u8; 32], provenance: ModuleProvenance) -> Self {
        Self {
            canonical_path,
            source_hash,
            provenance,
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
            let mut imported_enum_candidates = graph
                .imported_names_by_kind_for_file(&path, DefKind::Enum)
                .unwrap_or_default()
                .into_iter()
                .collect::<Vec<_>>();
            imported_enum_candidates.sort_unstable();
            let canonical = harn_modules::canonical_path(&path);
            let _ = self.prepare(
                &path,
                &canonical,
                &source,
                Some(&imported_enum_candidates),
                Some(&recorder),
                provenance,
            );
        }

        recorder.snapshot()
    }

    pub(crate) fn get(
        &self,
        canonical_path: &Path,
        source_hash: [u8; 32],
        provenance: ModuleProvenance,
    ) -> Option<Arc<PreparedModuleArtifact>> {
        let key =
            PreparedModuleCacheKey::new(canonical_path.to_path_buf(), source_hash, provenance);
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
        source_hash: [u8; 32],
        artifact: Arc<PreparedModuleArtifact>,
    ) -> Arc<PreparedModuleArtifact> {
        let key = PreparedModuleCacheKey::new(canonical_path, source_hash, artifact.provenance);
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

    pub(crate) fn prepare(
        &self,
        source_path: &Path,
        canonical_path: &Path,
        source: &ModuleSource,
        imported_enum_candidates: Option<&[String]>,
        recorder: Option<&ModulePhaseRecorder>,
        provenance: ModuleProvenance,
    ) -> Result<Arc<PreparedModuleArtifact>, VmError> {
        let prepared = {
            let _load_span = recorder.map(ModulePhaseRecorder::load_span);
            self.get(canonical_path, source.sha256(), provenance)
        };
        if let Some(prepared) = prepared {
            return Ok(prepared);
        }

        // Disk cache hits skip parse + compile. The scoped prepared cache
        // additionally skips deserialization and chunk hydration on later
        // fresh VMs without sharing any runtime module state.
        let cached = if provenance == ModuleProvenance::TrustedHostDispatch {
            let mut compile_span = recorder.map(ModulePhaseRecorder::compile_span);
            let compiled = match imported_enum_candidates {
                Some(candidates) => {
                    compile_trusted_host_dispatch_module_artifact_from_source_with_imported_enums(
                        source_path,
                        source.as_str(),
                        candidates.iter().cloned(),
                    )?
                }
                None => compile_trusted_host_dispatch_module_artifact_from_source(
                    source_path,
                    source.as_str(),
                )?,
            };
            if let Some(span) = &mut compile_span {
                span.mark_compile_succeeded();
            }
            drop(compile_span);
            compiled
        } else {
            // Only ordinary user bytecode enters the process-wide disk cache.
            // Trusted host-dispatch artifacts remain in this explicitly scoped,
            // provenance-keyed in-memory cache.
            let lookup = {
                let _load_span = recorder.map(ModulePhaseRecorder::load_span);
                crate::bytecode_cache::load_module(source_path, source)
            };
            if let Some(artifact) = lookup.artifact {
                artifact
            } else {
                let mut compile_span = recorder.map(ModulePhaseRecorder::compile_span);
                let compiled = match imported_enum_candidates {
                    Some(candidates) => compile_module_artifact_from_source_with_imported_enums(
                        source_path,
                        source.as_str(),
                        candidates.iter().cloned(),
                    )?,
                    None => compile_module_artifact_from_source(source_path, source.as_str())?,
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
        Ok(self.insert(canonical_path.to_path_buf(), source.sha256(), prepared))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_artifact::{compile_module_artifact_from_source, ModuleImportBinding};
    use crate::module_source::ModuleSource;
    use harn_parser::TypeExpr;

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
    fn bounded_cache_evicts_oldest_exact_key() {
        let cache = PreparedModuleCache::with_capacity(NonZeroUsize::new(1).unwrap());
        let first_source = ModuleSource::from_text("pub fn first() { 1 }");
        let second_source = ModuleSource::from_text("pub fn second() { 2 }");
        let first = empty_artifact();
        let _ = cache.insert(PathBuf::from("first.harn"), first_source.sha256(), first);
        let _ = cache.insert(
            PathBuf::from("second.harn"),
            second_source.sha256(),
            empty_artifact(),
        );

        assert!(cache
            .get(
                Path::new("first.harn"),
                first_source.sha256(),
                ModuleProvenance::User,
            )
            .is_none());
        assert!(cache
            .get(
                Path::new("second.harn"),
                second_source.sha256(),
                ModuleProvenance::User,
            )
            .is_some());
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().entries, 1);
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
        let function_name = function.name.as_ptr();
        let function_code = function.chunk.code.as_ptr();
        let param_name = function.params[0].name.as_ptr();
        let param_type_name = named_list_element(&function.params[0].type_expr).as_ptr();
        let nested_name = function.chunk.functions[0].name.as_ptr();
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
        assert_eq!(hydrated_function.name.as_ptr(), function_name);
        assert_eq!(hydrated_function.chunk.code.as_ptr(), function_code);
        assert_eq!(hydrated_function.params[0].name.as_ptr(), param_name);
        assert_eq!(
            named_list_element(&hydrated_function.params[0].type_expr).as_ptr(),
            param_type_name
        );
        assert_eq!(
            hydrated_function.chunk.functions[0].name.as_ptr(),
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
