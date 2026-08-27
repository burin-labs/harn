//! Shared projections from the bytecode cache's canonical graph walk.

use std::path::Path;

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
pub(crate) fn derive_interface(
    source_path: &Path,
    source: &str,
) -> Result<(ModuleCompilationContext, Option<ContextManifest>), VmError> {
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
            context
        }
        None => crate::module_artifact::module_compilation_context_for_source(source_path, source)?,
    };
    Ok((context, result.manifest))
}

/// Render `target` relative to `base` with `/` separators.
pub(super) fn relative_path_label(base: &Path, target: &Path) -> Option<String> {
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
