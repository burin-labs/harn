//! Compile the embedded stdlib into the artifacts a host binary embeds.
//!
//! The runtime half is `bytecode_cache::embedded_stdlib`: it answers a stdlib
//! import from the registered table by the same key the shared cache uses.
//! This half is the producer `harn-stdlib-artifacts` runs in its build script.
//! It touches no cache: every module is compiled from the embedded source, so
//! what a build embeds depends only on the source tree that built it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::bytecode_cache::{self, CacheKey};
use crate::module_artifact::{
    compile_embedded_stdlib_module_artifact_from_source_with_context,
    module_compilation_context_for_source, ModuleProvenance,
};
use crate::module_source::ModuleSource;

/// One compiled stdlib module, under the file name the shared cache would
/// store it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledStdlibArtifact {
    pub module: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// What [`compile_embedded_stdlib_artifacts`] produced.
#[derive(Debug, Default)]
pub struct StdlibArtifactBuild {
    /// Modules in this binary's stdlib catalog.
    pub modules: usize,
    /// One artifact per module that compiled, sorted by file name.
    pub artifacts: Vec<CompiledStdlibArtifact>,
    /// Modules that did not compile on their own, with the reason. Such a
    /// module is simply absent from the table and compiles on import.
    pub failed: Vec<(String, String)>,
}

/// Compile every embedded stdlib module across `threads` threads.
///
/// Keys and code follow the process's compiler options exactly as a runtime
/// import would, so a producer that wants the default keys runs without
/// compiler-option overrides in its environment.
pub fn compile_embedded_stdlib_artifacts(threads: usize) -> StdlibArtifactBuild {
    let sources = harn_modules::stdlib_catalog().sources;
    let next = AtomicUsize::new(0);
    let artifacts = Mutex::new(Vec::with_capacity(sources.len()));
    let failed = Mutex::new(Vec::new());
    crate::runtime_stack::scope(|scope| {
        for _ in 0..threads.clamp(1, sources.len().max(1)) {
            // Compiling recurses over program structure, so each thread needs
            // the VM stack contract, not the 2 MiB default.
            let spawned = scope.spawn_named("harn-stdlib-export", || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(entry) = sources.get(index) else {
                    break;
                };
                match compile_one(entry.module, entry.source) {
                    Ok(artifact) => artifacts
                        .lock()
                        .expect("stdlib export artifact list poisoned")
                        .push(artifact),
                    Err(error) => failed
                        .lock()
                        .expect("stdlib export failure list poisoned")
                        .push((entry.module.to_string(), error)),
                }
            });
            if spawned.is_err() {
                break;
            }
        }
    });
    let mut artifacts = artifacts
        .into_inner()
        .expect("stdlib export artifact list poisoned");
    artifacts.sort_by(|left, right| left.file_name.cmp(&right.file_name));
    let mut failed = failed
        .into_inner()
        .expect("stdlib export failure list poisoned");
    failed.sort();
    StdlibArtifactBuild {
        modules: sources.len(),
        artifacts,
        failed,
    }
}

pub(crate) fn compile_one(module: &str, source: &str) -> Result<CompiledStdlibArtifact, String> {
    let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
    let key = CacheKey::from_embedded_stdlib_module_content_hash(
        ModuleSource::from_text(source).sha256(),
        ModuleProvenance::EmbeddedStdlib,
    );
    let context = module_compilation_context_for_source(&synthetic, source)
        .map_err(|error| error.to_string())?;
    let artifact = compile_embedded_stdlib_module_artifact_from_source_with_context(
        &synthetic, source, &context,
    )
    .map_err(|error| error.to_string())?;
    let bytes = bytecode_cache::serialize_module_artifact(&key, &artifact)
        .map_err(|error| error.to_string())?;
    Ok(CompiledStdlibArtifact {
        module: module.to_string(),
        file_name: key.module_filename(),
        bytes,
    })
}
