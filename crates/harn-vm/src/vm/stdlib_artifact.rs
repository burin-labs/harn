//! Process-global cache of prepared stdlib module artifacts.
//!
//! Stdlib sources are embedded in the binary, so their content cannot change
//! between processes and every artifact is immutable for the life of the
//! build. That makes them the one module family worth holding in a static
//! cache with no invalidation story, which is why they are kept apart from
//! the user-module loading in [`super::modules`]: this cache is keyed by
//! embedded content, bounded by the stdlib catalog, and never observes a
//! filesystem edit.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use quick_cache::sync::{Cache, GuardResult};

use crate::bytecode_cache;
use crate::module_artifact::{
    compile_module_artifact_from_source_with_context, module_compilation_context_for_source,
    ModuleProvenance,
};
use crate::module_source::ModuleSource;
use crate::prepared_module::PreparedModuleArtifact;
use crate::value::VmError;

static STDLIB_MODULE_ARTIFACT_CACHE: OnceLock<Cache<String, Arc<PreparedModuleArtifact>>> =
    OnceLock::new();

fn stdlib_module_artifact_cache() -> &'static Cache<String, Arc<PreparedModuleArtifact>> {
    STDLIB_MODULE_ARTIFACT_CACHE.get_or_init(|| {
        // The key set is embedded in this exact binary and therefore bounded.
        // Sizing to its authoritative catalog keeps every immutable artifact
        // resident without a second capacity constant to drift.
        Cache::new(harn_stdlib::STDLIB_SOURCES.len().max(1))
    })
}

#[cfg(test)]
pub(super) fn reset_stdlib_module_artifact_cache() {
    stdlib_module_artifact_cache().clear();
}

#[cfg(test)]
pub(super) fn stdlib_module_artifact_cache_ptr(module: &str, source: &str) -> Option<usize> {
    let key = stdlib_artifact_cache_key(module, source);
    stdlib_module_artifact_cache()
        .get(&key)
        .map(|artifact| Arc::as_ptr(&artifact) as usize)
}

pub(super) fn stdlib_artifact_get_or_prepare(
    key: String,
    prepare: impl FnOnce() -> Result<Arc<PreparedModuleArtifact>, VmError>,
) -> Result<Arc<PreparedModuleArtifact>, VmError> {
    match stdlib_module_artifact_cache().get_value_or_guard(&key, None) {
        GuardResult::Value(artifact) => Ok(artifact),
        GuardResult::Guard(guard) => {
            let artifact = prepare()?;
            let _ = guard.insert(Arc::clone(&artifact));
            Ok(artifact)
        }
        GuardResult::Timeout => unreachable!("an unbounded stdlib cache wait cannot time out"),
    }
}

fn stdlib_artifact_cache_key(module: &str, source: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    module.hash(&mut hasher);
    source.hash(&mut hasher);
    format!("{module}:{:016x}", hasher.finish())
}

pub(super) fn stdlib_module_artifact(
    module: &str,
    synthetic: &Path,
    source: &'static str,
    recorder: Option<&super::ModulePhaseRecorder>,
) -> Result<Arc<PreparedModuleArtifact>, VmError> {
    let key = stdlib_artifact_cache_key(module, source);
    stdlib_artifact_get_or_prepare(key, || {
        // Stdlib modules are embedded in the binary so their content cannot
        // legitimately change between processes; that means the disk cache
        // for stdlib can use a synthetic source_path. The harn_version field
        // of the cache key gates correctness across releases.
        let embedded = ModuleSource::from_text(source);
        // Identity here is derived from the embedded bytes alone. Resolving the
        // imported interface first would lex and parse every stdlib module in
        // the closure on the warm path, in front of the lookup whose whole
        // purpose is to skip exactly that work.
        let lookup = {
            let _load_span = recorder.map(super::ModulePhaseRecorder::load_span);
            bytecode_cache::load_module_for_key(
                synthetic,
                bytecode_cache::CacheKey::from_embedded_stdlib_module_content_hash(
                    embedded.sha256(),
                    ModuleProvenance::User,
                ),
            )
        };
        let artifact = if let Some(artifact) = lookup.artifact {
            artifact
        } else {
            let mut compile_span = recorder.map(super::ModulePhaseRecorder::compile_span);
            // Only a miss needs the interface, and only because the compile
            // below consumes it. Keeping it inside the compile span also keeps
            // its cost attributable, which it was not when it ran ahead of the
            // lookup.
            let compilation_context = module_compilation_context_for_source(synthetic, source)?;
            let compiled = compile_module_artifact_from_source_with_context(
                synthetic,
                source,
                &compilation_context,
            )?;
            if let Some(span) = &mut compile_span {
                span.mark_compile_succeeded();
            }
            drop(compile_span);
            if let Err(err) = bytecode_cache::store_module(&lookup.key, &compiled) {
                if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                    eprintln!("[harn] stdlib module cache write skipped for {module}: {err}");
                }
            }
            compiled
        };

        let compiled = {
            let _load_span = recorder.map(super::ModulePhaseRecorder::load_span);
            Arc::new(PreparedModuleArtifact::from_cached(artifact))
        };
        Ok(compiled)
    })
}

pub(crate) fn prepare_stdlib_module_artifact(
    path: &Path,
    recorder: Option<&super::ModulePhaseRecorder>,
) -> Result<(), VmError> {
    let Some(module) = path.to_str().and_then(|path| path.strip_prefix("<std>/")) else {
        return Ok(());
    };
    let Some(source) = crate::stdlib_modules::get_stdlib_source(module) else {
        return Ok(());
    };
    let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
    stdlib_module_artifact(module, &synthetic, source, recorder).map(|_| ())
}
