//! `harn pack <entrypoint>` — build a signed-ready `.harnpack` from a
//! Harn entrypoint.
//!
//! Walks the entrypoint's transitive imports, precompiles every module
//! into a `.harnbc` artifact, snapshots the provider catalog and
//! stdlib pin, generates a minimal SBOM, assembles a v2 `WorkflowBundle`
//! manifest, and emits a deterministic tar.zst archive. Signing lands
//! in E6.3; SBOM body and CLI `--json` polish in E6.4.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process;

use harn_parser::DiagnosticSeverity;
use harn_vm::bytecode_cache;
use harn_vm::module_artifact;
use harn_vm::orchestration::{
    build_harnpack, current_provider_catalog_hash_blake3, load_workflow_bundle_any_version,
    workflow_bundle_hash, CatchupPolicySpec, ConnectorRequirement, EnvironmentRequirements,
    HarnpackEntry, ModuleEntry, RetryPolicySpec, SBOMDoc, SBOMPackage, SBOMRelationship, ToolEntry,
    WorkflowBundle, WorkflowBundlePolicy, WorkflowBundleReplayMetadata, WorkflowBundleTrigger,
    WORKFLOW_BUNDLE_SCHEMA_VERSION,
};
use harn_vm::Compiler;
use serde::Serialize;

use crate::cli::PackArgs;
use crate::command_error;
use crate::json_envelope::{to_string_pretty, JsonEnvelope, JsonOutput};
use crate::parse_source_file;

/// Stable schema version for the `harn pack --json` envelope. Bump when
/// [`PackJsonData`] changes shape in a way that agents need to detect.
pub const PACK_SCHEMA_VERSION: u32 = 1;

/// JSON payload emitted under `JsonEnvelope.data` for `harn pack`.
#[derive(Debug, Clone, Serialize)]
pub struct PackJsonData {
    pub bundle_hash: String,
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub manifest: WorkflowBundle,
}

struct PackJsonOutput(PackJsonData);

impl JsonOutput for PackJsonOutput {
    const SCHEMA_VERSION: u32 = PACK_SCHEMA_VERSION;
    type Data = PackJsonData;
    fn into_envelope(self) -> JsonEnvelope<Self::Data> {
        JsonEnvelope::ok(Self::SCHEMA_VERSION, self.0)
    }
}

pub fn run(args: PackArgs) {
    match build(&args) {
        Ok(outcome) => {
            if args.json {
                let envelope = PackJsonOutput(outcome.json).into_envelope();
                println!("{}", to_string_pretty(&envelope));
            } else {
                println!(
                    "wrote {} ({} bytes, bundle_hash {})",
                    outcome.output_path.display(),
                    outcome.size_bytes,
                    outcome.bundle_hash
                );
            }
        }
        Err(err) => {
            if args.json {
                let envelope: JsonEnvelope<PackJsonData> =
                    JsonEnvelope::err(PACK_SCHEMA_VERSION, err.code, err.message);
                println!("{}", to_string_pretty(&envelope));
                process::exit(1);
            }
            command_error(&err.message);
        }
    }
}

/// Programmatic entrypoint used by tests and other CLI command code
/// that needs the JSON envelope without going through stdout.
pub fn run_to_envelope(args: &PackArgs) -> JsonEnvelope<PackJsonData> {
    match build(args) {
        Ok(outcome) => PackJsonOutput(outcome.json).into_envelope(),
        Err(err) => JsonEnvelope::err(PACK_SCHEMA_VERSION, err.code, err.message),
    }
}

/// Outcome of [`build`]. Used by tests; the dispatcher consumes it
/// directly via [`run`].
pub struct PackOutcome {
    pub bundle_hash: String,
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub json: PackJsonData,
}

#[derive(Debug)]
pub struct PackError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PackError {}

impl PackError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub fn build(args: &PackArgs) -> Result<PackOutcome, PackError> {
    if let Some(upgrade) = &args.upgrade {
        if !upgrade.exists() {
            return Err(PackError::new(
                "upgrade.not_found",
                format!(
                    "--upgrade source bundle does not exist: {}",
                    upgrade.display()
                ),
            ));
        }
    }
    let entrypoint = args
        .entrypoint
        .canonicalize()
        .unwrap_or_else(|_| args.entrypoint.clone());
    if !entrypoint.exists() {
        return Err(PackError::new(
            "entrypoint.not_found",
            format!("entrypoint does not exist: {}", args.entrypoint.display()),
        ));
    }
    if !entrypoint.is_file() || entrypoint.extension().and_then(|ext| ext.to_str()) != Some("harn")
    {
        return Err(PackError::new(
            "entrypoint.invalid",
            format!(
                "entrypoint must be a .harn file: {}",
                args.entrypoint.display()
            ),
        ));
    }
    let project_root = entrypoint
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let entrypoint_rel = relativize(&project_root, &entrypoint).ok_or_else(|| {
        PackError::new(
            "entrypoint.outside_root",
            format!(
                "entrypoint {} could not be relativized against {}",
                entrypoint.display(),
                project_root.display()
            ),
        )
    })?;

    let prior = match &args.upgrade {
        Some(path) => Some(load_workflow_bundle_any_version(path).map_err(|err| {
            PackError::new(
                "upgrade.read_failed",
                format!("failed to read --upgrade source {}: {err}", path.display()),
            )
        })?),
        None => None,
    };

    let graph = harn_modules::build(std::slice::from_ref(&entrypoint));
    let mut module_paths = graph.module_paths();
    // The entrypoint is always present in the graph; ensure deterministic order.
    module_paths.sort();

    let mut transitive_modules = Vec::new();
    let mut contents = Vec::new();
    let mut sbom_packages = Vec::new();
    let mut sbom_relationships = Vec::new();

    let stdlib_version = bytecode_cache::HARN_VERSION.to_string();
    let harn_version = bytecode_cache::HARN_VERSION.to_string();

    sbom_packages.push(SBOMPackage {
        name: "harn-stdlib".to_string(),
        version: Some(stdlib_version.clone()),
        package_hash_blake3: None,
        license: None,
    });

    for module_path in &module_paths {
        let module_str = module_path.to_string_lossy().to_string();
        if module_str.starts_with("<std>/") {
            let stdlib_name = module_str.trim_start_matches("<std>/").to_string();
            sbom_packages.push(SBOMPackage {
                name: format!("std/{stdlib_name}"),
                version: Some(stdlib_version.clone()),
                package_hash_blake3: None,
                license: None,
            });
            sbom_relationships.push(SBOMRelationship {
                from: format!("entrypoint:{}", entrypoint_rel.display()),
                to: format!("std/{stdlib_name}"),
                relationship_type: "depends_on".to_string(),
            });
            continue;
        }

        let source = std::fs::read_to_string(module_path).map_err(|err| {
            PackError::new(
                "module.read_failed",
                format!("failed to read {}: {err}", module_path.display()),
            )
        })?;

        let (parsed_source, program) = parse_source_file(&module_str);
        debug_assert_eq!(parsed_source, source);
        type_check_or_fail(&source, &module_str, &program)?;

        let entry_chunk = Compiler::new().compile(&program).map_err(|err| {
            PackError::new(
                "module.compile_failed",
                format!("compile error in {}: {err}", module_path.display()),
            )
        })?;

        let module_artifact_opt =
            module_artifact::compile_module_artifact(&program, Some(module_str.clone())).ok();

        let cache_key = bytecode_cache::CacheKey::from_source(module_path, &source);
        let chunk_bytes = bytecode_cache::serialize_chunk_artifact(&cache_key, &entry_chunk)
            .map_err(|err| {
                PackError::new(
                    "module.serialize_failed",
                    format!(
                        "failed to serialize chunk for {}: {err}",
                        module_path.display()
                    ),
                )
            })?;

        let module_artifact_bytes = match module_artifact_opt.as_ref() {
            Some(artifact) => Some(
                bytecode_cache::serialize_module_artifact(&cache_key, artifact).map_err(|err| {
                    PackError::new(
                        "module.serialize_failed",
                        format!(
                            "failed to serialize module artifact for {}: {err}",
                            module_path.display()
                        ),
                    )
                })?,
            ),
            None => None,
        };

        let rel = relativize(&project_root, module_path).unwrap_or_else(|| {
            PathBuf::from(
                module_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| module_str.clone()),
            )
        });
        let source_archive_path = PathBuf::from("sources").join(&rel);
        let chunk_archive_path = adjacent_with_extension(&rel, bytecode_cache::CACHE_EXTENSION)
            .ok_or_else(|| {
                PackError::new(
                    "module.invalid_path",
                    format!("module path has no stem: {}", module_path.display()),
                )
            })?;
        let chunk_archive_path = PathBuf::from("bytecode").join(chunk_archive_path);

        let source_hash = blake3_hash(source.as_bytes());
        let harnbc_hash = blake3_hash(&chunk_bytes);

        transitive_modules.push(ModuleEntry {
            path: rel.clone(),
            source_hash_blake3: source_hash.clone(),
            harnbc_hash_blake3: harnbc_hash.clone(),
        });

        contents.push(HarnpackEntry::new(
            source_archive_path,
            source.as_bytes().to_vec(),
        ));
        contents.push(HarnpackEntry::new(chunk_archive_path, chunk_bytes));
        if let Some(artifact_bytes) = module_artifact_bytes {
            let module_rel = adjacent_with_extension(&rel, bytecode_cache::MODULE_CACHE_EXTENSION)
                .ok_or_else(|| {
                    PackError::new(
                        "module.invalid_path",
                        format!("module path has no stem: {}", module_path.display()),
                    )
                })?;
            let module_archive_path = PathBuf::from("bytecode").join(module_rel);
            contents.push(HarnpackEntry::new(module_archive_path, artifact_bytes));
        }

        if module_path != &entrypoint {
            sbom_relationships.push(SBOMRelationship {
                from: format!("entrypoint:{}", entrypoint_rel.display()),
                to: format!("module:{}", rel.display()),
                relationship_type: "depends_on".to_string(),
            });
        }
        sbom_packages.push(SBOMPackage {
            name: format!("module:{}", rel.display()),
            version: Some(harn_version.clone()),
            package_hash_blake3: Some(source_hash),
            license: None,
        });
    }

    if transitive_modules.is_empty() {
        return Err(PackError::new(
            "pack.no_modules",
            format!(
                "no Harn modules resolved from entrypoint {}",
                entrypoint.display()
            ),
        ));
    }

    let provider_catalog_hash = current_provider_catalog_hash_blake3().map_err(|err| {
        PackError::new(
            "provider_catalog.failed",
            format!("failed to snapshot provider catalog: {err}"),
        )
    })?;

    // E6.4 populates `tool_manifest` from the static module walk; today
    // it stays empty so the v2 manifest is consistently deterministic.
    let tool_manifest: Vec<ToolEntry> = Vec::new();
    let bundle = assemble_bundle(
        &entrypoint_rel,
        transitive_modules,
        stdlib_version,
        harn_version,
        provider_catalog_hash,
        tool_manifest,
        SBOMDoc {
            format: "spdx-lite".to_string(),
            version: "2.3".to_string(),
            packages: sbom_packages,
            relationships: sbom_relationships,
        },
        prior.as_ref(),
    );

    let archive_bytes = build_harnpack(&bundle, &contents).map_err(|err| {
        PackError::new(
            "pack.archive_failed",
            format!("failed to assemble .harnpack archive: {err}"),
        )
    })?;
    let bundle_hash = workflow_bundle_hash(&bundle, &contents).map_err(|err| {
        PackError::new(
            "pack.hash_failed",
            format!("failed to compute bundle hash: {err}"),
        )
    })?;

    let output_path = resolve_output_path(&args.out, &entrypoint);
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                PackError::new(
                    "pack.output_dir_failed",
                    format!("failed to create output dir {}: {err}", parent.display()),
                )
            })?;
        }
    }
    std::fs::write(&output_path, &archive_bytes).map_err(|err| {
        PackError::new(
            "pack.write_failed",
            format!("failed to write {}: {err}", output_path.display()),
        )
    })?;
    let size_bytes = archive_bytes.len() as u64;

    Ok(PackOutcome {
        bundle_hash: bundle_hash.clone(),
        output_path: output_path.clone(),
        size_bytes,
        json: PackJsonData {
            bundle_hash,
            output_path,
            size_bytes,
            manifest: bundle,
        },
    })
}

fn assemble_bundle(
    entrypoint_rel: &Path,
    transitive_modules: Vec<ModuleEntry>,
    stdlib_version: String,
    harn_version: String,
    provider_catalog_hash: String,
    tool_manifest: Vec<ToolEntry>,
    sbom: SBOMDoc,
    prior: Option<&WorkflowBundle>,
) -> WorkflowBundle {
    let stem = entrypoint_rel
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "harnpack".to_string());

    let mut bundle = prior.cloned().unwrap_or_else(|| WorkflowBundle {
        id: stem.clone(),
        name: Some(stem.clone()),
        version: "0.0.0".to_string(),
        workflow: degenerate_workflow(&stem),
        triggers: vec![WorkflowBundleTrigger {
            id: "manual".to_string(),
            kind: "manual".to_string(),
            node_id: Some("entry".to_string()),
            ..WorkflowBundleTrigger::default()
        }],
        policy: WorkflowBundlePolicy {
            autonomy_tier: "act_with_approval".to_string(),
            tool_policy: BTreeMap::new(),
            approval_required: Vec::new(),
            retry: RetryPolicySpec {
                max_attempts: 1,
                backoff: "none".to_string(),
            },
            catchup: CatchupPolicySpec {
                mode: "none".to_string(),
                max_events: None,
            },
        },
        connectors: Vec::<ConnectorRequirement>::new(),
        environment: EnvironmentRequirements::default(),
        receipts: WorkflowBundleReplayMetadata::default(),
        ..WorkflowBundle::default()
    });

    bundle.schema_version = WORKFLOW_BUNDLE_SCHEMA_VERSION;
    bundle.entrypoint = entrypoint_rel.to_path_buf();
    bundle.transitive_modules = transitive_modules;
    bundle.stdlib_version = stdlib_version;
    bundle.harn_version = harn_version;
    bundle.provider_catalog_hash = provider_catalog_hash;
    bundle.tool_manifest = tool_manifest;
    bundle.sbom = sbom;
    bundle.signature = None;
    bundle
}

fn degenerate_workflow(stem: &str) -> harn_vm::orchestration::WorkflowGraph {
    use harn_vm::orchestration::{WorkflowGraph, WorkflowNode};
    let mut nodes = BTreeMap::new();
    nodes.insert(
        "entry".to_string(),
        WorkflowNode {
            id: Some("entry".to_string()),
            kind: "action".to_string(),
            task_label: Some(stem.to_string()),
            ..WorkflowNode::default()
        },
    );
    WorkflowGraph {
        type_name: "workflow_graph".to_string(),
        id: format!("{stem}_pack"),
        name: Some(stem.to_string()),
        version: 1,
        entry: "entry".to_string(),
        nodes,
        ..WorkflowGraph::default()
    }
}

fn type_check_or_fail(
    source: &str,
    path: &str,
    program: &[harn_parser::SNode],
) -> Result<(), PackError> {
    let mut had_error = false;
    let mut messages = String::new();
    for diag in harn_parser::TypeChecker::new().check_with_source(program, source) {
        let rendered = harn_parser::diagnostic::render_type_diagnostic(source, path, &diag);
        if matches!(diag.severity, DiagnosticSeverity::Error) {
            had_error = true;
        }
        messages.push_str(&rendered);
    }
    if had_error {
        return Err(PackError::new(
            "module.type_error",
            format!("type errors in {path}:\n{messages}"),
        ));
    }
    if !messages.is_empty() {
        eprint!("{messages}");
    }
    Ok(())
}

fn relativize(root: &Path, target: &Path) -> Option<PathBuf> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let target_canon = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if let Ok(rel) = target_canon.strip_prefix(&root_canon) {
        return Some(rel.to_path_buf());
    }
    // Fallback: keep the filename so deeply-nested entrypoints still
    // pack as relative paths.
    target.file_name().map(PathBuf::from)
}

fn adjacent_with_extension(rel: &Path, extension: &str) -> Option<PathBuf> {
    let stem = rel.file_stem()?.to_string_lossy().into_owned();
    if stem.is_empty() {
        return None;
    }
    let parent_components: Vec<Component<'_>> = rel
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    let mut adjacent = PathBuf::new();
    for component in parent_components {
        adjacent.push(component.as_os_str());
    }
    let mut filename = stem;
    filename.push('.');
    filename.push_str(extension);
    adjacent.push(filename);
    Some(adjacent)
}

fn blake3_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes))
}

fn resolve_output_path(out: &Option<PathBuf>, entrypoint: &Path) -> PathBuf {
    if let Some(path) = out {
        return path.clone();
    }
    let stem = entrypoint
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bundle".to_string());
    let parent = entrypoint.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.harnpack"))
}
