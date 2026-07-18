//! Archive input/output for `harn pack`: safe unpack/repack and bundle verification.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process;

use ed25519_dalek::VerifyingKey;
use harn_vm::bytecode_cache;
use harn_vm::orchestration::{
    build_harnpack, load_workflow_bundle_any_version, read_harnpack,
    verify_workflow_bundle_signature, workflow_bundle_hash, Ed25519Signature, HarnpackEntry,
    WorkflowBundle, HARNPACK_MANIFEST_PATH,
};
use serde::Serialize;

use crate::cli::{PackRepackArgs, PackUnpackArgs, PackVerifyArgs};
use crate::command_error;
use crate::json_envelope::{to_string_pretty, JsonEnvelope, JsonOutput};
use crate::skill_provenance;

use super::{PackError, DEFAULT_PACK_FILE_MODE};

#[derive(Debug)]
pub struct PackUnpackOutcome {
    pub output_dir: PathBuf,
    pub content_entry_count: usize,
}

#[derive(Debug)]
pub struct PackRepackOutcome {
    pub output_path: PathBuf,
    pub size_bytes: u64,
    pub content_entry_count: usize,
}

pub fn run_unpack(args: PackUnpackArgs) {
    match unpack(&args) {
        Ok(outcome) => {
            println!(
                "unpacked {} to {} ({} payload entries)",
                args.bundle.display(),
                outcome.output_dir.display(),
                outcome.content_entry_count
            );
        }
        Err(err) => command_error(&err.message),
    }
}

pub fn run_repack(args: PackRepackArgs) {
    match repack(&args) {
        Ok(outcome) => {
            println!(
                "repacked {} to {} ({} payload entries, {} bytes)",
                args.dir.display(),
                outcome.output_path.display(),
                outcome.content_entry_count,
                outcome.size_bytes
            );
        }
        Err(err) => command_error(&err.message),
    }
}

pub fn unpack(args: &PackUnpackArgs) -> Result<PackUnpackOutcome, PackError> {
    let bytes = fs::read(&args.bundle).map_err(|err| {
        PackError::new(
            "unpack.read_failed",
            format!("failed to read {}: {err}", args.bundle.display()),
        )
    })?;
    let archive = read_harnpack(&bytes).map_err(|err| {
        PackError::new(
            "unpack.archive_failed",
            format!("failed to parse {}: {err}", args.bundle.display()),
        )
    })?;
    prepare_unpack_dir(&args.out, args.force)?;

    let manifest_bytes = serde_json::to_vec_pretty(&archive.manifest).map_err(|err| {
        PackError::new(
            "unpack.manifest_failed",
            format!("failed to encode {HARNPACK_MANIFEST_PATH}: {err}"),
        )
    })?;
    write_unpack_file(
        &args.out,
        Path::new(HARNPACK_MANIFEST_PATH),
        &manifest_bytes,
        DEFAULT_PACK_FILE_MODE,
    )?;
    for entry in &archive.contents {
        write_unpack_file(&args.out, &entry.path, &entry.bytes, entry.mode)?;
    }

    Ok(PackUnpackOutcome {
        output_dir: args.out.clone(),
        content_entry_count: archive.contents.len(),
    })
}

pub fn repack(args: &PackRepackArgs) -> Result<PackRepackOutcome, PackError> {
    if !args.dir.is_dir() {
        return Err(PackError::new(
            "repack.input_not_directory",
            format!("input is not a directory: {}", args.dir.display()),
        ));
    }
    reject_repack_output_inside_input(&args.dir, &args.out)?;
    if args.out.exists() && !args.force {
        return Err(PackError::new(
            "repack.output_exists",
            format!(
                "output path already exists: {} (re-run with --force to replace it)",
                args.out.display()
            ),
        ));
    }
    if args.out.is_dir() {
        return Err(PackError::new(
            "repack.output_is_directory",
            format!("output path is a directory: {}", args.out.display()),
        ));
    }

    let manifest_path = args.dir.join(HARNPACK_MANIFEST_PATH);
    let manifest = load_workflow_bundle_any_version(&manifest_path).map_err(|err| {
        PackError::new(
            "repack.manifest_failed",
            format!("failed to read {}: {err}", manifest_path.display()),
        )
    })?;
    let contents = collect_repack_entries(&args.dir)?;
    let archive_bytes = build_harnpack(&manifest, &contents).map_err(|err| {
        PackError::new(
            "repack.archive_failed",
            format!("failed to assemble {}: {err}", args.out.display()),
        )
    })?;
    if let Some(parent) = args
        .out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            PackError::new(
                "repack.write_failed",
                format!("failed to create {}: {err}", parent.display()),
            )
        })?;
    }
    fs::write(&args.out, &archive_bytes).map_err(|err| {
        PackError::new(
            "repack.write_failed",
            format!("failed to write {}: {err}", args.out.display()),
        )
    })?;

    Ok(PackRepackOutcome {
        output_path: args.out.clone(),
        size_bytes: archive_bytes.len() as u64,
        content_entry_count: contents.len(),
    })
}

fn prepare_unpack_dir(out: &Path, force: bool) -> Result<(), PackError> {
    if out.exists() {
        if !force {
            return Err(PackError::new(
                "unpack.output_exists",
                format!(
                    "output path already exists: {} (re-run with --force to replace it)",
                    out.display()
                ),
            ));
        }
        let metadata = fs::symlink_metadata(out).map_err(|err| {
            PackError::new(
                "unpack.read_failed",
                format!("failed to stat {}: {err}", out.display()),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PackError::new(
                "unpack.output_symlink",
                format!("refusing to replace symlink {}", out.display()),
            ));
        }
        if metadata.is_dir() {
            require_prior_unpack_dir(out)?;
            fs::remove_dir_all(out).map_err(|err| {
                PackError::new(
                    "unpack.remove_failed",
                    format!("failed to remove {}: {err}", out.display()),
                )
            })?;
        } else if metadata.is_file() {
            fs::remove_file(out).map_err(|err| {
                PackError::new(
                    "unpack.remove_failed",
                    format!("failed to remove {}: {err}", out.display()),
                )
            })?;
        } else {
            return Err(PackError::new(
                "unpack.output_unsupported",
                format!("refusing to replace non-file output path {}", out.display()),
            ));
        }
    }
    fs::create_dir_all(out).map_err(|err| {
        PackError::new(
            "unpack.write_failed",
            format!("failed to create {}: {err}", out.display()),
        )
    })
}

fn require_prior_unpack_dir(out: &Path) -> Result<(), PackError> {
    if is_current_dir(out) {
        return Err(PackError::new(
            "unpack.output_unsafe",
            format!("refusing to replace current directory {}", out.display()),
        ));
    }
    let manifest = out.join(HARNPACK_MANIFEST_PATH);
    if manifest.is_file() {
        return Ok(());
    }
    Err(PackError::new(
        "unpack.output_not_harnpack_dir",
        format!(
            "refusing to remove {} because it does not contain {}; choose a fresh --out dir or remove it manually",
            out.display(),
            HARNPACK_MANIFEST_PATH
        ),
    ))
}

fn is_current_dir(path: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let Ok(cwd) = env::current_dir().and_then(|cwd| cwd.canonicalize()) else {
        return false;
    };
    path == cwd
}

fn write_unpack_file(
    root: &Path,
    archive_path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), PackError> {
    let safe_path = normalize_safe_archive_path(archive_path)?;
    let destination = root.join(&safe_path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            PackError::new(
                "unpack.write_failed",
                format!("failed to create {}: {err}", parent.display()),
            )
        })?;
    }
    fs::write(&destination, bytes).map_err(|err| {
        PackError::new(
            "unpack.write_failed",
            format!("failed to write {}: {err}", destination.display()),
        )
    })?;
    set_file_mode(&destination, mode)?;
    Ok(())
}

fn collect_repack_entries(root: &Path) -> Result<Vec<HarnpackEntry>, PackError> {
    let mut entries = Vec::new();
    collect_repack_entries_inner(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn reject_repack_output_inside_input(input_dir: &Path, out: &Path) -> Result<(), PackError> {
    let input = input_dir.canonicalize().map_err(|err| {
        PackError::new(
            "repack.read_failed",
            format!("failed to canonicalize {}: {err}", input_dir.display()),
        )
    })?;
    let out_parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let out_file_name = out.file_name().ok_or_else(|| {
        PackError::new(
            "repack.output_invalid",
            format!("output path must include a file name: {}", out.display()),
        )
    })?;
    let out_parent = out_parent.canonicalize().unwrap_or_else(|_| {
        if out_parent.is_absolute() {
            out_parent.to_path_buf()
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(out_parent)
        }
    });
    let output = out_parent.join(out_file_name);
    if output.starts_with(&input) {
        return Err(PackError::new(
            "repack.output_inside_input",
            format!(
                "refusing to write {} inside input directory {}; choose an output path outside the unpacked tree",
                out.display(),
                input_dir.display()
            ),
        ));
    }
    Ok(())
}

fn collect_repack_entries_inner(
    root: &Path,
    current: &Path,
    entries: &mut Vec<HarnpackEntry>,
) -> Result<(), PackError> {
    let mut children = fs::read_dir(current)
        .map_err(|err| {
            PackError::new(
                "repack.read_failed",
                format!("failed to read {}: {err}", current.display()),
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            PackError::new(
                "repack.read_failed",
                format!("failed to read {}: {err}", current.display()),
            )
        })?;
    children.sort_by_key(|entry| entry.path());

    for child in children {
        let path = child.path();
        let metadata = fs::symlink_metadata(&path).map_err(|err| {
            PackError::new(
                "repack.read_failed",
                format!("failed to stat {}: {err}", path.display()),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(PackError::new(
                "repack.unsupported_entry",
                format!("refusing to pack symlink {}", path.display()),
            ));
        }
        if metadata.is_dir() {
            collect_repack_entries_inner(root, &path, entries)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(PackError::new(
                "repack.unsupported_entry",
                format!("refusing to pack non-file entry {}", path.display()),
            ));
        }

        let rel = path.strip_prefix(root).map_err(|err| {
            PackError::new(
                "repack.path_failed",
                format!(
                    "failed to relativize {} against {}: {err}",
                    path.display(),
                    root.display()
                ),
            )
        })?;
        let archive_path = normalize_safe_archive_path(rel)?;
        if archive_path == Path::new(HARNPACK_MANIFEST_PATH) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|err| {
            PackError::new(
                "repack.read_failed",
                format!("failed to read {}: {err}", path.display()),
            )
        })?;
        entries.push(HarnpackEntry::new(archive_path, bytes).with_mode(file_mode(&metadata)));
    }
    Ok(())
}

fn normalize_safe_archive_path(path: &Path) -> Result<PathBuf, PackError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(PackError::new(
                    "pack.unsafe_archive_path",
                    format!("archive path may not contain '..': {}", path.display()),
                ));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(PackError::new(
                    "pack.unsafe_archive_path",
                    format!("archive path must be relative: {}", path.display()),
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(PackError::new(
            "pack.unsafe_archive_path",
            "archive path may not be empty",
        ));
    }
    Ok(normalized)
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    DEFAULT_PACK_FILE_MODE
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<(), PackError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|err| {
        PackError::new(
            "unpack.write_failed",
            format!("failed to set permissions on {}: {err}", path.display()),
        )
    })
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<(), PackError> {
    Ok(())
}

// --- `harn pack verify` -----------------------------------------------------

/// Stable schema version for the `harn pack verify --json` envelope.
/// Bump when [`PackVerifyJsonData`] changes shape in a way agents need
/// to detect.
pub const PACK_VERIFY_SCHEMA_VERSION: u32 = 1;

/// JSON payload emitted under `JsonEnvelope.data` for `harn pack verify`.
#[derive(Debug, Clone, Serialize)]
pub struct PackVerifyJsonData {
    pub bundle: PathBuf,
    pub bundle_hash: String,
    pub recorded_bundle_hash: Option<String>,
    pub signature_present: bool,
    pub signature_verified: bool,
    pub key_id: Option<String>,
    pub schema_version: u32,
    pub entrypoint: PathBuf,
    pub module_count: usize,
    pub content_entry_count: usize,
}

struct PackVerifyJsonOutput(PackVerifyJsonData);

impl JsonOutput for PackVerifyJsonOutput {
    const SCHEMA_VERSION: u32 = PACK_VERIFY_SCHEMA_VERSION;
    type Data = PackVerifyJsonData;
    fn into_envelope(self) -> JsonEnvelope<Self::Data> {
        JsonEnvelope::ok(Self::SCHEMA_VERSION, self.0)
    }
}

/// JSON schema for `harn pack verify --json`. Mirrors the runtime
/// envelope so agents can validate output before consuming it.
pub fn verify_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "harn pack verify --json",
        "type": "object",
        "required": ["schemaVersion", "ok", "data", "warnings"],
        "properties": {
            "schemaVersion": { "const": PACK_VERIFY_SCHEMA_VERSION },
            "ok": { "type": "boolean" },
            "warnings": { "type": "array" },
            "data": {
                "type": "object",
                "required": [
                    "bundle",
                    "bundle_hash",
                    "signature_present",
                    "signature_verified",
                    "recorded_bundle_hash",
                    "key_id",
                    "schema_version",
                    "entrypoint",
                    "module_count",
                    "content_entry_count"
                ],
                "properties": {
                    "bundle": { "type": "string", "minLength": 1 },
                    "bundle_hash": { "type": "string", "pattern": "^blake3:" },
                    "recorded_bundle_hash": { "type": ["string", "null"] },
                    "signature_present": { "type": "boolean" },
                    "signature_verified": { "type": "boolean" },
                    "key_id": { "type": ["string", "null"] },
                    "schema_version": { "type": "integer", "minimum": 1 },
                    "entrypoint": { "type": "string", "minLength": 1 },
                    "module_count": { "type": "integer", "minimum": 1 },
                    "content_entry_count": { "type": "integer", "minimum": 1 }
                }
            }
        }
    })
}

/// Dispatcher for [`PackCommand::Verify`]: prints a human-readable
/// line or a `JsonEnvelope` and exits non-zero on verification failure.
pub fn run_verify(args: PackVerifyArgs) {
    match verify(&args) {
        Ok(outcome) => {
            if args.json {
                let envelope = PackVerifyJsonOutput(outcome).into_envelope();
                println!("{}", to_string_pretty(&envelope));
            } else {
                println!(
                    "ok {} (bundle_hash {}, signature_verified={})",
                    outcome.bundle.display(),
                    outcome.bundle_hash,
                    outcome.signature_verified
                );
            }
        }
        Err(err) => {
            if args.json {
                let envelope: JsonEnvelope<PackVerifyJsonData> =
                    JsonEnvelope::err(PACK_VERIFY_SCHEMA_VERSION, err.code, err.message);
                println!("{}", to_string_pretty(&envelope));
                process::exit(1);
            }
            command_error(&err.message);
        }
    }
}

/// Programmatic verify entry point used by tests so they can read the
/// envelope structurally instead of parsing stdout.
pub fn verify_to_envelope(args: &PackVerifyArgs) -> JsonEnvelope<PackVerifyJsonData> {
    match verify(args) {
        Ok(outcome) => PackVerifyJsonOutput(outcome).into_envelope(),
        Err(err) => JsonEnvelope::err(PACK_VERIFY_SCHEMA_VERSION, err.code, err.message),
    }
}

/// Verify the bundle at `args.bundle`:
///
/// 1. Read the archive (`tar.zst`) and decode the manifest.
/// 2. Recompute the canonical bundle hash from manifest + contents.
/// 3. If the manifest carries an Ed25519 signature, run the existing
///    [`verify_workflow_bundle_signature`] check; refuse unsigned
///    bundles unless `--allow-unsigned` was passed.
/// 4. Walk each `ModuleEntry` and verify its `source_hash_blake3` /
///    `harnbc_hash_blake3` match the in-archive payload.
///
/// Any mismatch yields a [`PackError`] with a stable structured code
/// suitable for JSON consumers.
pub fn verify(args: &PackVerifyArgs) -> Result<PackVerifyJsonData, PackError> {
    let bytes = std::fs::read(&args.bundle).map_err(|err| {
        PackError::new(
            "verify.read_failed",
            format!("failed to read {}: {err}", args.bundle.display()),
        )
    })?;
    let archive = read_harnpack(&bytes).map_err(|err| {
        PackError::new(
            "verify.archive_failed",
            format!("failed to parse {}: {err}", args.bundle.display()),
        )
    })?;
    let manifest = &archive.manifest;
    let contents = &archive.contents;

    let expected_hash = workflow_bundle_hash(manifest, contents).map_err(|err| {
        PackError::new(
            "verify.hash_failed",
            format!("failed to recompute bundle hash: {err}"),
        )
    })?;

    let trust_policy = args
        .trust_policy
        .as_deref()
        .map(skill_provenance::load_trust_policy)
        .transpose()
        .map_err(|err| PackError::new("verify.trust_policy_failed", err))?;
    let signature_present = manifest.signature.is_some();
    let mut signature_verified = false;
    let mut key_id = None;
    if let Some(signature) = manifest.signature.as_ref() {
        key_id = signature.key_id.clone();
        verify_workflow_bundle_signature(manifest, contents)
            .map_err(|err| PackError::new("verify.signature_failed", err.message))?;
        if args.require_trusted_signer {
            let signer_fingerprint = bundle_signer_fingerprint(signature).map_err(|err| {
                PackError::new(
                    "verify.signature_failed",
                    format!("invalid bundle signer: {err}"),
                )
            })?;
            match skill_provenance::check_trusted_signer(&signer_fingerprint, trust_policy.as_ref())
                .map_err(|err| PackError::new("verify.trust_policy_failed", err))?
            {
                skill_provenance::TrustedSignerStatus::Trusted => {}
                skill_provenance::TrustedSignerStatus::MissingSigner => {
                    return Err(PackError::new(
                        "verify.untrusted_signer",
                        format!(
                            "bundle {} was signed by {}, but that signer is not present in the trusted signer registry",
                            args.bundle.display(),
                            signer_fingerprint
                        ),
                    ));
                }
                skill_provenance::TrustedSignerStatus::UntrustedSigner => {
                    return Err(PackError::new(
                        "verify.untrusted_signer",
                        format!(
                            "bundle {} was signed by {}, which is not in the trust policy's trusted_signers allowlist",
                            args.bundle.display(),
                            signer_fingerprint
                        ),
                    ));
                }
            }
        }
        signature_verified = true;
        key_id.get_or_insert(
            signer_fingerprint_from_public_key(&signature.public_key).map_err(|err| {
                PackError::new(
                    "verify.signature_failed",
                    format!("invalid bundle signer: {err}"),
                )
            })?,
        );
    } else if args.require_trusted_signer {
        return Err(PackError::new(
            "verify.untrusted_signer",
            format!(
                "bundle {} is unsigned and cannot satisfy --require-trusted-signer",
                args.bundle.display()
            ),
        ));
    } else if !args.allow_unsigned {
        return Err(PackError::new(
            "verify.unsigned",
            format!(
                "refusing to verify unsigned bundle {} (re-run with --allow-unsigned)",
                args.bundle.display()
            ),
        ));
    }

    let mut source_map: BTreeMap<PathBuf, &HarnpackEntry> = BTreeMap::new();
    let mut bytecode_map: BTreeMap<PathBuf, &HarnpackEntry> = BTreeMap::new();
    let mut archive_hashes: BTreeMap<PathBuf, String> = BTreeMap::new();
    for entry in contents {
        archive_hashes.insert(entry.path.clone(), blake3_hash(&entry.bytes));
        if let Ok(rel) = entry.path.strip_prefix("sources") {
            source_map.insert(rel.to_path_buf(), entry);
        } else if let Ok(rel) = entry.path.strip_prefix("bytecode") {
            bytecode_map.insert(rel.to_path_buf(), entry);
        }
    }

    for module in &manifest.transitive_modules {
        let source_entry = source_map.get(&module.path).ok_or_else(|| {
            PackError::new(
                "verify.module_missing",
                format!(
                    "manifest lists module {} but archive has no sources/{} entry",
                    module.path.display(),
                    module.path.display()
                ),
            )
        })?;
        let actual_source = blake3_hash(&source_entry.bytes);
        if actual_source != module.source_hash_blake3 {
            return Err(PackError::new(
                "verify.source_mismatch",
                format!(
                    "source hash mismatch for {}: manifest {}, archive {}",
                    module.path.display(),
                    module.source_hash_blake3,
                    actual_source
                ),
            ));
        }
        let chunk_rel = adjacent_with_extension(&module.path, bytecode_cache::CACHE_EXTENSION)
            .ok_or_else(|| {
                PackError::new(
                    "verify.module_invalid_path",
                    format!("module {} has no stem", module.path.display()),
                )
            })?;
        let chunk_entry = bytecode_map.get(&chunk_rel).ok_or_else(|| {
            PackError::new(
                "verify.module_missing",
                format!(
                    "manifest lists bytecode for {} but archive has no bytecode/{} entry",
                    module.path.display(),
                    chunk_rel.display()
                ),
            )
        })?;
        let actual_harnbc = blake3_hash(&chunk_entry.bytes);
        if actual_harnbc != module.harnbc_hash_blake3 {
            return Err(PackError::new(
                "verify.bytecode_mismatch",
                format!(
                    "bytecode hash mismatch for {}: manifest {}, archive {}",
                    module.path.display(),
                    module.harnbc_hash_blake3,
                    actual_harnbc
                ),
            ));
        }
    }

    if args.strict {
        verify_sbom_package_hashes(manifest, &archive_hashes)?;
    }

    // Cross-check the recorded signature hash against the recomputed
    // canonical hash. `verify_workflow_bundle_signature` already does
    // this for signed bundles; for unsigned bundles we report the
    // recomputed hash so callers can compare against external
    // attestations.
    let recorded_bundle_hash = manifest
        .signature
        .as_ref()
        .map(|sig| sig.manifest_hash_blake3.clone());
    if let Some(recorded) = &recorded_bundle_hash {
        if recorded != &expected_hash {
            return Err(PackError::new(
                "verify.recorded_hash_mismatch",
                format!(
                    "recorded signature manifest hash {recorded} does not match recomputed {expected_hash}"
                ),
            ));
        }
    }

    Ok(PackVerifyJsonData {
        bundle: args.bundle.clone(),
        bundle_hash: expected_hash,
        recorded_bundle_hash,
        signature_present,
        signature_verified,
        key_id,
        schema_version: manifest.schema_version,
        entrypoint: manifest.entrypoint.clone(),
        module_count: manifest.transitive_modules.len(),
        content_entry_count: contents.len(),
    })
}

fn verify_sbom_package_hashes(
    manifest: &WorkflowBundle,
    archive_hashes: &BTreeMap<PathBuf, String>,
) -> Result<(), PackError> {
    let module_hashes: BTreeMap<&Path, &str> = manifest
        .transitive_modules
        .iter()
        .map(|module| (module.path.as_path(), module.source_hash_blake3.as_str()))
        .collect();

    for package in &manifest.sbom.packages {
        let Some(expected_hash) = package.package_hash_blake3.as_deref() else {
            continue;
        };

        if let Some(rel) = package.name.strip_prefix("module:") {
            let module_path = Path::new(rel);
            let manifest_hash = module_hashes.get(module_path).ok_or_else(|| {
                PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} does not match any manifest transitive module",
                        package.name
                    ),
                )
            })?;
            if *manifest_hash != expected_hash {
                return Err(PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} recorded hash {} but manifest module {} uses {}",
                        package.name,
                        expected_hash,
                        module_path.display(),
                        manifest_hash
                    ),
                ));
            }
            let source_archive_path = PathBuf::from("sources").join(module_path);
            let archive_hash = archive_hashes.get(&source_archive_path).ok_or_else(|| {
                PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} refers to {}, but archive is missing {}",
                        package.name,
                        module_path.display(),
                        source_archive_path.display()
                    ),
                )
            })?;
            if archive_hash != expected_hash {
                return Err(PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} recorded hash {} but archive {} hashes to {}",
                        package.name,
                        expected_hash,
                        source_archive_path.display(),
                        archive_hash
                    ),
                ));
            }
            continue;
        }

        if let Some(rel) = package.name.strip_prefix("asset:") {
            let asset_archive_path = PathBuf::from("sources").join(rel);
            let archive_hash = archive_hashes.get(&asset_archive_path).ok_or_else(|| {
                PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} refers to {}, but archive is missing {}",
                        package.name,
                        rel,
                        asset_archive_path.display()
                    ),
                )
            })?;
            if archive_hash != expected_hash {
                return Err(PackError::new(
                    "verify.sbom_mismatch",
                    format!(
                        "SBOM package {} recorded hash {} but archive {} hashes to {}",
                        package.name,
                        expected_hash,
                        asset_archive_path.display(),
                        archive_hash
                    ),
                ));
            }
            continue;
        }

        let candidate_path = Path::new(&package.name);
        let Some(archive_hash) = archive_hashes.get(candidate_path) else {
            continue;
        };
        if archive_hash != expected_hash {
            return Err(PackError::new(
                "verify.sbom_mismatch",
                format!(
                    "SBOM package {} recorded hash {} but archive {} hashes to {}",
                    package.name,
                    expected_hash,
                    candidate_path.display(),
                    archive_hash
                ),
            ));
        }
    }

    Ok(())
}

fn bundle_signer_fingerprint(signature: &Ed25519Signature) -> Result<String, String> {
    match signature.key_id.as_deref() {
        Some(key_id) if !key_id.trim().is_empty() => Ok(key_id.to_string()),
        _ => signer_fingerprint_from_public_key(&signature.public_key),
    }
}

fn signer_fingerprint_from_public_key(public_key_hex: &str) -> Result<String, String> {
    let public_key_bytes = decode_hex_32(public_key_hex)?;
    let verifying_key = VerifyingKey::from_bytes(&public_key_bytes).map_err(|error| {
        format!("workflow bundle signature public_key is invalid Ed25519: {error}")
    })?;
    Ok(skill_provenance::fingerprint_for_key(&verifying_key))
}

fn decode_hex_32(raw: &str) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    if trimmed.len() != 64 {
        return Err(format!(
            "workflow bundle signature public_key must be 64 hex characters, found {}",
            trimmed.len()
        ));
    }
    let mut bytes = [0_u8; 32];
    for (idx, slot) in bytes.iter_mut().enumerate() {
        let start = idx * 2;
        let end = start + 2;
        *slot = u8::from_str_radix(&trimmed[start..end], 16).map_err(|error| {
            format!(
                "workflow bundle signature public_key contains invalid hex at byte {idx}: {error}"
            )
        })?;
    }
    Ok(bytes)
}
