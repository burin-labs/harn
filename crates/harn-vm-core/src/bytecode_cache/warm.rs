//! Explicit cache preparation uses the same projections and keys as execution.

use super::*;
use crate::module_artifact::{
    compile_embedded_stdlib_module_artifact_from_source_with_context,
    compile_module_artifact_from_source_with_context,
    compile_trusted_host_dispatch_module_artifact_from_source_with_context,
};
use crate::VmError;

/// Prepare one reachable module without instantiating it. Unlike execution's
/// opportunistic writes, explicit preparation fails if persistence fails.
/// Stdlib graph paths use the graph owner's `<std>/name` spelling.
pub fn precompile_module_to_cache(
    path: &Path,
    source: &str,
    provenance: ModuleProvenance,
) -> Result<(), VmError> {
    if !cache_enabled() || cache_dir().is_none() {
        return Err(VmError::Runtime(
            "module cache is disabled or unavailable".into(),
        ));
    }
    let persist_error = |error: io::Error| {
        VmError::Runtime(format!("prepare cache for {}: {error}", path.display()))
    };
    if let Some(module) = path.to_str().and_then(|path| path.strip_prefix("<std>/")) {
        let source = crate::stdlib_modules::get_stdlib_source(module)
            .ok_or_else(|| VmError::Runtime(format!("unknown stdlib module {module}")))?;
        let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
        let key = CacheKey::from_embedded_stdlib_module_content_hash(
            sha256(source.as_bytes()),
            ModuleProvenance::EmbeddedStdlib,
        );
        if load_embedded_stdlib_module(&key, &synthetic).is_some() {
            return Ok(());
        }
        let artifact = match load_module_for_key(&synthetic, key.clone()).artifact {
            Some(artifact) => artifact,
            None => {
                let context = crate::module_artifact::module_compilation_context_for_source(
                    &synthetic, source,
                )?;
                compile_embedded_stdlib_module_artifact_from_source_with_context(
                    &synthetic, source, &context,
                )?
            }
        };
        return store_module(&key, &artifact).map_err(persist_error);
    }

    let (context, manifest) = graph::derive_interface(path, source)?;
    let manifest = manifest.ok_or_else(|| {
        VmError::Runtime(format!("incomplete interface graph for {}", path.display()))
    })?;
    let slot = InterfaceSlot::open(path, source)
        .ok_or_else(|| VmError::Runtime("module interface cache is unavailable".into()))?;
    slot.write(&context, &manifest).map_err(persist_error)?;
    let source_bytes = crate::module_source::ModuleSource::from_text(source);
    let lookup = load_module(path, &source_bytes, &context, provenance);
    let artifact = match lookup.artifact {
        Some(artifact) => artifact,
        None => match provenance {
            ModuleProvenance::TrustedHostDispatch => {
                compile_trusted_host_dispatch_module_artifact_from_source_with_context(
                    path, source, &context,
                )?
            }
            ModuleProvenance::EmbeddedStdlib => {
                compile_embedded_stdlib_module_artifact_from_source_with_context(
                    path, source, &context,
                )?
            }
            ModuleProvenance::User | ModuleProvenance::PrivilegedWire => {
                compile_module_artifact_from_source_with_context(path, source, &context)?
            }
        },
    };
    store_module(&lookup.key, &artifact).map_err(persist_error)
}
