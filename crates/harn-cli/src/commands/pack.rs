//! `harn pack <entrypoint>` — build a signed-ready `.harnpack` from a
//! Harn entrypoint.
//!
//! Walks the entrypoint's transitive imports, precompiles every module
//! into a `.harnbc` artifact, snapshots the provider catalog and
//! stdlib pin, generates a minimal SBOM, assembles a v2 `WorkflowBundle`
//! manifest, and emits a deterministic tar.zst archive.
//!
//! `harn pack verify <bundle.harnpack>` (#1779) reads a bundle back,
//! recomputes its canonical hash, verifies the embedded Ed25519
//! signature (if any), and cross-checks every per-module BLAKE3.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process;

use ed25519_dalek::Signer;
use harn_parser::DiagnosticSeverity;
use harn_vm::bytecode_cache;
use harn_vm::module_artifact;
use harn_vm::orchestration::{
    build_harnpack, load_workflow_bundle_any_version, workflow_bundle_hash, CatchupPolicySpec,
    ConnectorRequirement, Ed25519Signature, EnvironmentRequirements, HarnpackEntry, ModuleEntry,
    RetryPolicySpec, SBOMDoc, SBOMPackage, SBOMRelationship, ToolEntry, WorkflowBundle,
    WorkflowBundlePolicy, WorkflowBundleReplayMetadata, WorkflowBundleTrigger,
    WORKFLOW_BUNDLE_SCHEMA_VERSION,
};
use harn_vm::{AutonomyTier, TrustRecord};
use serde::{Deserialize, Serialize};

use crate::cli::{PackArgs, PackCommand};
use crate::command_error;
use crate::json_envelope::{to_string_pretty, JsonEnvelope, JsonOutput, JsonWarning};
use crate::parse_source_file;
use crate::skill_provenance;

/// Stable schema version for the `harn pack --json` envelope. Bump when
/// [`PackJsonData`] changes shape in a way that agents need to detect.
pub const PACK_SCHEMA_VERSION: u32 = 2;
pub const PACK_SBOM_ARCHIVE_PATH: &str = "sbom.spdx.json";
const DEFAULT_PACK_FILE_MODE: u32 = 0o644;

/// JSON payload emitted under `JsonEnvelope.data` for `harn pack`.
#[derive(Debug, Clone, Serialize)]
pub struct PackJsonData {
    pub bundle_hash: String,
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub signature: PackSignatureSummary,
    pub sbom_summary: PackSbomSummary,
    pub debug_symbol_metadata: PackDebugSymbolMetadata,
    pub manifest: WorkflowBundle,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackSignatureSummary {
    pub algorithm: String,
    pub key_id: Option<String>,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackSbomSummary {
    pub components: usize,
    pub stdlib_modules: usize,
    pub providers: usize,
    pub tools: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackDebugSymbolMetadata {
    pub harnbc_count: usize,
    pub total_bytes: u64,
}

struct PackJsonOutput {
    data: PackJsonData,
    warnings: Vec<JsonWarning>,
}

fn logical_bundle_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

impl JsonOutput for PackJsonOutput {
    const SCHEMA_VERSION: u32 = PACK_SCHEMA_VERSION;
    type Data = PackJsonData;
    fn into_envelope(self) -> JsonEnvelope<Self::Data> {
        let mut envelope = JsonEnvelope::ok(Self::SCHEMA_VERSION, self.data);
        envelope.warnings = self.warnings;
        envelope
    }
}

pub fn run(args: PackArgs) {
    if let Some(command) = args.command {
        match command {
            PackCommand::Unpack(unpack_args) => return run_unpack(unpack_args),
            PackCommand::Repack(repack_args) => return run_repack(repack_args),
            PackCommand::Verify(verify_args) => return run_verify(verify_args),
        }
    }
    let Some(entrypoint) = args.entrypoint.clone() else {
        command_error("harn pack requires an entrypoint or a subcommand (see `harn pack --help`)");
    };
    let build_args = BuildArgs {
        entrypoint,
        out: args.out,
        upgrade: args.upgrade,
        sign: args.sign,
        key: args.key,
        unsigned: args.unsigned,
        exclude_secrets: args.exclude_secrets,
        json: args.json,
    };
    match build(&build_args) {
        Ok(outcome) => {
            if build_args.json {
                let envelope = PackJsonOutput {
                    data: outcome.json,
                    warnings: outcome.warnings,
                }
                .into_envelope();
                println!("{}", to_string_pretty(&envelope));
            } else {
                for warning in &outcome.warnings {
                    eprintln!("warning[{}]: {}", warning.code, warning.message);
                }
                println!(
                    "wrote {} ({} bytes, bundle_hash {})",
                    outcome.output_path.display(),
                    outcome.size_bytes,
                    outcome.bundle_hash
                );
            }
        }
        Err(err) => {
            if build_args.json {
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
    let Some(entrypoint) = args.entrypoint.clone() else {
        return JsonEnvelope::err(
            PACK_SCHEMA_VERSION,
            "pack.missing_entrypoint",
            "harn pack requires an entrypoint or a subcommand".to_string(),
        );
    };
    let build_args = BuildArgs {
        entrypoint,
        out: args.out.clone(),
        upgrade: args.upgrade.clone(),
        sign: args.sign,
        key: args.key.clone(),
        unsigned: args.unsigned,
        exclude_secrets: args.exclude_secrets,
        json: args.json,
    };
    match build(&build_args) {
        Ok(outcome) => PackJsonOutput {
            data: outcome.json,
            warnings: outcome.warnings,
        }
        .into_envelope(),
        Err(err) => JsonEnvelope::err(PACK_SCHEMA_VERSION, err.code, err.message),
    }
}

/// Plain-data input to [`build`]: a flattened copy of [`PackArgs`]
/// without the subcommand surface. Tests can construct this directly
/// instead of going through the CLI parser.
#[derive(Debug, Clone)]
pub struct BuildArgs {
    pub entrypoint: PathBuf,
    pub out: Option<PathBuf>,
    pub upgrade: Option<PathBuf>,
    pub sign: bool,
    pub key: Option<PathBuf>,
    pub unsigned: bool,
    pub exclude_secrets: bool,
    pub json: bool,
}

pub fn json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "harn pack --json",
        "type": "object",
        "required": ["schemaVersion", "ok", "data", "warnings"],
        "properties": {
            "schemaVersion": { "const": PACK_SCHEMA_VERSION },
            "ok": { "const": true },
            "warnings": { "type": "array" },
            "data": {
                "type": "object",
                "required": [
                    "bundle_hash",
                    "output_path",
                    "size_bytes",
                    "signature",
                    "sbom_summary",
                    "debug_symbol_metadata",
                    "manifest"
                ],
                "properties": {
                    "bundle_hash": { "type": "string", "pattern": "^blake3:" },
                    "output_path": { "type": "string", "minLength": 1 },
                    "size_bytes": { "type": "integer", "minimum": 1 },
                    "signature": {
                        "type": "object",
                        "required": ["algorithm", "key_id", "present"],
                        "properties": {
                            "algorithm": { "const": "ed25519" },
                            "key_id": { "type": ["string", "null"] },
                            "present": { "type": "boolean" }
                        }
                    },
                    "sbom_summary": {
                        "type": "object",
                        "required": ["components", "stdlib_modules", "providers", "tools"],
                        "properties": {
                            "components": { "type": "integer", "minimum": 1 },
                            "stdlib_modules": { "type": "integer", "minimum": 0 },
                            "providers": { "type": "integer", "minimum": 0 },
                            "tools": { "type": "integer", "minimum": 0 }
                        }
                    },
                    "debug_symbol_metadata": {
                        "type": "object",
                        "required": ["harnbc_count", "total_bytes"],
                        "properties": {
                            "harnbc_count": { "type": "integer", "minimum": 1 },
                            "total_bytes": { "type": "integer", "minimum": 1 }
                        }
                    },
                    "manifest": { "type": "object" }
                }
            }
        }
    })
}

/// Outcome of [`build`]. Used by tests; the dispatcher consumes it
/// directly via [`run`].
#[derive(Debug)]
pub struct PackOutcome {
    pub bundle_hash: String,
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub json: PackJsonData,
    pub warnings: Vec<JsonWarning>,
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

mod archive;

pub use archive::{
    repack, run_repack, run_unpack, run_verify, unpack, verify, verify_json_schema,
    verify_to_envelope, PackRepackOutcome, PackUnpackOutcome, PackVerifyJsonData,
    PACK_VERIFY_SCHEMA_VERSION,
};

pub fn build(args: &BuildArgs) -> Result<PackOutcome, PackError> {
    if args.sign && args.unsigned {
        return Err(PackError::new(
            "pack.sign_conflict",
            "--sign and --unsigned cannot be used together",
        ));
    }
    if args.sign && args.key.is_none() {
        return Err(PackError::new(
            "pack.sign_missing_key",
            "--sign requires --key <path>",
        ));
    }
    if !args.sign && args.key.is_some() {
        return Err(PackError::new(
            "pack.key_without_sign",
            "--key requires --sign",
        ));
    }
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
    let entrypoint_input = args.entrypoint.clone();
    let entrypoint = entrypoint_input
        .canonicalize()
        .unwrap_or_else(|_| entrypoint_input.clone());
    if !entrypoint.exists() {
        return Err(PackError::new(
            "entrypoint.not_found",
            format!("entrypoint does not exist: {}", entrypoint_input.display()),
        ));
    }
    if !entrypoint.is_file() || entrypoint.extension().and_then(|ext| ext.to_str()) != Some("harn")
    {
        return Err(PackError::new(
            "entrypoint.invalid",
            format!(
                "entrypoint must be a .harn file: {}",
                entrypoint_input.display()
            ),
        ));
    }
    if args.exclude_secrets && path_looks_like_secret(&entrypoint) {
        return Err(PackError::new(
            "pack.secret_blocked",
            format!(
                "entrypoint {} matches a secret-bearing path pattern; \
                 re-run with --include-secrets to override",
                entrypoint_input.display()
            ),
        ));
    }
    let project_root = pack_archive_root(&entrypoint);
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
    let entrypoint_id = logical_bundle_path(&entrypoint_rel);

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
    let mut graph_paths = graph.module_paths();
    // The entrypoint is always present in the graph; ensure deterministic order.
    graph_paths.sort();
    let mut module_paths: Vec<PathBuf> = graph_paths
        .iter()
        .filter(|path| is_harn_module_path(path))
        .cloned()
        .collect();
    module_paths.sort();

    let mut transitive_modules = Vec::new();
    let mut contents = Vec::new();
    let mut sbom_packages = Vec::new();
    let mut sbom_relationships = Vec::new();
    let mut warnings = Vec::new();
    let mut skipped_assets = Vec::new();
    let mut debug_symbol_metadata = PackDebugSymbolMetadata {
        harnbc_count: 0,
        total_bytes: 0,
    };

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
                from: format!("entrypoint:{entrypoint_id}"),
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

        let imported_enum_candidates =
            crate::imported_enum_candidates_for_source(module_path, &source);
        let entry_chunk =
            crate::compiler_with_imported_enum_candidates(imported_enum_candidates.iter().cloned())
                .compile(&program)
                .map_err(|err| {
                    PackError::new(
                        "module.compile_failed",
                        format!("compile error in {}: {err}", module_path.display()),
                    )
                })?;

        let module_artifact_opt =
            module_artifact::compile_module_artifact_from_source_with_imported_enums(
                module_path,
                &source,
                imported_enum_candidates,
            )
            .ok();

        let entry_cache_key = bytecode_cache::CacheKey::from_source(module_path, &source);
        let chunk_bytes = bytecode_cache::serialize_chunk_artifact(&entry_cache_key, &entry_chunk)
            .map_err(|err| {
                PackError::new(
                    "module.serialize_failed",
                    format!(
                        "failed to serialize chunk for {}: {err}",
                        module_path.display()
                    ),
                )
            })?;

        let module_artifact_bytes = module_artifact_opt
            .as_ref()
            .map(|artifact| {
                let module_cache_key = bytecode_cache::CacheKey::from_module_source(
                    &harn_vm::module_source::ModuleSource::from_text(source.as_str()),
                );
                bytecode_cache::serialize_module_artifact(&module_cache_key, artifact).map_err(
                    |err| {
                        PackError::new(
                            "module.serialize_failed",
                            format!(
                                "failed to serialize module artifact for {}: {err}",
                                module_path.display()
                            ),
                        )
                    },
                )
            })
            .transpose()?;

        let rel = relativize(&project_root, module_path).ok_or_else(|| {
            PackError::new(
                "module.outside_root",
                format!(
                    "module {} resolves outside pack archive root {}; add a harn.toml at the intended project root or keep imports inside it",
                    module_path.display(),
                    project_root.display()
                ),
            )
        })?;
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
        debug_symbol_metadata.harnbc_count += 1;
        debug_symbol_metadata.total_bytes += chunk_bytes.len() as u64;

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
            debug_symbol_metadata.total_bytes += artifact_bytes.len() as u64;
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

        let module_id = logical_bundle_path(&rel);
        if module_path != &entrypoint {
            sbom_relationships.push(SBOMRelationship {
                from: format!("entrypoint:{entrypoint_id}"),
                to: format!("module:{module_id}"),
                relationship_type: "depends_on".to_string(),
            });
        }
        sbom_packages.push(SBOMPackage {
            name: format!("module:{module_id}"),
            version: Some(harn_version.clone()),
            package_hash_blake3: Some(source_hash),
            license: None,
        });
    }

    for asset in discover_import_assets(&graph, &module_paths, &project_root)? {
        if args.exclude_secrets && path_looks_like_secret(&asset.path) {
            warnings.push(JsonWarning {
                code: "pack.asset_skipped_secret".to_string(),
                message: format!(
                    "skipped imported asset {} because it matches a secret-bearing path pattern",
                    asset.rel.display()
                ),
            });
            skipped_assets.push(SkippedAsset {
                path: logical_bundle_path(&asset.rel),
                reason: "secret_path".to_string(),
            });
            continue;
        }

        let bytes = std::fs::read(&asset.path).map_err(|err| {
            PackError::new(
                "asset.read_failed",
                format!(
                    "failed to read imported asset {}: {err}",
                    asset.path.display()
                ),
            )
        })?;
        let asset_hash = blake3_hash(&bytes);
        let asset_id = logical_bundle_path(&asset.rel);
        contents.push(HarnpackEntry::new(
            PathBuf::from("sources").join(&asset.rel),
            bytes,
        ));
        sbom_packages.push(SBOMPackage {
            name: format!("asset:{asset_id}"),
            version: Some(harn_version.clone()),
            package_hash_blake3: Some(asset_hash),
            license: None,
        });
        sbom_relationships.push(SBOMRelationship {
            from: format!("entrypoint:{entrypoint_id}"),
            to: format!("asset:{asset_id}"),
            relationship_type: "depends_on".to_string(),
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

    let provider_catalog = harn_vm::provider_catalog::artifact();
    let provider_catalog_bytes = serde_json::to_vec(&provider_catalog).map_err(|err| {
        PackError::new(
            "provider_catalog.failed",
            format!("failed to serialize provider catalog snapshot: {err}"),
        )
    })?;
    let provider_catalog_hash = blake3_hash(&provider_catalog_bytes);
    sbom_packages.push(SBOMPackage {
        name: "harn-provider-catalog".to_string(),
        version: Some(harn_version.clone()),
        package_hash_blake3: Some(provider_catalog_hash.clone()),
        license: None,
    });
    sbom_relationships.push(SBOMRelationship {
        from: format!("entrypoint:{entrypoint_id}"),
        to: "harn-provider-catalog".to_string(),
        relationship_type: "depends_on".to_string(),
    });
    for provider in &provider_catalog.providers {
        let provider_name = format!("provider:{}", provider.id);
        sbom_packages.push(SBOMPackage {
            name: provider_name.clone(),
            version: None,
            package_hash_blake3: None,
            license: None,
        });
        sbom_relationships.push(SBOMRelationship {
            from: "harn-provider-catalog".to_string(),
            to: provider_name,
            relationship_type: "contains".to_string(),
        });
    }

    // Tool entries use the same manifest/SBOM path as modules and
    // providers, keeping the archive representation centralized.
    let tool_manifest: Vec<ToolEntry> = Vec::new();
    for tool in &tool_manifest {
        sbom_packages.push(SBOMPackage {
            name: format!("tool:{}", tool.name),
            version: None,
            package_hash_blake3: tool.schema_hash_blake3.clone(),
            license: None,
        });
        sbom_relationships.push(SBOMRelationship {
            from: format!("entrypoint:{entrypoint_id}"),
            to: format!("tool:{}", tool.name),
            relationship_type: "depends_on".to_string(),
        });
    }
    let mut bundle = assemble_bundle(
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
    if !skipped_assets.is_empty() {
        bundle.metadata.insert(
            "skipped_assets".to_string(),
            serde_json::to_value(&skipped_assets).map_err(|err| {
                PackError::new(
                    "pack.metadata_failed",
                    format!("failed to render skipped asset metadata: {err}"),
                )
            })?,
        );
    }
    // Carry host-surface extension data — the `[[contributes]]` block plus
    // package identity/permissions — into the signed bundle's metadata so a
    // host (e.g. an IDE) can discover and gate contributions from the verified
    // artifact alone, with no separate descriptor. Harn stays agnostic about
    // the kind-specific payload; it only ferries it. See `docs` and the
    // `ContributionEntry` schema.
    // Also bundle files referenced by the `[[contributes]]` block (preview
    // HTML, theme JSON, canon dir, skill, …) so an imported pack is
    // self-contained for the host.
    contents.extend(carry_extension_metadata(&project_root, &mut bundle)?);
    sort_sbom_doc(&mut bundle.sbom);
    let sbom_bytes = serde_json::to_vec_pretty(&bundle.sbom).map_err(|err| {
        PackError::new(
            "pack.sbom_failed",
            format!("failed to render SBOM document: {err}"),
        )
    })?;
    contents.push(HarnpackEntry::new(PACK_SBOM_ARCHIVE_PATH, sbom_bytes));

    if args.sign {
        let key_path = args.key.as_ref().expect("checked above");
        sign_bundle(&mut bundle, &contents, key_path)?;
    }

    let bundle_hash = workflow_bundle_hash(&bundle, &contents).map_err(|err| {
        PackError::new(
            "pack.hash_failed",
            format!("failed to compute bundle hash: {err}"),
        )
    })?;
    let archive_bytes = build_harnpack(&bundle, &contents).map_err(|err| {
        PackError::new(
            "pack.archive_failed",
            format!("failed to assemble .harnpack archive: {err}"),
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
    emit_release_trust_record(&project_root, &bundle_hash, &bundle.harn_version, args.sign)?;

    Ok(PackOutcome {
        bundle_hash: bundle_hash.clone(),
        output_path: output_path.clone(),
        size_bytes,
        json: PackJsonData {
            bundle_hash,
            output_path,
            size_bytes,
            signature: signature_summary(&bundle),
            sbom_summary: sbom_summary(&bundle),
            debug_symbol_metadata,
            manifest: bundle,
        },
        warnings,
    })
}

fn sign_bundle(
    bundle: &mut WorkflowBundle,
    contents: &[HarnpackEntry],
    key_path: &Path,
) -> Result<(), PackError> {
    let signing_key = skill_provenance::load_ed25519_signing_key(key_path).map_err(|err| {
        PackError::new(
            "pack.sign_key_failed",
            format!("failed to load signing key {}: {err}", key_path.display()),
        )
    })?;
    let bundle_hash = workflow_bundle_hash(bundle, contents).map_err(|err| {
        PackError::new(
            "pack.hash_failed",
            format!("failed to compute bundle hash before signing: {err}"),
        )
    })?;
    let verifying_key = signing_key.verifying_key();
    let signature = signing_key.sign(bundle_hash.as_bytes());
    bundle.signature = Some(Ed25519Signature {
        key_id: Some(skill_provenance::fingerprint_for_key(&verifying_key)),
        public_key: hex::encode(verifying_key.to_bytes()),
        signature: hex::encode(signature.to_bytes()),
        manifest_hash_blake3: bundle_hash,
        algorithm: "ed25519".to_string(),
    });
    Ok(())
}

fn emit_release_trust_record(
    project_root: &Path,
    bundle_hash: &str,
    harn_version: &str,
    signed: bool,
) -> Result<TrustRecord, PackError> {
    let log = harn_vm::event_log::install_default_for_base_dir(project_root).map_err(|err| {
        PackError::new(
            "pack.trust_log_failed",
            format!(
                "failed to open OpenTrustGraph event log under {}: {err}",
                project_root.display()
            ),
        )
    })?;
    let parent_trust_record_id = futures::executor::block_on(harn_vm::query_trust_records(
        &log,
        &harn_vm::TrustQueryFilters::default(),
    ))
    .map_err(|err| {
        PackError::new(
            "pack.trust_query_failed",
            format!("failed to query prior OpenTrustGraph records: {err}"),
        )
    })?
    .last()
    .map(|record| record.record_id.clone());
    let mut record = TrustRecord::release(
        std::env::var("USER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "harn-pack".to_string()),
        bundle_hash.to_string(),
        harn_version.to_string(),
        parent_trust_record_id,
        format!("harnpack-release-{}", uuid::Uuid::now_v7()),
        if signed {
            AutonomyTier::ActAuto
        } else {
            AutonomyTier::Suggest
        },
    );
    record
        .metadata
        .insert("signed".to_string(), serde_json::json!(signed));
    futures::executor::block_on(harn_vm::append_trust_record(&log, &record)).map_err(|err| {
        PackError::new(
            "pack.trust_record_failed",
            format!("failed to append OpenTrustGraph release record: {err}"),
        )
    })
}

fn signature_summary(bundle: &WorkflowBundle) -> PackSignatureSummary {
    match &bundle.signature {
        Some(signature) => PackSignatureSummary {
            algorithm: signature.algorithm.clone(),
            key_id: signature.key_id.clone(),
            present: true,
        },
        None => PackSignatureSummary {
            algorithm: "ed25519".to_string(),
            key_id: None,
            present: false,
        },
    }
}

fn sbom_summary(bundle: &WorkflowBundle) -> PackSbomSummary {
    let stdlib_modules = bundle
        .sbom
        .packages
        .iter()
        .filter(|package| package.name.starts_with("std/"))
        .count();
    let providers = bundle
        .sbom
        .packages
        .iter()
        .filter(|package| package.name.starts_with("provider:"))
        .count();
    PackSbomSummary {
        components: bundle.sbom.packages.len(),
        stdlib_modules,
        providers,
        tools: bundle.tool_manifest.len(),
    }
}

#[derive(Debug)]
struct ImportedAsset {
    path: PathBuf,
    rel: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct SkippedAsset {
    path: String,
    reason: String,
}

fn discover_import_assets(
    graph: &harn_modules::ModuleGraph,
    module_paths: &[PathBuf],
    project_root: &Path,
) -> Result<Vec<ImportedAsset>, PackError> {
    let mut assets = BTreeMap::<PathBuf, ImportedAsset>::new();
    for module_path in module_paths {
        if module_path.to_string_lossy().starts_with("<std>/") {
            continue;
        }
        for import in graph.imports_for_module(module_path) {
            let Some(resolved_path) = import.resolved_path else {
                continue;
            };
            if is_harn_module_path(&resolved_path) {
                continue;
            }
            let canonical = resolved_path
                .canonicalize()
                .unwrap_or_else(|_| resolved_path.clone());
            let rel = relativize(project_root, &canonical).ok_or_else(|| {
                PackError::new(
                    "asset.outside_root",
                    format!(
                        "imported asset {} resolves outside pack archive root {}; add a harn.toml at the intended project root or keep imports inside it",
                        canonical.display(),
                        project_root.display()
                    ),
                )
            })?;
            assets.entry(canonical.clone()).or_insert(ImportedAsset {
                path: canonical,
                rel,
            });
        }
    }
    Ok(assets.into_values().collect())
}

fn is_harn_module_path(path: &Path) -> bool {
    path.to_string_lossy().starts_with("<std>/")
        || path.extension().and_then(|ext| ext.to_str()) == Some("harn")
}

fn sort_sbom_doc(sbom: &mut SBOMDoc) {
    sbom.packages.sort_by(|left, right| {
        (&left.name, &left.version, &left.package_hash_blake3).cmp(&(
            &right.name,
            &right.version,
            &right.package_hash_blake3,
        ))
    });
    sbom.relationships.sort_by(|left, right| {
        (&left.from, &left.to, &left.relationship_type).cmp(&(
            &right.from,
            &right.to,
            &right.relationship_type,
        ))
    });
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

/// Carry the package's host-surface extension data into the signed bundle's
/// metadata. Reads the nearest `harn.toml`, serializing the `[[contributes]]`
/// block under the `contributes` key and package identity/permissions under
/// `extension`. No-op when there is no manifest, it fails to parse, or there
/// are no contributions — packing a plain workflow is unaffected.
fn carry_extension_metadata(
    project_root: &Path,
    bundle: &mut WorkflowBundle,
) -> Result<Vec<HarnpackEntry>, PackError> {
    let manifest_path = project_root.join("harn.toml");
    if !manifest_path.is_file() {
        return Ok(Vec::new());
    }
    let Ok(text) = std::fs::read_to_string(&manifest_path) else {
        return Ok(Vec::new());
    };
    let manifest: crate::package::Manifest = match toml::from_str(&text) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(Vec::new()),
    };
    // Validate before sealing into the signed bundle so a malformed
    // `[[contributes]]` block fails the pack rather than shipping silently.
    crate::package::validate_contributions(&manifest)
        .map_err(|err| PackError::new("pack.invalid_contributes", err.message().to_string()))?;
    if !manifest.contributes.is_empty() {
        bundle.metadata.insert(
            "contributes".to_string(),
            serde_json::to_value(&manifest.contributes).map_err(|err| {
                PackError::new(
                    "pack.metadata_failed",
                    format!("failed to render contributions: {err}"),
                )
            })?,
        );
    }
    if let Some(pkg) = manifest.package.as_ref() {
        bundle.metadata.insert(
            "extension".to_string(),
            serde_json::json!({
                "name": pkg.name,
                "version": pkg.version,
                "publisher": pkg.publisher,
                "contact": pkg.contact,
                "created": pkg.created,
                "description": pkg.description,
                "license": pkg.license,
                "permissions": pkg.permissions,
                "host_requirements": pkg.host_requirements,
            }),
        );
    }
    collect_contribution_assets(project_root, &manifest.contributes)
}

/// Bundle files referenced by `[[contributes]]` config (`entry`/`file`/`path`)
/// at their bundle-relative archive paths so a host can load them after import
/// (the preview HTML, theme JSON, canon dir, skill, …). `harn pack` otherwise
/// only walks the entrypoint's transitive imports, which never reach these.
/// Paths must stay inside the archive root; `..` escapes are skipped.
fn collect_contribution_assets(
    project_root: &Path,
    contributes: &[crate::package::ContributionEntry],
) -> Result<Vec<HarnpackEntry>, PackError> {
    const ASSET_KEYS: [&str; 3] = ["entry", "file", "path"];
    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    let mut entries: Vec<HarnpackEntry> = Vec::new();
    for contribution in contributes {
        for key in ASSET_KEYS {
            let Some(rel) = contribution.config.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            let rel_path = PathBuf::from(rel);
            // Reject absolute paths and `..` escapes.
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
            {
                continue;
            }
            collect_path(project_root, &rel_path, &mut seen, &mut entries)?;
        }
    }
    Ok(entries)
}

/// Recursively add `rel` (file or directory, relative to `project_root`) to the
/// archive at its bundle-relative path. No-op when the path is missing.
fn collect_path(
    project_root: &Path,
    rel: &Path,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    entries: &mut Vec<HarnpackEntry>,
) -> Result<(), PackError> {
    let abs = project_root.join(rel);
    let metadata = match std::fs::symlink_metadata(&abs) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(()); // never follow symlinks out of the archive root
    }
    if metadata.is_dir() {
        let mut children: Vec<PathBuf> = std::fs::read_dir(&abs)
            .map_err(|err| {
                PackError::new(
                    "asset.read_failed",
                    format!("failed to read contribution dir {}: {err}", abs.display()),
                )
            })?
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .map(|name| rel.join(name))
            .collect();
        children.sort();
        for child in children {
            collect_path(project_root, &child, seen, entries)?;
        }
        return Ok(());
    }
    if !metadata.is_file() || !seen.insert(rel.to_path_buf()) {
        return Ok(());
    }
    let bytes = std::fs::read(&abs).map_err(|err| {
        PackError::new(
            "asset.read_failed",
            format!("failed to read contribution asset {}: {err}", abs.display()),
        )
    })?;
    entries.push(HarnpackEntry::new(
        PathBuf::from(logical_bundle_path(rel)),
        bytes,
    ));
    Ok(())
}

fn pack_archive_root(entrypoint: &Path) -> PathBuf {
    let parent = entrypoint.parent().unwrap_or_else(|| Path::new("."));
    harn_modules::asset_paths::find_project_root(parent).unwrap_or_else(|| parent.to_path_buf())
}

fn relativize(root: &Path, target: &Path) -> Option<PathBuf> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let target_canon = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if let Ok(rel) = target_canon.strip_prefix(&root_canon) {
        return Some(rel.to_path_buf());
    }
    None
}

pub(super) fn adjacent_with_extension(rel: &Path, extension: &str) -> Option<PathBuf> {
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

pub(super) fn blake3_hash(bytes: &[u8]) -> String {
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

/// Heuristic gate for `--exclude-secrets`. Matches `.env`, `.env.*`,
/// `*.pem`, `*.key`, `credentials*`, and any path under a `secrets/`
/// directory. Kept conservative so false positives don't strand
/// legitimate bundles; mirrors common git secret-scanning policies.
pub(crate) fn path_looks_like_secret(path: &Path) -> bool {
    let lower_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if lower_name == ".env" || lower_name.starts_with(".env.") {
        return true;
    }
    if lower_name.starts_with("credentials") {
        return true;
    }
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if ext == "pem" || ext == "key" {
            return true;
        }
    }
    for component in path.components() {
        if let Component::Normal(part) = component {
            if part.to_string_lossy().eq_ignore_ascii_case("secrets") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn build_args(entrypoint: PathBuf, out: PathBuf) -> BuildArgs {
        BuildArgs {
            entrypoint,
            out: Some(out),
            upgrade: None,
            sign: false,
            key: None,
            unsigned: true,
            exclude_secrets: false,
            json: true,
        }
    }

    #[test]
    fn carry_extension_metadata_injects_contributes_and_identity() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("harn.toml"),
            r#"
[package]
name = "harn-latex"
version = "0.1.0"
publisher = "Burin Labs"
permissions = ["workspace:read_text"]

[[contributes]]
kind = "editor.language"
id = "latex"
scopes = ["workspace:read_text"]
languageId = "latex"
"#,
        )
        .unwrap();
        let mut bundle = WorkflowBundle::default();
        carry_extension_metadata(temp.path(), &mut bundle).unwrap();

        let contributes = bundle
            .metadata
            .get("contributes")
            .expect("contributes carried");
        assert_eq!(contributes.as_array().unwrap().len(), 1);
        assert_eq!(contributes[0]["kind"], "editor.language");
        // kind-specific keys are flattened into the contribution object
        assert_eq!(contributes[0]["languageId"], "latex");

        let ext = bundle.metadata.get("extension").expect("identity carried");
        assert_eq!(ext["name"], "harn-latex");
        assert_eq!(ext["publisher"], "Burin Labs");
        assert_eq!(ext["permissions"][0], "workspace:read_text");
    }

    #[test]
    fn carry_extension_metadata_bundles_contribution_assets() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("assets")).unwrap();
        fs::write(temp.path().join("assets/preview.html"), "<html></html>").unwrap();
        fs::create_dir_all(temp.path().join("canon/latex")).unwrap();
        fs::write(temp.path().join("canon/latex/invariants.harn"), "// rules").unwrap();
        fs::write(temp.path().join("SKILL.md"), "# skill").unwrap();
        fs::write(
            temp.path().join("harn.toml"),
            r#"
[package]
name = "harn-latex"
permissions = ["workspace:read_text"]

[[contributes]]
kind = "editor.preview"
id = "p"
scopes = ["workspace:read_text"]
entry = "assets/preview.html"

[[contributes]]
kind = "harn.canon"
id = "c"
path = "canon/latex"

[[contributes]]
kind = "harn.skill"
id = "s"
path = "SKILL.md"
"#,
        )
        .unwrap();
        let mut bundle = WorkflowBundle::default();
        let assets = carry_extension_metadata(temp.path(), &mut bundle).unwrap();
        let paths: std::collections::BTreeSet<String> = assets
            .iter()
            .map(|e| crate::format::slash_path(&e.path))
            .collect();
        assert!(paths.contains("assets/preview.html"), "{paths:?}");
        assert!(paths.contains("canon/latex/invariants.harn"), "{paths:?}");
        assert!(paths.contains("SKILL.md"), "{paths:?}");
    }

    #[test]
    fn carry_extension_metadata_skips_parent_escape_assets() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("harn.toml"),
            "[package]\nname = \"x\"\npermissions = []\n\n[[contributes]]\nkind = \"editor.preview\"\nid = \"p\"\nentry = \"../escape.html\"\n",
        )
        .unwrap();
        let mut bundle = WorkflowBundle::default();
        let assets = carry_extension_metadata(temp.path(), &mut bundle).unwrap();
        assert!(assets.is_empty());
    }

    #[test]
    fn carry_extension_metadata_is_noop_without_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let mut bundle = WorkflowBundle::default();
        carry_extension_metadata(temp.path(), &mut bundle).unwrap();
        assert!(!bundle.metadata.contains_key("contributes"));
    }

    #[test]
    fn pack_uses_nearest_harn_toml_root_for_nested_entrypoint_assets() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("harn.toml"),
            "[package]\nname = \"pack-root\"\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("scripts")).unwrap();
        fs::create_dir_all(temp.path().join("assets")).unwrap();
        fs::write(temp.path().join("assets/prompt.txt"), "prompt asset\n").unwrap();
        fs::write(
            temp.path().join("scripts/entry.harn"),
            "import \"../assets/prompt.txt\"\n__io_println(\"packed\")\n",
        )
        .unwrap();

        let outcome = build(&build_args(
            temp.path().join("scripts/entry.harn"),
            temp.path().join("bundle.harnpack"),
        ))
        .unwrap();

        assert_eq!(
            outcome.json.manifest.entrypoint,
            PathBuf::from("scripts/entry.harn")
        );
        assert!(outcome
            .json
            .manifest
            .sbom
            .packages
            .iter()
            .any(|package| package.name == "asset:assets/prompt.txt"));
    }

    #[test]
    fn pack_rejects_imported_asset_outside_archive_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("prompt.txt"), "outside asset\n").unwrap();
        fs::write(
            root.join("entry.harn"),
            "import \"../outside/prompt.txt\"\n__io_println(\"packed\")\n",
        )
        .unwrap();

        let err = build(&build_args(
            root.join("entry.harn"),
            root.join("bundle.harnpack"),
        ))
        .unwrap_err();

        assert_eq!(err.code, "asset.outside_root");
        assert!(!root.join("bundle.harnpack").exists());
    }
}
