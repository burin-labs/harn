//! Shared projections from the bytecode cache's canonical graph walk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::context_manifest::ContextManifest;
use crate::module_artifact::ModuleCompilationContext;
use crate::VmError;

/// Recompute a store outcome from the graph as it exists after run setup.
///
/// Package materialization can change import resolution between the initial
/// cache probe and compilation. Writers use this constructor after setup so a
/// newly compiled chunk is never paired with the probe's older graph.
pub fn prepare_entry_store(source_path: &Path, source: &str) -> super::LookupOutcome {
    let source_hash = super::sha256(source.as_bytes());
    let (context_hash, manifest) = super::GraphWalk::new(source_path, source).finish();
    super::LookupOutcome {
        key: super::CacheKey {
            source_hash,
            context_hash,
            harn_version: std::borrow::Cow::Borrowed(super::HARN_VERSION),
            compiler_tag: super::compiler_options_tag(super::CompilerOptions::from_env()),
            provenance: super::ModuleProvenance::User,
        },
        chunk: None,
        manifest,
        link_table: None,
    }
}

/// Derive an entry interface and the graph capture that keeps it reusable.
///
/// A record stored by an earlier walk answers without parsing anything when
/// its manifest proves the closure unchanged; otherwise the walk runs and its
/// answer is stored for the next process.
pub(crate) fn derive_interface(
    source_path: &Path,
    source: &str,
) -> Result<(ModuleCompilationContext, Option<ContextManifest>), VmError> {
    let slot = super::InterfaceSlot::open(source_path, source);
    if let Some((context, manifest)) = slot.as_ref().and_then(super::InterfaceSlot::recall) {
        return Ok((context, Some(manifest)));
    }
    let result = super::walk_import_graph_fingerprinted(
        source_path,
        source,
        super::CODEGEN_FINGERPRINT,
        true,
    );
    let context = match result.entry_compilation_context {
        Some(context) => {
            #[cfg(test)]
            crate::module_artifact::INTERFACE_RESOLUTIONS.with(|count| count.set(count.get() + 1));
            if let (Some(slot), Some(manifest)) = (&slot, &result.manifest) {
                slot.store(&context, manifest);
            }
            context
        }
        None => crate::module_artifact::module_compilation_context_for_source(source_path, source)?,
    };
    Ok((context, result.manifest))
}

/// Render `target` relative to `base` with `/` separators.
fn relative_path_label(base: &Path, target: &Path) -> Option<String> {
    let base_components = base.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();
    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 && (base.is_absolute() || target.is_absolute()) {
        return None;
    }

    let mut parts = Vec::new();
    for component in &base_components[common..] {
        if matches!(component, std::path::Component::Normal(_)) {
            parts.push("..".to_string());
        }
    }
    for component in &target_components[common..] {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => parts.push("..".to_string()),
            std::path::Component::CurDir => {}
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Hash the import graph for a relocatable entry key.
///
/// The key may depend only on what determines the compiled output: the bytes
/// of the entry's import closure, that closure's layout relative to the entry,
/// and the compiler identity. Each input and why it is admitted:
///
/// - Node paths are canonical (`canonical_identity`), so a symlinked, relative,
///   or `..`-laden spelling of one file is one node, and every label below is
///   measured from the canonical entry directory. A symlink inside the tree is
///   followed to its target, and the label is the target's place relative to
///   the entry: following it is what makes two spellings one node.
/// - Labels: package files by their place in the packages tree
///   ([`relocatable_label`]), never by install generation; other files by
///   their path relative to the entry, `/`-separated on every host.
/// - Unresolved imports by their canonical anchor's label plus the import text
///   as written. Built as a string rather than a joined path, so an import that
///   climbs with `..` cannot push a package anchor back out through its
///   generation directory.
/// - A file on another filesystem root (a Windows drive) has no relative
///   path. It keeps its canonical path: moving the tree does not move that
///   file, so reusing a packaged artifact against it would be a guess.
/// - Unreadable files hash their `io::ErrorKind`, a property of the file and
///   not of where the tree sits.
/// - The seed folds the embedded stdlib digest and `CODEGEN_FINGERPRINT`, a
///   hash of the compiler's sources with line endings normalized at build time
///   (`build.rs`), so it names compiler behavior, not the build host.
///
/// Nodes are sorted by label, ties broken by content, so the hash never
/// depends on walk or map order. Nothing here reads the environment, cwd,
/// `HOME`, or mtimes; mtimes live only in the manifest, which is not hashed.
pub(super) fn relocatable_graph_hash(
    source_path: &Path,
    visited: &BTreeMap<PathBuf, super::ImportNode>,
    codegen_fingerprint: &str,
) -> [u8; 32] {
    let entry_identity = crate::module_source::canonical_identity(source_path);
    let entry_dir = entry_identity.parent().unwrap_or(Path::new(""));
    let label_of = |path: &Path| {
        relocatable_label(entry_dir, path)
            .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
    };
    let mut nodes = visited
        .iter()
        .map(|(path, node)| {
            let label = match node {
                super::ImportNode::Unresolved { anchor, import } => {
                    format!("{}\0unresolved\0{import}", label_of(anchor))
                }
                _ => label_of(path),
            };
            let mut bytes = Sha256::new();
            super::hash_import_node(&mut bytes, node);
            (label, <[u8; 32]>::from(bytes.finalize()))
        })
        .collect::<Vec<_>>();
    nodes.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(b"relocatable-entry-graph-v3\0");
    super::seed_entry_context_hasher(&mut hasher, codegen_fingerprint);
    for (label, node_digest) in nodes {
        hasher.update(label.as_bytes());
        hasher.update(b"\0");
        hasher.update(node_digest);
        hasher.update(b"\0");
    }
    hasher.finalize().into()
}

/// Label one dependency for the relocatable entry key.
///
/// A package file is named by its place in the packages tree, not by a path
/// through the generation id that every install mints afresh. Its bytes are
/// hashed beside the label, so equal package content keeps the key across
/// reinstalls and changed content still invalidates it.
fn relocatable_label(entry_dir: &Path, path: &Path) -> Option<String> {
    if let Some(within) = harn_modules::package_snapshot::path_within_package_generation(path) {
        return Some(format!(
            "@packages/{}",
            within.to_string_lossy().replace('\\', "/")
        ));
    }
    relative_path_label(entry_dir, path)
}

#[cfg(test)]
#[path = "package_key_tests.rs"]
mod package_key_tests;

#[cfg(test)]
#[path = "determinism_tests.rs"]
mod determinism_tests;
