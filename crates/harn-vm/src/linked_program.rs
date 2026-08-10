//! Closed-program bytecode artifact and program-scoped module repository.
//!
//! Ordinary module artifacts are caller-independent and therefore retain their
//! complete export surface. A linked program is different: its graph and every
//! namespace use are closed at build time, so the linker may specialize module
//! exports without weakening generic cache correctness. The runtime installs
//! the decoded module templates for one VM execution tree; it never inserts
//! them into the ordinary source-keyed cache.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::bytecode_cache;
use crate::chunk::{CachedChunk, Chunk};
use crate::module_artifact::ModuleArtifact;
use crate::prepared_module::PreparedModuleArtifact;

pub const LINKED_PROGRAM_SCHEMA_VERSION: u32 = 1;
pub const LINKED_PROGRAM_ARCHIVE_PATH: &str = "artifacts/program.harnlink";
pub const LINKER_ALGORITHM_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"HARNLINK";

/// Compile and specialize one closed source graph into the runtime's single
/// linked-program artifact. Package policy, signing, and source inclusion stay
/// with the archive caller; graph discovery and bytecode reachability live here.
pub fn link_program(
    entrypoint: &Path,
    project_root: &Path,
) -> Result<LinkedProgramArtifact, LinkedProgramError> {
    let entrypoint = harn_modules::canonical_path(entrypoint);
    let project_root = harn_modules::canonical_path(project_root);
    let build = harn_modules::build_closed_program(std::slice::from_ref(&entrypoint));
    let reachability = harn_modules::closed_program_reachability(&build, &entrypoint);
    let entry_source = build.parsed_sources.get(&entrypoint).ok_or_else(|| {
        LinkedProgramError::invalid(format!(
            "entrypoint {} was not parsed by the closed graph",
            entrypoint.display()
        ))
    })?;
    let imported_enums = build
        .graph
        .imported_names_by_kind_for_file(&entrypoint, harn_modules::DefKind::Enum)
        .unwrap_or_default();
    let imported_callables = build
        .graph
        .imported_callable_names_for_file(&entrypoint)
        .unwrap_or_default();
    let entry_chunk = crate::Compiler::new()
        .with_imported_enum_candidates(imported_enums)
        .with_imported_source_callable_names(imported_callables)
        .compile(&entry_source.program)
        .map_err(|error| {
            LinkedProgramError::invalid(format!(
                "entrypoint compile failed for {}: {error}",
                entrypoint.display()
            ))
        })?
        .freeze_for_cache();

    let entrypoint_rel = entrypoint.strip_prefix(&project_root).map_err(|_| {
        LinkedProgramError::invalid(format!(
            "entrypoint {} is outside package root {}",
            entrypoint.display(),
            project_root.display()
        ))
    })?;
    let entry_bytes = postcard::to_allocvec(&entry_chunk)
        .map_err(|error| LinkedProgramError::invalid(format!("entry size failed: {error}")))?;
    let mut report = LinkReport {
        linker_algorithm_version: LINKER_ALGORITHM_VERSION,
        harn_version: bytecode_cache::HARN_VERSION.to_string(),
        codegen_fingerprint: bytecode_cache::CODEGEN_FINGERPRINT.to_string(),
        input_bytecode_bytes: entry_bytes.len() as u64,
        output_bytecode_bytes: entry_bytes.len() as u64,
        user_input_bytes: entry_bytes.len() as u64,
        user_output_bytes: entry_bytes.len() as u64,
        modules: vec![LinkModuleReport {
            path: entrypoint_rel.to_path_buf(),
            demand: LinkModuleDemand::WholeNamespace,
            input_bytes: entry_bytes.len() as u64,
            output_bytes: entry_bytes.len() as u64,
            initializer_bytes: 0,
            type_schema_bytes: 0,
            widening_reason: None,
            retained_symbols: vec![LinkSymbolReason {
                symbol: "<entry>".to_string(),
                reason: "typed entry chunk".to_string(),
            }],
            removed_symbols: Vec::new(),
        }],
        ..LinkReport::default()
    };

    let mut modules = BTreeMap::new();
    let mut digest_inputs = Vec::new();
    for path in build.graph.module_paths() {
        let path = harn_modules::canonical_path(&path);
        // A closed-program build retains every source that parsed as Harn, so a
        // graph node with none is an imported non-Harn asset. Assets are archive
        // payload owned by the packaging caller, not executable modules: they
        // carry no bytecode to link and their integrity is bound by the SBOM and
        // archive hashes rather than the program's graph digest.
        let Some(parsed) = build.parsed_sources.get(&path) else {
            continue;
        };
        let archive_path = archive_module_path(&project_root, &path)?;
        digest_inputs.push((archive_path.clone(), parsed.source.as_bytes().to_vec()));
        if path == entrypoint {
            continue;
        }
        let imported_enums = build
            .graph
            .imported_names_by_kind_for_file(&path, harn_modules::DefKind::Enum)
            .unwrap_or_default();
        let imported_callables = build
            .graph
            .imported_callable_names_for_file(&path)
            .unwrap_or_default();
        let compile_path = runtime_compile_path(&path);
        let full =
            crate::module_artifact::compile_module_artifact_from_source_with_imported_symbols(
                &compile_path,
                &parsed.source,
                imported_enums,
                imported_callables,
            )
            .map_err(|error| {
                LinkedProgramError::invalid(format!(
                    "module compile failed for {}: {error}",
                    path.display()
                ))
            })?;
        let full_symbols = artifact_symbols(&full);
        let input_bytes = postcard::to_allocvec(&full)
            .map_err(|error| LinkedProgramError::invalid(format!("module size failed: {error}")))?
            .len() as u64;
        let requested = reachability.demand_for(&path);
        let widening_reason = full.imports.iter().any(|import| import.is_pub).then(|| {
            "public re-export shares the module's local import projection; retained whole namespace"
                .to_string()
        });
        let effective = if widening_reason.is_some() {
            harn_modules::ExportDemand::WholeNamespace
        } else {
            requested
        };
        let selected = crate::module_artifact::specialize_module_artifact(
            &parsed.program,
            Some(compile_path.display().to_string()),
            full,
            &effective,
        )
        .map_err(|error| {
            LinkedProgramError::invalid(format!(
                "module specialization failed for {}: {error}",
                path.display()
            ))
        })?;
        let selected_symbols = artifact_symbols(&selected);
        let output_bytes = postcard::to_allocvec(&selected)
            .map_err(|error| LinkedProgramError::invalid(format!("module size failed: {error}")))?
            .len() as u64;
        let mut retained_symbols = selected_symbols
            .iter()
            .map(|symbol| LinkSymbolReason {
                symbol: symbol.clone(),
                reason: if effective.contains(symbol) {
                    "observable export".to_string()
                } else {
                    "initializer or private callable dependency".to_string()
                },
            })
            .collect::<Vec<_>>();
        let initializer_bytes = selected.init_chunk.as_ref().map_or(0, |chunk| {
            postcard::to_allocvec(chunk).map_or(0, |bytes| bytes.len() as u64)
        });
        let type_schema_bytes = selected
            .type_schema_init_chunks
            .iter()
            .map(|chunk| postcard::to_allocvec(chunk).map_or(0, |bytes| bytes.len() as u64))
            .sum();
        if initializer_bytes > 0 {
            retained_symbols.push(LinkSymbolReason {
                symbol: "<module_initializer>".to_string(),
                reason: "module effects are preserved conservatively".to_string(),
            });
        }
        let removed_symbols = full_symbols
            .difference(&selected_symbols)
            .cloned()
            .collect::<Vec<_>>();
        report.input_bytecode_bytes += input_bytes;
        report.output_bytecode_bytes += output_bytes;
        if archive_path
            .to_str()
            .is_some_and(|path| path.starts_with("<std>/"))
        {
            report.stdlib_input_bytes += input_bytes;
            report.stdlib_output_bytes += output_bytes;
        } else {
            report.user_input_bytes += input_bytes;
            report.user_output_bytes += output_bytes;
        }
        report.retained_symbols += retained_symbols.len() as u64;
        report.removed_symbols += removed_symbols.len() as u64;
        report.modules.push(LinkModuleReport {
            path: archive_path.clone(),
            demand: match effective {
                harn_modules::ExportDemand::InitializationOnly => {
                    LinkModuleDemand::InitializationOnly
                }
                harn_modules::ExportDemand::Members(_) => LinkModuleDemand::Members,
                harn_modules::ExportDemand::WholeNamespace => LinkModuleDemand::WholeNamespace,
            },
            input_bytes,
            output_bytes,
            initializer_bytes,
            type_schema_bytes,
            widening_reason,
            retained_symbols,
            removed_symbols,
        });
        modules.insert(archive_path, selected);
    }
    digest_inputs.sort_by(|left, right| left.0.cmp(&right.0));
    let graph_digest_blake3 = graph_digest_from_sources(&digest_inputs);
    report.graph_digest_blake3.clone_from(&graph_digest_blake3);
    report
        .modules
        .sort_by(|left, right| left.path.cmp(&right.path));

    Ok(LinkedProgramArtifact {
        schema_version: LINKED_PROGRAM_SCHEMA_VERSION,
        identity: LinkedProgramIdentity::current(graph_digest_blake3),
        entrypoint: entrypoint_rel.to_path_buf(),
        entry_chunk,
        modules,
        report,
    })
}

fn archive_module_path(project_root: &Path, path: &Path) -> Result<PathBuf, LinkedProgramError> {
    if path.to_str().is_some_and(|path| path.starts_with("<std>/")) {
        return Ok(path.to_path_buf());
    }
    path.strip_prefix(project_root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            LinkedProgramError::invalid(format!(
                "module {} is outside package root {}",
                path.display(),
                project_root.display()
            ))
        })
}

fn runtime_compile_path(path: &Path) -> PathBuf {
    path.to_str()
        .and_then(|path| path.strip_prefix("<std>/"))
        .map_or_else(
            || path.to_path_buf(),
            |module| PathBuf::from(format!("<stdlib>/{module}.harn")),
        )
}

fn artifact_symbols(artifact: &ModuleArtifact) -> std::collections::BTreeSet<String> {
    artifact
        .functions
        .keys()
        .chain(artifact.public_exports.keys())
        .cloned()
        .collect()
}

/// Digest the exact normalized source graph compiled into a linked program.
/// The pack verifier reconstructs this independently from verified archive
/// sources plus the current embedded stdlib modules before installation.
pub fn graph_digest_from_sources(sources: &[(PathBuf, Vec<u8>)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"harn-linked-program-graph-v1\0");
    for (path, source) in sources {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        hasher.update(&(source.len() as u64).to_le_bytes());
        hasher.update(source);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Reconstruct and verify a linked graph from independently verified user
/// sources plus the runtime's embedded stdlib. Archive verification and direct
/// execution share this boundary so neither can accept a self-consistent but
/// manifest-detached artifact.
pub fn verify_graph_binding(
    report: &LinkReport,
    expected_digest: &str,
    mut user_source: impl FnMut(&Path) -> Option<Vec<u8>>,
) -> Result<(), LinkedProgramError> {
    let mut sources = Vec::new();
    for module in &report.modules {
        let bytes = if let Some(name) = module
            .path
            .to_str()
            .and_then(|path| path.strip_prefix("<std>/"))
        {
            crate::stdlib_modules::get_stdlib_source(name)
                .ok_or_else(|| {
                    LinkedProgramError::invalid(format!(
                        "runtime has no embedded stdlib module std/{name}"
                    ))
                })?
                .as_bytes()
                .to_vec()
        } else {
            user_source(&module.path).ok_or_else(|| {
                LinkedProgramError::invalid(format!(
                    "verified source graph has no {}",
                    module.path.display()
                ))
            })?
        };
        sources.push((module.path.clone(), bytes));
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    let actual = graph_digest_from_sources(&sources);
    if actual != expected_digest {
        return Err(LinkedProgramError::invalid(format!(
            "linked graph digest mismatch: manifest {expected_digest}, verified sources {actual}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkedProgramIdentity {
    pub graph_digest_blake3: String,
    pub harn_version: String,
    pub codegen_fingerprint: String,
    pub bytecode_schema_version: u32,
    pub linker_algorithm_version: u32,
    pub optimizations_enabled: bool,
}

impl LinkedProgramIdentity {
    pub fn current(graph_digest_blake3: String) -> Self {
        Self {
            graph_digest_blake3,
            harn_version: bytecode_cache::HARN_VERSION.to_string(),
            codegen_fingerprint: bytecode_cache::CODEGEN_FINGERPRINT.to_string(),
            bytecode_schema_version: bytecode_cache::SCHEMA_VERSION,
            linker_algorithm_version: LINKER_ALGORITHM_VERSION,
            optimizations_enabled: crate::CompilerOptions::from_env().optimizations_enabled(),
        }
    }

    fn validate_current(&self) -> Result<(), LinkedProgramError> {
        let expected = Self::current(self.graph_digest_blake3.clone());
        if self.harn_version != expected.harn_version {
            return Err(LinkedProgramError::incompatible(format!(
                "linked program was built by harn {}; this runtime is {}",
                self.harn_version, expected.harn_version
            )));
        }
        if self.codegen_fingerprint != expected.codegen_fingerprint
            || self.bytecode_schema_version != expected.bytecode_schema_version
            || self.linker_algorithm_version != expected.linker_algorithm_version
            || self.optimizations_enabled != expected.optimizations_enabled
        {
            return Err(LinkedProgramError::incompatible(
                "linked program compiler, bytecode, or linker identity does not match this runtime",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkReport {
    pub graph_digest_blake3: String,
    pub linker_algorithm_version: u32,
    pub harn_version: String,
    pub codegen_fingerprint: String,
    pub input_bytecode_bytes: u64,
    pub output_bytecode_bytes: u64,
    pub user_input_bytes: u64,
    pub user_output_bytes: u64,
    pub stdlib_input_bytes: u64,
    pub stdlib_output_bytes: u64,
    pub retained_symbols: u64,
    pub removed_symbols: u64,
    pub modules: Vec<LinkModuleReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkModuleReport {
    pub path: PathBuf,
    pub demand: LinkModuleDemand,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub initializer_bytes: u64,
    pub type_schema_bytes: u64,
    pub widening_reason: Option<String>,
    pub retained_symbols: Vec<LinkSymbolReason>,
    pub removed_symbols: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkModuleDemand {
    InitializationOnly,
    Members,
    WholeNamespace,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkSymbolReason {
    pub symbol: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkedProgramArtifact {
    pub schema_version: u32,
    pub identity: LinkedProgramIdentity,
    /// Archive-relative user source path.
    pub entrypoint: PathBuf,
    pub entry_chunk: CachedChunk,
    /// Archive-relative user source paths or `<std>/<module>` virtual paths.
    pub modules: BTreeMap<PathBuf, ModuleArtifact>,
    pub report: LinkReport,
}

impl LinkedProgramArtifact {
    pub fn encode(&self) -> Result<Vec<u8>, LinkedProgramError> {
        let payload = postcard::to_allocvec(self)
            .map_err(|error| LinkedProgramError::invalid(format!("encode failed: {error}")))?;
        let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&LINKED_PROGRAM_SCHEMA_VERSION.to_le_bytes());
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, LinkedProgramError> {
        let Some((magic, rest)) = bytes.split_at_checked(MAGIC.len()) else {
            return Err(LinkedProgramError::invalid(
                "linked program header is truncated",
            ));
        };
        if magic != MAGIC {
            return Err(LinkedProgramError::invalid(
                "linked program magic is invalid",
            ));
        }
        let Some((schema, payload)) = rest.split_at_checked(4) else {
            return Err(LinkedProgramError::invalid(
                "linked program schema header is truncated",
            ));
        };
        let actual = u32::from_le_bytes(schema.try_into().expect("four-byte schema"));
        if actual != LINKED_PROGRAM_SCHEMA_VERSION {
            return Err(LinkedProgramError::incompatible(format!(
                "linked program schema {actual} is unsupported; expected {LINKED_PROGRAM_SCHEMA_VERSION}"
            )));
        }
        let (artifact, trailing): (Self, &[u8]) = postcard::take_from_bytes(payload)
            .map_err(|error| LinkedProgramError::invalid(format!("decode failed: {error}")))?;
        if !trailing.is_empty() {
            return Err(LinkedProgramError::invalid(
                "linked program contains trailing bytes",
            ));
        }
        if artifact.schema_version != LINKED_PROGRAM_SCHEMA_VERSION {
            return Err(LinkedProgramError::invalid(format!(
                "linked program payload schema {} disagrees with its header",
                artifact.schema_version
            )));
        }
        artifact.identity.validate_current()?;
        Ok(artifact)
    }

    pub fn into_runtime(self, source_root: &Path) -> LinkedProgramRuntime {
        let modules = self
            .modules
            .into_iter()
            .map(|(path, mut artifact)| {
                let runtime_path = runtime_module_path(source_root, &path);
                artifact.bind_source_file(&runtime_path);
                (
                    runtime_path,
                    Arc::new(PreparedModuleArtifact::from_cached(artifact)),
                )
            })
            .collect();
        LinkedProgramRuntime {
            digest: self.identity.graph_digest_blake3,
            entry_chunk: Chunk::from_cached(self.entry_chunk),
            repository: Arc::new(LinkedProgramRepository { modules }),
            report: self.report,
        }
    }
}

fn runtime_module_path(source_root: &Path, path: &Path) -> PathBuf {
    if let Some(path) = path.to_str().and_then(|path| path.strip_prefix("<std>/")) {
        return PathBuf::from(format!("<stdlib>/{path}.harn"));
    }
    let path = source_root.join(path);
    path.canonicalize().unwrap_or(path)
}

pub struct LinkedProgramRuntime {
    pub digest: String,
    pub entry_chunk: Chunk,
    pub report: LinkReport,
    pub(crate) repository: Arc<LinkedProgramRepository>,
}

pub(crate) struct LinkedProgramRepository {
    modules: BTreeMap<PathBuf, Arc<PreparedModuleArtifact>>,
}

impl LinkedProgramRepository {
    pub(crate) fn get(&self, path: &Path) -> Option<Arc<PreparedModuleArtifact>> {
        self.modules.get(path).cloned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedProgramError {
    pub code: &'static str,
    pub message: String,
}

impl LinkedProgramError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "linked_program.invalid",
            message: message.into(),
        }
    }

    fn incompatible(message: impl Into<String>) -> Self {
        Self {
            code: "linked_program.incompatible",
            message: message.into(),
        }
    }
}

impl fmt::Display for LinkedProgramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LinkedProgramError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn identity_rejects_codegen_drift() {
        let mut identity = LinkedProgramIdentity::current("blake3:test".to_string());
        identity.codegen_fingerprint.push_str("-different");
        let error = identity.validate_current().unwrap_err();
        assert_eq!(error.code, "linked_program.incompatible");
    }

    #[test]
    fn closed_link_retains_private_callable_closure_and_initializer_roots() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library.harn");
        let entry = dir.path().join("entry.harn");
        fs::write(
            &library,
            r#"
            fn helper_a() { helper_b() }
            fn helper_b() { 7 }
            fn init_helper() { "initialized" }
            const init_hook = init_helper
            pub fn kept() { helper_a() }
            pub fn dead() { "dead" }
            pub type KeptShape = { value: int }
            pub type DeadShape = { value: string }
            "#,
        )
        .unwrap();
        fs::write(
            &entry,
            r#"
            import * as lib from "./library.harn"
            fn main() { println(lib.kept()) }
            "#,
        )
        .unwrap();

        let linked = link_program(&entry, dir.path()).expect("link succeeds");
        let library = &linked.modules[Path::new("library.harn")];
        assert!(library.functions.contains_key("kept"));
        assert!(library.functions.contains_key("helper_a"));
        assert!(library.functions.contains_key("helper_b"));
        assert!(library.functions.contains_key("init_helper"));
        assert!(!library.functions.contains_key("dead"));
        assert_eq!(
            library.public_exports.keys().cloned().collect::<Vec<_>>(),
            ["kept"]
        );
        let report = linked
            .report
            .modules
            .iter()
            .find(|module| module.path == Path::new("library.harn"))
            .unwrap();
        assert!(report.removed_symbols.iter().any(|name| name == "dead"));
        assert!(report.initializer_bytes > 0);
        assert!(report.output_bytes < report.input_bytes);
    }

    #[test]
    fn selective_type_import_retains_only_its_schema_initializer() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("types.harn");
        let entry = dir.path().join("entry.harn");
        fs::write(
            &library,
            r"
            pub type KeptShape = { value: int }
            pub type DeadShape = { value: string }
            ",
        )
        .unwrap();
        fs::write(
            &entry,
            r#"
            import { KeptShape } from "./types.harn"
            fn accept(value: KeptShape) { value.value }
            fn main() { accept({ value: 7 }) }
            "#,
        )
        .unwrap();

        let linked = link_program(&entry, dir.path()).expect("link succeeds");
        let types = &linked.modules[Path::new("types.harn")];
        assert_eq!(
            types.public_type_names.iter().cloned().collect::<Vec<_>>(),
            ["KeptShape"]
        );
        assert_eq!(types.type_schema_init_chunks.len(), 1);
        let report = linked
            .report
            .modules
            .iter()
            .find(|module| module.path == Path::new("types.harn"))
            .unwrap();
        assert!(report.type_schema_bytes > 0);
        assert!(report
            .removed_symbols
            .iter()
            .any(|name| name == "DeadShape"));
    }

    #[test]
    fn public_reexport_records_conservative_widening() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner.harn");
        let facade = dir.path().join("facade.harn");
        let entry = dir.path().join("entry.harn");
        fs::write(&inner, "pub fn kept() { 7 }\npub fn dead() { 8 }\n").unwrap();
        fs::write(
            &facade,
            r#"
            pub import { kept } from "./inner.harn"
            pub fn local_dead() { 9 }
            "#,
        )
        .unwrap();
        fs::write(
            &entry,
            r#"
            import { kept } from "./facade.harn"
            fn main() { println(kept()) }
            "#,
        )
        .unwrap();

        let linked = link_program(&entry, dir.path()).expect("link succeeds");
        let facade_report = linked
            .report
            .modules
            .iter()
            .find(|module| module.path == Path::new("facade.harn"))
            .unwrap();
        assert_eq!(facade_report.demand, LinkModuleDemand::WholeNamespace);
        assert!(facade_report
            .widening_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("public re-export")));
        assert!(linked.modules[Path::new("facade.harn")]
            .functions
            .contains_key("local_dead"));
    }
}
