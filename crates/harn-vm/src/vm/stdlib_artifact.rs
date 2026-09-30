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
    compile_embedded_stdlib_module_artifact_from_source_with_context,
    module_compilation_context_for_source, ModuleProvenance,
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
                    ModuleProvenance::EmbeddedStdlib,
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
            let compiled = compile_embedded_stdlib_module_artifact_from_source_with_context(
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

/// What [`warm_embedded_stdlib`] prepared.
#[derive(Debug, Default)]
pub struct StdlibWarmReport {
    /// Modules in this binary's stdlib catalog.
    pub modules: usize,
    /// Modules now prepared. Short of `modules` when a module failed or a warm
    /// thread could not start.
    pub warmed: usize,
    /// Modules that did not compile on their own, with the reason. A failure
    /// here only means that module stays lazy; importing it still reports the
    /// real error in the importing process.
    pub failed: Vec<(String, String)>,
}

/// Prepare every embedded stdlib module once, across `threads` threads, so the
/// on-disk bytecode cache is warm before this binary fans out processes.
///
/// Without it, every child that imports a stdlib module the cache has not seen
/// compiles that module itself, concurrently with its siblings. An unoptimized
/// build spends about 20 s of CPU compiling the agent stack, and a test deadline
/// then measures that stampede instead of the test (harn#8575).
pub fn warm_embedded_stdlib(threads: usize) -> StdlibWarmReport {
    let sources = harn_stdlib::STDLIB_SOURCES;
    let next = std::sync::atomic::AtomicUsize::new(0);
    let warmed = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, sources.len().max(1)) {
            // Compiling recurses over program structure, so each warm thread
            // needs the VM stack contract, not the 2 MiB default.
            let builder = std::thread::Builder::new()
                .name("harn-stdlib-warm".to_owned())
                .stack_size(crate::RUNTIME_STACK_SIZE);
            let spawned = builder.spawn_scoped(scope, || loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(entry) = sources.get(index) else {
                    break;
                };
                let synthetic = PathBuf::from(format!("<stdlib>/{}.harn", entry.module));
                match stdlib_module_artifact(entry.module, &synthetic, entry.source, None) {
                    Ok(_) => {
                        warmed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(error) => failed
                        .lock()
                        .expect("stdlib warm failure list poisoned")
                        .push((entry.module.to_string(), error.to_string())),
                }
            });
            // Warming is an optimization: with fewer threads the remaining
            // modules warm more slowly, and with none they stay lazy.
            if spawned.is_err() {
                break;
            }
        }
    });
    StdlibWarmReport {
        modules: sources.len(),
        warmed: warmed.into_inner(),
        failed: failed
            .into_inner()
            .expect("stdlib warm failure list poisoned"),
    }
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
