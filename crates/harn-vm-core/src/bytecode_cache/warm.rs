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
    stdlib_prepared: bool,
}

impl CachePrecompiler {
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Prepare every module in the installed stdlib catalog, including modules
    /// loaded dynamically by native capabilities rather than source imports.
    pub fn prepare_stdlib_catalog(&mut self) -> Result<(), VmError> {
        require_cache()?;
        if !self.stdlib_prepared {
            for entry in harn_modules::stdlib_catalog().sources {
                self.prepare_stdlib_module(entry.module)?;
            }
            self.stdlib_prepared = true;
        }
        Ok(())
    }

    /// Prepare a dynamically loaded stdlib root and its import closure. Lean
    /// backends use this instead of preparing optional, unlinked integrations.
    pub fn prepare_stdlib(&mut self, module: &str) -> Result<(), VmError> {
        require_cache()?;
        let source = crate::stdlib_modules::get_stdlib_source(module)
            .ok_or_else(|| VmError::Runtime(format!("unknown stdlib module {module}")))?;
        self.prepare_stdlib_module(module)?;
        self.prepare_graph_stdlib(&PathBuf::from(format!("<stdlib>/{module}.harn")), source)
    }

    fn prepare_graph_stdlib(&mut self, path: &Path, source: &str) -> Result<(), VmError> {
        if !self.stdlib_prepared {
            for path in harn_modules::build_with_source(path, source).module_paths() {
                if let Some(module) = harn_modules::stdlib_module_name(&path) {
                    self.prepare_stdlib_module(module)?;
                }
            }
        }
        Ok(())
    }

    /// Prepare modules and the root's imported interface without instantiating
    /// source. Return the same entry projection and store identity to the
    /// caller, which owns entry compilation and package-manager authority.
    /// Static stdlib imports are included. Call `prepare_stdlib` for additional
    /// roots loaded dynamically by native capabilities, or prepare the whole
    /// catalog when the backend links every stdlib integration.
    /// File roots support user or trusted host-dispatch provenance; embedded
    /// stdlib preparation belongs to `prepare_stdlib_catalog`.
    /// Unlike execution's opportunistic writes, persistence failures are errors.
    pub fn prepare(
        &mut self,
        path: &Path,
        source: &str,
        provenance: ModuleProvenance,
    ) -> Result<(ModuleCompilationContext, LookupOutcome), VmError> {
        require_cache()?;
        if !matches!(
            provenance,
            ModuleProvenance::User | ModuleProvenance::TrustedHostDispatch
        ) {
            return Err(VmError::Runtime(
                "file preparation requires user or trusted host-dispatch provenance".into(),
            ));
        }
        let slot = InterfaceSlot::open(path, source)
            .ok_or_else(|| VmError::Runtime("module interface cache is unavailable".into()))?;
        let result = shipped_interface::recall(path, source)
            .or_else(|| {
                let (context, manifest) = slot.recall()?;
                let mut result = walk_import_graph_fingerprinted(
                    path,
                    source,
                    CODEGEN_FINGERPRINT,
                    GraphOutputs::Hashes,
                );
                // A concurrent edit between recall and hashing must not pair an
                // old typed projection with a new graph identity.
                let manifest = match manifest.check(&manifest.entry) {
                    ManifestCheck::Valid => manifest,
                    ManifestCheck::ValidAfterRecheck { refreshed } => refreshed,
                    ManifestCheck::Stale => return None,
                };
                result.manifest = Some(manifest);
                result.entry_compilation_context = Some(context);
                Some(result)
            })
            .unwrap_or_else(|| {
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
        self.prepare_graph_stdlib(path, source)?;
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
                ModuleProvenance::User => compile_module_artifact_from_source_with_context(
                    path,
                    source.as_str(),
                    context,
                )?,
                ModuleProvenance::EmbeddedStdlib | ModuleProvenance::PrivilegedWire => {
                    return Err(VmError::Runtime(
                        "unsupported file preparation provenance".into(),
                    ));
                }
            },
        };
        store_module(&lookup.key, &artifact).map_err(persist_error)?;
        self.modules.insert(identity);
        Ok(())
    }

    fn prepare_stdlib_module(&mut self, module: &str) -> Result<(), VmError> {
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

fn require_cache() -> Result<(), VmError> {
    if !cache_enabled() || cache_dir().is_none() {
        return Err(VmError::Runtime(
            "module cache is disabled or unavailable".into(),
        ));
    }
    Ok(())
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
