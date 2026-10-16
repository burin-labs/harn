//! Explicit preparation shares one graph projection across its module keys.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::module_artifact::{
    compile_embedded_stdlib_module_artifact_from_source_with_context,
    compile_module_artifact_from_source_with_context,
    compile_trusted_host_dispatch_module_artifact_from_source_with_context,
};
use crate::VmError;

/// Pure compilation into runtime cache identities. A session remembers exact
/// module keys so overlapping entrypoints compile and persist a module once.
#[derive(Default)]
pub struct CachePrecompiler {
    modules: BTreeSet<String>,
}

impl CachePrecompiler {
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Prepare modules and the root's imported interface without instantiating
    /// source. Return the same entry projection and store identity to the
    /// caller, which owns entry compilation and package-manager authority.
    /// Unlike execution's opportunistic writes, persistence failures are errors.
    pub fn prepare(
        &mut self,
        path: &Path,
        source: &str,
        provenance: ModuleProvenance,
    ) -> Result<(ModuleCompilationContext, LookupOutcome), VmError> {
        if !cache_enabled() || cache_dir().is_none() {
            return Err(VmError::Runtime(
                "module cache is disabled or unavailable".into(),
            ));
        }
        let result = shipped_interface::recall(path, source).unwrap_or_else(|| {
            walk_import_graph_fingerprinted(
                path,
                source,
                CODEGEN_FINGERPRINT,
                GraphOutputs::ManifestAndEntryContext,
            )
        });
        let context = result.entry_compilation_context.ok_or_else(|| {
            VmError::Runtime(format!(
                "incomplete imported interface for {}",
                path.display()
            ))
        })?;
        let manifest = result.manifest.ok_or_else(|| {
            VmError::Runtime(format!("incomplete interface graph for {}", path.display()))
        })?;
        let slot = InterfaceSlot::open(path, source)
            .ok_or_else(|| VmError::Runtime("module interface cache is unavailable".into()))?;
        slot.write(&context, &manifest).map_err(persist_error)?;
        self.prepare_module(path, &ModuleSource::from_text(source), &context, provenance)?;
        let mut projections = manifest
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    (file.content_hash, &file.compilation_context),
                )
            })
            .collect::<BTreeMap<_, _>>();
        projections.insert(
            module_source::canonical_identity(path),
            (sha256(source.as_bytes()), &context),
        );
        for file in &manifest.files {
            let source = module_source::read(&file.path).map_err(persist_error)?;
            if source.sha256() != file.content_hash {
                return Err(VmError::Runtime(format!(
                    "{} changed during preparation",
                    file.path.display()
                )));
            }
            prepare_interface(&file.path, &source, &file.compilation_context, &projections)?;
            self.prepare_module(&file.path, &source, &file.compilation_context, provenance)?;
        }
        // Stdlib has its own runtime key domain and is not a filesystem input
        // in the entry manifest. The module graph supplies its virtual paths.
        for module in harn_modules::build_with_source(path, source).module_paths() {
            if let Some(name) = module.to_str().and_then(|path| path.strip_prefix("<std>/")) {
                self.prepare_stdlib(name)?;
            }
        }
        Ok((
            context,
            graph::entry_store_from_graph(source, result.canonical, Some(manifest)),
        ))
    }

    fn prepare_module(
        &mut self,
        path: &Path,
        source: &ModuleSource,
        context: &ModuleCompilationContext,
        provenance: ModuleProvenance,
    ) -> Result<(), VmError> {
        let key = CacheKey::from_module_source(source, context, provenance);
        let identity = key.identity_filename(MODULE_CACHE_EXTENSION);
        if self.modules.contains(&identity) {
            return Ok(());
        }
        let lookup = load_module_for_key(path, key);
        let artifact = match lookup.artifact {
            Some(artifact) => artifact,
            None => match provenance {
                ModuleProvenance::TrustedHostDispatch => {
                    compile_trusted_host_dispatch_module_artifact_from_source_with_context(
                        path,
                        source.as_str(),
                        context,
                    )?
                }
                ModuleProvenance::EmbeddedStdlib => {
                    compile_embedded_stdlib_module_artifact_from_source_with_context(
                        path,
                        source.as_str(),
                        context,
                    )?
                }
                ModuleProvenance::User | ModuleProvenance::PrivilegedWire => {
                    compile_module_artifact_from_source_with_context(
                        path,
                        source.as_str(),
                        context,
                    )?
                }
            },
        };
        store_module(&lookup.key, &artifact).map_err(persist_error)?;
        self.modules.insert(identity);
        Ok(())
    }

    fn prepare_stdlib(&mut self, module: &str) -> Result<(), VmError> {
        let source = crate::stdlib_modules::get_stdlib_source(module)
            .ok_or_else(|| VmError::Runtime(format!("unknown stdlib module {module}")))?;
        let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
        let key = CacheKey::from_embedded_stdlib_module_content_hash(
            sha256(source.as_bytes()),
            ModuleProvenance::EmbeddedStdlib,
        );
        let identity = key.identity_filename(MODULE_CACHE_EXTENSION);
        if self.modules.contains(&identity) {
            return Ok(());
        }
        if load_embedded_stdlib_module(&key, &synthetic).is_none() {
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
            store_module(&key, &artifact).map_err(persist_error)?;
        }
        self.modules.insert(identity);
        Ok(())
    }
}

/// Each module keeps its own dependency proof. The hash-only walk collects
/// that closure without parsing or deriving the typed projections again.
fn prepare_interface(
    path: &Path,
    source: &ModuleSource,
    context: &ModuleCompilationContext,
    projections: &BTreeMap<PathBuf, ([u8; 32], &ModuleCompilationContext)>,
) -> Result<(), VmError> {
    let slot = InterfaceSlot::open(path, source.as_str())
        .ok_or_else(|| VmError::Runtime("module interface cache is unavailable".into()))?;
    if slot.recall().is_some() {
        return Ok(());
    }
    let proof = walk_import_graph_fingerprinted(
        path,
        source.as_str(),
        CODEGEN_FINGERPRINT,
        GraphOutputs::InterfaceProof,
    );
    let mut manifest = proof.manifest.ok_or_else(|| {
        VmError::Runtime(format!("incomplete interface graph for {}", path.display()))
    })?;
    for file in &mut manifest.files {
        let (_, context) = projections
            .get(&file.path)
            .filter(|(hash, _)| *hash == file.content_hash)
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "{} changed during preparation",
                    file.path.display()
                ))
            })?;
        file.compilation_context = (*context).clone();
    }
    slot.write(context, &manifest).map_err(persist_error)
}

fn persist_error(error: io::Error) -> VmError {
    VmError::Runtime(format!("prepare runtime cache: {error}"))
}
