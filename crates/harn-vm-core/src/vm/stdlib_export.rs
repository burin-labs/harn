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
    /// One artifact per compiled content key, sorted by file name. Logical
    /// module aliases with identical sources share the same immutable blob.
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
    let artifacts = artifacts
        .into_inner()
        .expect("stdlib export artifact list poisoned");
    let artifacts =
        unique_artifacts(artifacts).expect("stdlib content keys must identify one blob");
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

fn unique_artifacts(
    mut artifacts: Vec<CompiledStdlibArtifact>,
) -> Result<Vec<CompiledStdlibArtifact>, String> {
    artifacts.sort_by(|left, right| {
        left.file_name
            .cmp(&right.file_name)
            .then_with(|| left.module.cmp(&right.module))
    });
    for pair in artifacts.windows(2) {
        if pair[0].file_name == pair[1].file_name && pair[0].bytes != pair[1].bytes {
            return Err(format!(
                "stdlib modules {} and {} have conflicting blobs for {}",
                pair[0].module, pair[1].module, pair[0].file_name
            ));
        }
    }
    artifacts.dedup_by(|left, right| left.file_name == right.file_name);
    Ok(artifacts)
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

#[cfg(test)]
mod tests {
    use super::{compile_one, unique_artifacts};

    #[test]
    fn content_aliases_share_one_blob_and_conflicts_are_refused() {
        let first = compile_one("alias/first", "pub fn answer() -> int { return 42 }")
            .expect("compile first logical module");
        let second = compile_one("alias/second", "pub fn answer() -> int { return 42 }")
            .expect("compile second logical module");
        assert_eq!(first.file_name, second.file_name);
        assert_eq!(
            first.bytes, second.bytes,
            "content artifacts must be relocatable"
        );
        let unique =
            unique_artifacts(vec![second.clone(), first.clone()]).expect("identical aliases");
        assert_eq!(unique, vec![first.clone()]);

        let mut conflict = second;
        conflict.bytes.push(0);
        assert!(unique_artifacts(vec![first, conflict])
            .expect_err("equal keys with different payloads cannot silently choose a winner")
            .contains("conflicting blobs"));
    }
}
