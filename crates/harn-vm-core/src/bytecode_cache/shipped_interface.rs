//! Relocatable imported interfaces, validated by the ordinary graph identity.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::*;

/// No host paths or timestamps enter a shipped payload. The graph key proves
/// content and resolution; these `/`-separated labels associate its typed
/// projections with the current machine's observations.
#[derive(Serialize, Deserialize)]
struct ShippedInterface {
    entry: ModuleCompilationContext,
    files: BTreeMap<String, ModuleCompilationContext>,
    package_import_aliases: Vec<String>,
}

fn key(source: &str, context_hash: [u8; 32]) -> CacheKey {
    CacheKey {
        source_hash: sha256(source.as_bytes()),
        context_hash,
        harn_version: Cow::Borrowed(HARN_VERSION),
        compiler_contracts: compiler_contracts::fingerprint(),
        compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
        provenance: ModuleProvenance::User,
    }
}

fn label(entry: &Path, path: &Path) -> Option<String> {
    graph::relocatable_label(entry.parent()?, path)
}

/// A compilation projection and its relocatable artifact, ready to publish
/// once the caller's source validation has succeeded.
pub struct PrecompiledInterface {
    context: ModuleCompilationContext,
    encoded: Option<Vec<u8>>,
}

impl PrecompiledInterface {
    pub fn context(&self) -> &ModuleCompilationContext {
        &self.context
    }

    /// Persist new output, retaining a valid existing artifact byte for byte.
    pub fn store_at(&self, destination: &Path) -> io::Result<()> {
        if let Some(bytes) = &self.encoded {
            ensure_parent_dir(destination)?;
            crate::atomic_io::atomic_write_with_mode(destination, bytes, ARTIFACT_MODE)?;
        }
        Ok(())
    }
}

/// Prepare a relocatable interface using the runtime's graph owner. Existing
/// valid output is reused without parsing or rewriting it.
pub fn prepare_precompiled_interface(
    destination: &Path,
    source_path: &Path,
    source: &str,
) -> io::Result<PrecompiledInterface> {
    if let Some(result) = recall_at(destination, source_path, source) {
        return Ok(PrecompiledInterface {
            context: result
                .entry_compilation_context
                .expect("validated interface"),
            encoded: None,
        });
    }
    let result = walk_import_graph_fingerprinted(
        source_path,
        source,
        CODEGEN_FINGERPRINT,
        GraphOutputs::ManifestAndEntryContext,
    );
    let context = result
        .entry_compilation_context
        .ok_or_else(|| io::Error::other("cannot derive a complete module interface"))?;
    let manifest = result
        .manifest
        .ok_or_else(|| io::Error::other("cannot observe the module interface dependency graph"))?;
    let files = manifest
        .files
        .iter()
        .map(|file| {
            label(&manifest.entry, &file.path)
                .map(|label| (label, file.compilation_context.clone()))
        })
        .collect::<Option<BTreeMap<_, _>>>()
        .ok_or_else(|| io::Error::other("module interface dependencies span filesystem roots"))?;
    let payload = serialize_cache_payload(&ShippedInterface {
        entry: context.clone(),
        files,
        package_import_aliases: manifest.package_import_aliases,
    })?;
    let bytes = encode_artifact(
        &key(source, result.relocatable),
        KIND_MODULE_INTERFACE,
        &payload,
    );
    Ok(PrecompiledInterface {
        context,
        encoded: Some(bytes),
    })
}

pub(super) fn recall(source_path: &Path, source: &str) -> Option<GraphHashes> {
    if !cache_enabled() {
        return None;
    }
    let destination = adjacent_path_with_extension(source_path, INTERFACE_CACHE_EXTENSION)?;
    let result = recall_at(&destination, source_path, source);
    if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
        let verdict = if result.is_some() { "hit" } else { "miss" };
        eprintln!(
            "[harn] adjacent module interface {verdict}: {}",
            destination.display()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_interface_identity_rejects_foreign_stale_and_resolution_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("entry.harn");
        let dep = dir.path().join("dep.harn");
        let source = "import \"./dep\"\npub fn value() { return Color.Ready }\n";
        std::fs::write(&root, source).unwrap();
        std::fs::write(&dep, "pub enum Color { Ready Empty }\n").unwrap();
        let destination = root.with_extension(INTERFACE_CACHE_EXTENSION);
        assert!(
            recall_at(&destination, &root, source).is_none(),
            "missing artifact control"
        );
        let artifact = prepare_precompiled_interface(&destination, &root, source).unwrap();
        artifact.store_at(&destination).unwrap();
        let original = std::fs::read(&destination).unwrap();
        let first = recall_at(&destination, &root, source).expect("valid interface reached");
        assert_eq!(
            first.entry_compilation_context.unwrap(),
            *artifact.context()
        );
        let cache_key = CacheKey::from_relocatable_source(&root, source);
        let header =
            read_header_if_matches(&destination, &cache_key, Some(&cache_key.context_hash))
                .unwrap()
                .unwrap();
        let foreign = encode_artifact_fingerprinted(
            &cache_key,
            KIND_MODULE_INTERFACE,
            &header.payload,
            "another-compiler",
        );
        std::fs::write(&destination, foreign).unwrap();
        assert!(
            recall_at(&destination, &root, source).is_none(),
            "foreign compiler control"
        );
        std::fs::write(&destination, &original).unwrap();
        std::fs::write(&dep, "pub enum Shade { Ready Empty }\n").unwrap();
        assert!(
            recall_at(&destination, &root, source).is_none(),
            "dependency content control"
        );
        std::fs::write(&dep, "pub enum Color { Ready Empty }\n").unwrap();
        assert!(
            recall_at(&destination, &root, source).is_some(),
            "restored content must hit"
        );
        std::fs::create_dir(dir.path().join("dep")).unwrap();
        assert!(
            recall_at(&destination, &root, source).is_none(),
            "resolver shadow control"
        );
    }

    #[test]
    fn shipped_payload_is_reproducible_under_different_roots() {
        let mut artifacts = Vec::new();
        for _ in 0..2 {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("nested")).unwrap();
            let root = dir.path().join("entry.harn");
            let source = "import \"./nested/dep\"\npub fn value() { return Color.Ready }\n";
            std::fs::write(&root, source).unwrap();
            std::fs::write(
                dir.path().join("nested/dep.harn"),
                "pub enum Color { Ready Empty }\n",
            )
            .unwrap();
            let destination = root.with_extension(INTERFACE_CACHE_EXTENSION);
            prepare_precompiled_interface(&destination, &root, source)
                .unwrap()
                .store_at(&destination)
                .unwrap();
            let key = CacheKey::from_relocatable_source(&root, source);
            let header = read_header_if_matches(&destination, &key, Some(&key.context_hash))
                .unwrap()
                .unwrap();
            let payload: ShippedInterface = deserialize_cache_payload(&header.payload).unwrap();
            assert_eq!(
                payload.files.keys().collect::<Vec<_>>(),
                [&"nested/dep.harn".to_string()]
            );
            artifacts.push(std::fs::read(&destination).unwrap());
        }
        assert_eq!(
            artifacts[0], artifacts[1],
            "root and timestamps must not enter shipped bytes"
        );
    }
}

fn recall_at(destination: &Path, source_path: &Path, source: &str) -> Option<GraphHashes> {
    // Missing output must not make the ordinary warm shared-cache path walk.
    if !destination.is_file() {
        return None;
    }
    let mut result = walk_import_graph_fingerprinted(
        source_path,
        source,
        CODEGEN_FINGERPRINT,
        GraphOutputs::InterfaceProof,
    );
    let key = key(source, result.relocatable);
    let header = read_header_if_matches(destination, &key, Some(&key.context_hash)).ok()??;
    if header.kind != KIND_MODULE_INTERFACE {
        return None;
    }
    let payload: ShippedInterface = deserialize_cache_payload(&header.payload).ok()?;
    let manifest = result.manifest.as_mut()?;
    if payload.files.len() != manifest.files.len() {
        return None;
    }
    for file in &mut manifest.files {
        file.compilation_context = payload
            .files
            .get(&label(&manifest.entry, &file.path)?)?
            .clone();
    }
    manifest.package_import_aliases = payload.package_import_aliases;
    result.entry_compilation_context = Some(payload.entry);
    Some(result)
}
