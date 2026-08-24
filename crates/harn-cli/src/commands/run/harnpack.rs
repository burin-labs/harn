//! `harn run <bundle.harnpack>` — verify the embedded OpenTrustGraph
//! signature, replay the archive into the content-addressed pack cache,
//! and execute the bundled entrypoint.
//!
//! See issue #1784 (epic #1779). The verify path reuses the helpers
//! shipped with E6.1/E6.3 (`workflow_bundle.rs`) so signing and
//! verification share the same canonical-hash code path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use harn_vm::bytecode_cache;
use harn_vm::orchestration::{
    read_harnpack, verify_workflow_bundle_signature, workflow_bundle_hash,
    ExecutionArtifactFallback, HarnpackEntry, WorkflowBundle, WorkflowBundleError,
};

/// Zstandard magic prefix. `.harnpack` archives are zstd-compressed tar
/// streams, so the on-disk byte signature is the zstd frame header.
const ZSTD_MAGIC: &[u8; 4] = &[0x28, 0xb5, 0x2f, 0xfd];

/// Options for [`prepare_harnpack`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnpackRunOptions {
    /// Run the pack even when it carries no Ed25519 signature.
    pub allow_unsigned: bool,
    /// Verify-only mode: stop after the cache replay and emit a
    /// `pack_run` event without executing the entrypoint.
    pub dry_run_verify: bool,
}

/// Outcome of [`prepare_harnpack`]. The CLI surface uses this to (a)
/// emit the `pack_run` event before the run starts, (b) decide whether
/// to short-circuit on `--dry-run-verify`, and (c) hand off the unpacked
/// entrypoint path to the existing source-execution code path.
#[derive(Debug)]
pub struct PreparedHarnpack {
    pub bundle_hash: String,
    pub signature_verified: bool,
    pub key_id: Option<String>,
    pub cache_hit: bool,
    pub cache_dir: PathBuf,
    pub entrypoint_path: PathBuf,
    pub manifest: WorkflowBundle,
    pub linked_program: Option<harn_vm::linked_program::LinkedProgramArtifact>,
    pub execution_artifact_state: &'static str,
    pub fallback_reason: Option<String>,
    pub artifact_decode_elapsed: Duration,
}

#[derive(Debug)]
pub struct HarnpackError {
    pub code: &'static str,
    pub message: String,
}

impl HarnpackError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HarnpackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for HarnpackError {}

impl From<WorkflowBundleError> for HarnpackError {
    fn from(error: WorkflowBundleError) -> Self {
        Self::new("harnpack.archive", error.message)
    }
}

/// Detect whether `path` references a `.harnpack` bundle by extension
/// or zstd magic header. The magic-header path keeps detection robust
/// for renamed bundles (`./bundle` without extension) which is the
/// failure mode that bit us when users curl bundles without `-o`.
pub fn looks_like_harnpack(path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) == Some("harnpack") {
        return true;
    }
    match fs::File::open(path) {
        Ok(mut file) => {
            use std::io::Read;
            let mut buf = [0u8; 4];
            file.read_exact(&mut buf).is_ok() && &buf == ZSTD_MAGIC
        }
        Err(_) => false,
    }
}

/// Verify the bundle at `path`, replay it into the content-addressed
/// pack cache, and return the unpacked entrypoint to execute.
///
/// Errors map to user-facing exit-code-1 messages on the CLI; the
/// [`HarnpackError::code`] discriminates failure modes for JSON
/// callers and tests.
pub fn prepare_harnpack<W: Write>(
    path: &Path,
    options: &HarnpackRunOptions,
    stderr: &mut W,
) -> Result<PreparedHarnpack, HarnpackError> {
    let bytes = fs::read(path).map_err(|err| {
        HarnpackError::new(
            "harnpack.read_failed",
            format!("failed to read {}: {err}", path.display()),
        )
    })?;
    let archive = read_harnpack(&bytes)?;
    let manifest = archive.manifest;
    let contents = archive.contents;

    let (signature_verified, key_id) = match manifest.signature.as_ref() {
        Some(signature) => {
            verify_workflow_bundle_signature(&manifest, &contents)?;
            (true, signature.key_id.clone())
        }
        None => {
            if !options.allow_unsigned {
                return Err(HarnpackError::new(
                    "harnpack.unsigned",
                    format!(
                        "refusing to run unsigned bundle {} \
                         (re-run with --allow-unsigned to override)",
                        path.display()
                    ),
                ));
            }
            (false, None)
        }
    };

    // Preserve the public unsafe-path error contract before validating payload
    // identity: the entrypoint is host-synthesized replay state, not an archive
    // member that the shared pack verifier owns.
    let entrypoint_rel = join_safe_nonempty(Path::new(""), &manifest.entrypoint)?;
    crate::commands::pack::verify_runtime_payloads(&manifest, &contents, signature_verified)
        .map_err(|error| HarnpackError::new("harnpack.archive_validation", error.message))?;

    check_harn_version_compat(&manifest.harn_version, stderr)?;
    let decode_started = Instant::now();
    let (linked_program, execution_artifact_state, fallback_reason) =
        prepare_execution_artifact(&manifest, &contents)?;
    let artifact_decode_elapsed = decode_started.elapsed();
    let bundle_hash = workflow_bundle_hash(&manifest, &contents)?;
    // Unpacking a bundle needs somewhere durable to replay into, so unlike the
    // bytecode cache this cannot degrade to "off".
    let Some(packs_root) = bytecode_cache::packs_cache_dir() else {
        return Err(HarnpackError::new(
            "harnpack.no_cache_dir",
            format!(
                "no cache directory resolves for unpacking this bundle; set {} to an absolute path",
                bytecode_cache::CACHE_DIR_ENV
            ),
        ));
    };
    let cache_dir = packs_root.join(sanitize_bundle_hash(&bundle_hash));
    let replay_plan = plan_replay(&contents)?;
    let cache_hit = manifest_already_replayed(&cache_dir, &manifest)?;
    if !cache_hit {
        replay_archive(&cache_dir, &manifest, &replay_plan)?;
    }
    ensure_replay_projection(&cache_dir, &manifest, &replay_plan)?;

    let entrypoint_path = cache_dir.join("sources").join(entrypoint_rel);
    if !entrypoint_path.exists() {
        return Err(HarnpackError::new(
            "harnpack.missing_entrypoint",
            format!(
                "manifest entrypoint {} not present in unpacked bundle at {}",
                manifest.entrypoint.display(),
                entrypoint_path.display()
            ),
        ));
    }

    Ok(PreparedHarnpack {
        bundle_hash,
        signature_verified,
        key_id,
        cache_hit,
        cache_dir,
        entrypoint_path,
        manifest,
        linked_program,
        execution_artifact_state,
        fallback_reason,
        artifact_decode_elapsed,
    })
}

fn prepare_execution_artifact(
    manifest: &WorkflowBundle,
    contents: &[HarnpackEntry],
) -> Result<
    (
        Option<harn_vm::linked_program::LinkedProgramArtifact>,
        &'static str,
        Option<String>,
    ),
    HarnpackError,
> {
    for module in &manifest.transitive_modules {
        let source_path = PathBuf::from("sources").join(&module.path);
        let source = contents
            .iter()
            .find(|entry| entry.path == source_path)
            .ok_or_else(|| {
                HarnpackError::new(
                    "harnpack.source_missing",
                    format!("archive is missing {}", source_path.display()),
                )
            })?;
        let actual = format!("blake3:{}", blake3::hash(&source.bytes).to_hex());
        if actual != module.source_hash_blake3 {
            return Err(HarnpackError::new(
                "harnpack.source_mismatch",
                format!("source hash mismatch for {}", module.path.display()),
            ));
        }
    }
    let Some(descriptor) = manifest.execution_artifact.as_ref() else {
        if manifest.schema_version >= harn_vm::orchestration::WORKFLOW_BUNDLE_SCHEMA_VERSION {
            return Err(HarnpackError::new(
                "harnpack.linked_artifact_missing",
                "schema-v3 bundle is missing its execution_artifact descriptor",
            ));
        }
        return Ok((None, "legacy_v2", None));
    };
    if descriptor.format != "harn.linked_program.v1" {
        return Err(HarnpackError::new(
            "harnpack.linked_artifact_incompatible",
            format!(
                "unsupported execution artifact format {}",
                descriptor.format
            ),
        ));
    }
    let entry = contents
        .iter()
        .find(|entry| entry.path == descriptor.path)
        .ok_or_else(|| {
            HarnpackError::new(
                "harnpack.linked_artifact_missing",
                format!("archive is missing {}", descriptor.path.display()),
            )
        })?;
    let actual_hash = format!("blake3:{}", blake3::hash(&entry.bytes).to_hex());
    if actual_hash != descriptor.hash_blake3 {
        return Err(HarnpackError::new(
            "harnpack.linked_artifact_mismatch",
            format!(
                "linked artifact hash mismatch: manifest {}, archive {}",
                descriptor.hash_blake3, actual_hash
            ),
        ));
    }
    harn_vm::linked_program::verify_graph_binding(
        &descriptor.link_report,
        &descriptor.graph_digest_blake3,
        |path| {
            let source_path = PathBuf::from("sources").join(path);
            contents
                .iter()
                .find(|entry| entry.path == source_path)
                .map(|entry| entry.bytes.clone())
        },
    )
    .map_err(|error| HarnpackError::new("harnpack.linked_graph_mismatch", error.message))?;
    let decoded = harn_vm::linked_program::LinkedProgramArtifact::decode(&entry.bytes);
    let linked = match decoded {
        Ok(linked) => linked,
        Err(error)
            if error.code == "linked_program.incompatible"
                && descriptor.fallback == ExecutionArtifactFallback::ExactSources =>
        {
            return Ok((None, "source_fallback", Some(error.message)));
        }
        Err(error) => return Err(HarnpackError::new(error.code, error.message)),
    };
    if linked.entrypoint != manifest.entrypoint
        || linked.identity.graph_digest_blake3 != descriptor.graph_digest_blake3
        || linked.report != descriptor.link_report
    {
        return Err(HarnpackError::new(
            "harnpack.linked_artifact_mismatch",
            "linked artifact identity, entrypoint, or report disagrees with the manifest",
        ));
    }
    Ok((Some(linked), "linked", None))
}

/// Translate a `blake3:<hex>` digest into a filename-safe directory
/// component. `:` is illegal in some path layers (Windows, `tar`
/// member names), so swap it for `_` while keeping the algorithm
/// prefix for forensic readability.
fn sanitize_bundle_hash(hash: &str) -> String {
    hash.replace(':', "_")
}

/// `harn_version` compatibility check: refuse on a major or minor
/// mismatch, warn on a patch mismatch. The contract is documented on
/// issue #1784.
fn check_harn_version_compat<W: Write>(
    bundle_version: &str,
    stderr: &mut W,
) -> Result<(), HarnpackError> {
    let current_version = env!("CARGO_PKG_VERSION");
    if bundle_version == current_version {
        return Ok(());
    }
    let (Some(bundle), Some(current)) = (
        parse_semver_triplet(bundle_version),
        parse_semver_triplet(current_version),
    ) else {
        let _ = writeln!(
            stderr,
            "warning: harnpack harn_version {bundle_version} is not parseable; running anyway"
        );
        return Ok(());
    };
    if bundle.0 != current.0 || bundle.1 != current.1 {
        return Err(HarnpackError::new(
            "harnpack.version_mismatch",
            format!(
                "harnpack was built for harn {bundle_version}; \
                 this runtime is {current_version} (major/minor mismatch refused)"
            ),
        ));
    }
    let _ = writeln!(
        stderr,
        "warning: harnpack was built for harn {bundle_version}; \
         this runtime is {current_version} (patch mismatch)"
    );
    Ok(())
}

/// Parse the `major.minor.patch` triplet from a version string,
/// ignoring any pre-release or build metadata. Returns `None` when the
/// string can't be parsed as `<u32>.<u32>.<u32>` at the front — callers
/// fall back to a permissive warning so unusual version pins don't
/// strand a working bundle.
fn parse_semver_triplet(input: &str) -> Option<(u32, u32, u32)> {
    let core = input.split_once('-').map(|(head, _)| head).unwrap_or(input);
    let core = core.split_once('+').map(|(head, _)| head).unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Returns true when `cache_dir` already holds a previously-replayed
/// archive whose `harnpack.json` matches `manifest`. Content addressing
/// (`bundle_hash` in the directory name) makes a single positive match
/// sufficient; we still cross-check the manifest payload to defend
/// against partial writes from a prior crash.
fn manifest_already_replayed(
    cache_dir: &Path,
    manifest: &WorkflowBundle,
) -> Result<bool, HarnpackError> {
    let manifest_path = cache_dir.join("harnpack.json");
    let Ok(bytes) = fs::read(&manifest_path) else {
        return Ok(false);
    };
    let cached: WorkflowBundle = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(&cached == manifest)
}

/// Unpack the bundle into a fresh staging directory and then rename
/// into the content-addressed cache slot atomically. The intermediate
/// directory keeps a crash mid-extract from leaving a half-populated
/// `<bundle_hash>/` that future runs would mistake for a cache hit.
fn replay_archive(
    cache_dir: &Path,
    manifest: &WorkflowBundle,
    entries: &[ReplayEntry<'_>],
) -> Result<(), HarnpackError> {
    let parent = cache_dir.parent().ok_or_else(|| {
        HarnpackError::new(
            "harnpack.replay_failed",
            format!("pack cache path has no parent: {}", cache_dir.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|err| io_err("harnpack.replay_failed", err, parent))?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(parent)
        .map_err(|err| io_err("harnpack.replay_failed", err, parent))?;
    let staging_path = staging.path().to_path_buf();

    for replay in entries {
        let dest = join_safe(&staging_path, &replay.destination)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io_err("harnpack.replay_failed", err, parent))?;
        }
        fs::write(&dest, &replay.entry.bytes)
            .map_err(|err| io_err("harnpack.replay_failed", err, &dest))?;
    }

    let manifest_bytes = serde_json::to_vec(manifest).map_err(|err| {
        HarnpackError::new(
            "harnpack.replay_failed",
            format!("failed to encode manifest for cache: {err}"),
        )
    })?;
    let manifest_path = staging_path.join("harnpack.json");
    fs::write(&manifest_path, &manifest_bytes)
        .map_err(|err| io_err("harnpack.replay_failed", err, &manifest_path))?;

    // `rename` is atomic on the same filesystem. Two concurrent runs
    // unpacking the same bundle hash will both attempt the rename;
    // whichever loses sees the destination already present. The idempotent
    // verification step after this function checks the winner byte-for-byte
    // before any replayed path reaches execution.
    // `TempDir::into_path()` defuses the auto-cleanup so the rename
    // owns the directory.
    let staged = staging.keep();
    match fs::rename(&staged, cache_dir) {
        Ok(()) => Ok(()),
        Err(err) if cache_dir.join("harnpack.json").exists() => {
            let _ = fs::remove_dir_all(&staged);
            // The other writer's tree is now in place. It is not trusted until
            // `ensure_replay_projection` validates and repairs it.
            let _ = err;
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&staged);
            Err(io_err("harnpack.replay_failed", err, cache_dir))
        }
    }
}

#[derive(Debug)]
struct ReplayEntry<'a> {
    entry: &'a HarnpackEntry,
    destination: PathBuf,
}

/// Map generated bytecode to the adjacent paths the canonical loaders own,
/// while leaving the archive's authoritative `bytecode/` layout untouched.
/// One archive entry produces one replay file; destination collisions fail
/// before any staging directory is written.
fn plan_replay(contents: &[HarnpackEntry]) -> Result<Vec<ReplayEntry<'_>>, HarnpackError> {
    let source_paths = contents
        .iter()
        .filter_map(|entry| {
            entry
                .path
                .strip_prefix("sources")
                .ok()
                .map(Path::to_path_buf)
        })
        .collect::<BTreeSet<_>>();
    let mut destinations = BTreeMap::<PathBuf, PathBuf>::new();
    let mut plan = Vec::with_capacity(contents.len());

    for entry in contents {
        let destination = projected_artifact_path(&entry.path, &source_paths)
            .unwrap_or_else(|| entry.path.clone());
        if let Some(existing) = destinations.insert(destination.clone(), entry.path.clone()) {
            return Err(HarnpackError::new(
                "harnpack.replay_collision",
                format!(
                    "archive entries {} and {} both replay to {}",
                    existing.display(),
                    entry.path.display(),
                    destination.display()
                ),
            ));
        }
        plan.push(ReplayEntry { entry, destination });
    }
    Ok(plan)
}

fn projected_artifact_path(
    archive_path: &Path,
    source_paths: &BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    let artifact_rel = archive_path.strip_prefix("bytecode").ok()?;
    let extension = artifact_rel.extension()?.to_str()?;
    if extension != bytecode_cache::CACHE_EXTENSION
        && extension != bytecode_cache::MODULE_CACHE_EXTENSION
    {
        return None;
    }
    let mut source_rel = artifact_rel.to_path_buf();
    source_rel.set_extension("harn");
    source_paths
        .contains(&source_rel)
        .then(|| PathBuf::from("sources").join(artifact_rel))
}

/// Make the verified archive bytes authoritative over an existing replay slot.
/// This both upgrades old `bytecode/` layouts and repairs tampered or partial
/// cache hits. Every payload and the synthetic manifest are compared exactly;
/// atomic writes make parallel repairs converge on the same bytes.
fn ensure_replay_projection(
    cache_dir: &Path,
    manifest: &WorkflowBundle,
    entries: &[ReplayEntry<'_>],
) -> Result<(), HarnpackError> {
    let manifest_bytes = serde_json::to_vec(manifest).map_err(|error| {
        HarnpackError::new(
            "harnpack.replay_failed",
            format!("failed to encode manifest for cache: {error}"),
        )
    })?;
    ensure_exact_replay_file(cache_dir, &cache_dir.join("harnpack.json"), &manifest_bytes)?;
    for replay in entries {
        let target = join_safe(cache_dir, &replay.destination)?;
        ensure_exact_replay_file(cache_dir, &target, &replay.entry.bytes)?;
    }
    Ok(())
}

fn ensure_exact_replay_file(
    cache_dir: &Path,
    target: &Path,
    expected: &[u8],
) -> Result<(), HarnpackError> {
    ensure_real_parent_dirs(cache_dir, target)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let actual = fs::read(target)
                .map_err(|error| io_err("harnpack.replay_failed", error, target))?;
            if actual == expected {
                return Ok(());
            }
        }
        Ok(_) => {
            return Err(HarnpackError::new(
                "harnpack.replay_collision",
                format!(
                    "refusing to replace non-file replay target {}",
                    target.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_err("harnpack.replay_failed", error, target)),
    }
    harn_vm::atomic_io::atomic_write(target, expected)
        .map_err(|error| io_err("harnpack.replay_failed", error, target))
}

/// Validate every existing path component without following symlinks. This is
/// intentionally stricter than `create_dir_all`: replay repair must never
/// escape its content-addressed slot through a cached `sources/` symlink.
fn ensure_real_parent_dirs(cache_dir: &Path, target: &Path) -> Result<(), HarnpackError> {
    let relative = target.strip_prefix(cache_dir).map_err(|_| {
        HarnpackError::new(
            "harnpack.unsafe_path",
            format!("replay target escapes cache slot: {}", target.display()),
        )
    })?;
    require_real_directory(cache_dir)?;
    let mut current = cache_dir.to_path_buf();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        let Component::Normal(part) = component else {
            return Err(HarnpackError::new(
                "harnpack.unsafe_path",
                format!("unsafe replay parent: {}", target.display()),
            ));
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(HarnpackError::new(
                    "harnpack.replay_collision",
                    format!(
                        "replay parent is not a real directory: {}",
                        current.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        require_real_directory(&current)?;
                    }
                    Err(error) => {
                        return Err(io_err("harnpack.replay_failed", error, &current));
                    }
                }
            }
            Err(error) => return Err(io_err("harnpack.replay_failed", error, &current)),
        }
    }
    Ok(())
}

fn require_real_directory(path: &Path) -> Result<(), HarnpackError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_err("harnpack.replay_failed", error, path))?;
    if metadata.file_type().is_dir() {
        return Ok(());
    }
    Err(HarnpackError::new(
        "harnpack.replay_collision",
        format!("replay parent is not a real directory: {}", path.display()),
    ))
}

fn io_err(code: &'static str, err: io::Error, path: &Path) -> HarnpackError {
    HarnpackError::new(code, format!("{}: {err}", path.display()))
}

/// Join an archive-relative path onto `base` while refusing anything
/// that would escape via `..` or absolute components. `read_harnpack`
/// already rejects unsafe entries at archive parse time; this is
/// belt-and-braces defense for paths we synthesize on the host side.
fn join_safe(base: &Path, rel: &Path) -> Result<PathBuf, HarnpackError> {
    let mut out = base.to_path_buf();
    for component in rel.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(HarnpackError::new(
                    "harnpack.unsafe_path",
                    format!("refusing to unpack unsafe path: {}", rel.display()),
                ));
            }
        }
    }
    Ok(out)
}

fn join_safe_nonempty(base: &Path, rel: &Path) -> Result<PathBuf, HarnpackError> {
    let out = join_safe(base, rel)?;
    if out == base {
        return Err(HarnpackError::new(
            "harnpack.unsafe_path",
            "refusing to use empty harnpack entrypoint",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_triplet_parses_release_versions() {
        assert_eq!(parse_semver_triplet("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver_triplet("0.10.42"), Some((0, 10, 42)));
        assert_eq!(parse_semver_triplet("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver_triplet("1.2.3+build.4"), Some((1, 2, 3)));
        assert_eq!(parse_semver_triplet("garbage"), None);
        assert_eq!(parse_semver_triplet("1.2"), None);
    }

    #[test]
    fn sanitize_bundle_hash_replaces_colon() {
        assert_eq!(sanitize_bundle_hash("blake3:abc"), "blake3_abc");
        assert_eq!(sanitize_bundle_hash("nohash"), "nohash");
    }

    #[test]
    fn check_harn_version_compat_warns_on_patch_mismatch() {
        let current = env!("CARGO_PKG_VERSION");
        let (major, minor, patch) = parse_semver_triplet(current).expect("current parses");
        let other_patch = format!("{major}.{minor}.{}", patch.wrapping_add(1));
        let mut stderr = String::new();
        check_harn_version_compat(&other_patch, &mut stderr).expect("patch mismatch warns");
        assert!(stderr.contains("patch mismatch"), "stderr was {stderr}");
    }

    #[test]
    fn check_harn_version_compat_refuses_on_minor_mismatch() {
        let current = env!("CARGO_PKG_VERSION");
        let (major, minor, _patch) = parse_semver_triplet(current).expect("current parses");
        let other_minor = format!("{major}.{}.0", minor.wrapping_add(1));
        let mut stderr = String::new();
        let err = check_harn_version_compat(&other_minor, &mut stderr)
            .expect_err("minor mismatch must refuse");
        assert_eq!(err.code, "harnpack.version_mismatch");
    }

    #[test]
    fn check_harn_version_compat_is_lenient_with_unparseable_bundle_version() {
        let mut stderr = String::new();
        check_harn_version_compat("not-a-version", &mut stderr).expect("permissive on parse fail");
        assert!(stderr.contains("not parseable"));
    }

    #[test]
    fn join_safe_refuses_traversal() {
        let base = PathBuf::from("/tmp/cache");
        assert!(join_safe(&base, Path::new("../escape")).is_err());
        assert!(join_safe(&base, Path::new("/abs/path")).is_err());
        assert_eq!(
            join_safe(&base, Path::new("sources/hello.harn")).unwrap(),
            base.join("sources").join("hello.harn"),
        );
    }

    #[test]
    fn fresh_replay_keeps_one_projected_copy_of_generated_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir = temp.path().join("slot");
        let contents = vec![
            HarnpackEntry::new("sources/hello.harn", b"fn main() {}\n"),
            HarnpackEntry::new("bytecode/hello.harnbc", b"entry-bytecode"),
            HarnpackEntry::new("bytecode/hello.harnmod", b"module-bytecode"),
        ];
        let plan = plan_replay(&contents).expect("plan replay");

        replay_archive(&cache_dir, &WorkflowBundle::default(), &plan).expect("replay");

        assert_eq!(
            fs::read(cache_dir.join("sources/hello.harnbc")).unwrap(),
            b"entry-bytecode"
        );
        assert_eq!(
            fs::read(cache_dir.join("sources/hello.harnmod")).unwrap(),
            b"module-bytecode"
        );
        assert!(
            !cache_dir.join("bytecode").exists(),
            "a fresh replay must project rather than duplicate generated artifacts"
        );
    }

    #[test]
    fn matching_old_layout_cache_gets_missing_adjacent_projection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir = temp.path().join("slot");
        fs::create_dir_all(cache_dir.join("bytecode")).unwrap();
        fs::create_dir_all(cache_dir.join("sources")).unwrap();
        fs::write(cache_dir.join("sources/hello.harn"), "tampered source\n").unwrap();
        fs::write(cache_dir.join("harnpack.json"), b"{}").unwrap();
        fs::write(
            cache_dir.join("bytecode/hello.harnbc"),
            b"old-layout-bytecode",
        )
        .unwrap();
        let contents = vec![
            HarnpackEntry::new("sources/hello.harn", b"fn main() {}\n"),
            HarnpackEntry::new("bytecode/hello.harnbc", b"old-layout-bytecode"),
        ];
        let plan = plan_replay(&contents).expect("plan replay");
        let manifest = WorkflowBundle::default();

        ensure_replay_projection(&cache_dir, &manifest, &plan).expect("upgrade old cache");
        ensure_replay_projection(&cache_dir, &manifest, &plan).expect("idempotent retry");

        assert_eq!(
            fs::read(cache_dir.join("sources/hello.harnbc")).unwrap(),
            b"old-layout-bytecode"
        );
        assert_eq!(
            fs::read(cache_dir.join("sources/hello.harn")).unwrap(),
            b"fn main() {}\n",
            "verified archive source repairs a tampered cache hit"
        );
        assert_eq!(
            fs::read(cache_dir.join("harnpack.json")).unwrap(),
            serde_json::to_vec(&manifest).unwrap(),
            "synthetic manifest is repaired to its canonical replay bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replay_repair_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir = temp.path().join("slot");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, cache_dir.join("sources")).unwrap();
        let contents = vec![HarnpackEntry::new("sources/hello.harn", b"fn main() {}\n")];
        let plan = plan_replay(&contents).expect("plan replay");

        let error = ensure_replay_projection(&cache_dir, &WorkflowBundle::default(), &plan)
            .expect_err("symlinked parent must fail closed");
        assert_eq!(error.code, "harnpack.replay_collision");
        assert!(
            !outside.join("hello.harn").exists(),
            "repair must not follow the cached parent symlink"
        );
    }

    #[test]
    fn projected_artifact_collision_fails_before_replay() {
        let contents = vec![
            HarnpackEntry::new("sources/hello.harn", b"fn main() {}\n"),
            HarnpackEntry::new("sources/hello.harnbc", b"user asset"),
            HarnpackEntry::new("bytecode/hello.harnbc", b"generated artifact"),
        ];
        let error = plan_replay(&contents).expect_err("collision must fail closed");
        assert_eq!(error.code, "harnpack.replay_collision");
    }

    #[test]
    fn incompatible_linked_program_fails_closed_or_reports_explicit_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let entry_path = temp.path().join("entry.harn");
        let source = "fn main(harness: Harness) { harness.stdio.println(\"linked\") }\n";
        fs::write(&entry_path, source).expect("entry source");
        let linked =
            harn_vm::linked_program::link_program(&entry_path, temp.path()).expect("link program");
        let mut bytes = linked.encode().expect("encode linked program");
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        let descriptor = harn_vm::orchestration::ExecutionArtifact {
            format: "harn.linked_program.v1".to_string(),
            path: PathBuf::from(harn_vm::linked_program::LINKED_PROGRAM_ARCHIVE_PATH),
            hash_blake3: hash,
            graph_digest_blake3: linked.identity.graph_digest_blake3.clone(),
            fallback: ExecutionArtifactFallback::Deny,
            link_report: linked.report,
        };
        let mut manifest = WorkflowBundle {
            entrypoint: PathBuf::from("entry.harn"),
            execution_artifact: Some(descriptor),
            transitive_modules: vec![harn_vm::orchestration::ModuleEntry {
                path: PathBuf::from("entry.harn"),
                source_hash_blake3: format!("blake3:{}", blake3::hash(source.as_bytes()).to_hex()),
                harnbc_hash_blake3: String::new(),
            }],
            ..WorkflowBundle::default()
        };
        let contents = vec![
            HarnpackEntry::new("sources/entry.harn", source.as_bytes()),
            HarnpackEntry::new(harn_vm::linked_program::LINKED_PROGRAM_ARCHIVE_PATH, bytes),
        ];

        let error = prepare_execution_artifact(&manifest, &contents)
            .expect_err("default policy must fail closed");
        assert_eq!(error.code, "linked_program.incompatible");

        manifest.execution_artifact.as_mut().unwrap().fallback =
            ExecutionArtifactFallback::ExactSources;
        let (artifact, state, reason) = prepare_execution_artifact(&manifest, &contents)
            .expect("signed policy explicitly allows exact sources");
        assert!(artifact.is_none());
        assert_eq!(state, "source_fallback");
        assert!(reason.is_some_and(|reason| reason.contains("schema 2")));
    }

    #[test]
    fn prepare_harnpack_rejects_absolute_manifest_entrypoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let external = temp.path().join("outside.harn");
        fs::write(&external, "fn main() {}\n").expect("external source");

        let mut bundle = WorkflowBundle {
            entrypoint: external,
            ..WorkflowBundle::default()
        };
        bundle.harn_version = env!("CARGO_PKG_VERSION").to_string();
        let bytes = harn_vm::orchestration::build_harnpack(
            &bundle,
            &[HarnpackEntry::new("sources/inside.harn", b"fn main() {}\n")],
        )
        .expect("build pack");
        let pack_path = temp.path().join("unsafe.harnpack");
        fs::write(&pack_path, bytes).expect("pack file");

        let mut stderr = String::new();
        let err = prepare_harnpack(
            &pack_path,
            &HarnpackRunOptions {
                allow_unsigned: true,
                dry_run_verify: false,
            },
            &mut stderr,
        )
        .expect_err("absolute entrypoint must be rejected");
        assert_eq!(err.code, "harnpack.unsafe_path");
    }

    #[test]
    fn prepare_harnpack_rejects_traversing_manifest_entrypoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut bundle = WorkflowBundle {
            entrypoint: PathBuf::from("../outside.harn"),
            ..WorkflowBundle::default()
        };
        bundle.harn_version = env!("CARGO_PKG_VERSION").to_string();
        let bytes = harn_vm::orchestration::build_harnpack(
            &bundle,
            &[HarnpackEntry::new("outside.harn", b"fn main() {}\n")],
        )
        .expect("build pack");
        let pack_path = temp.path().join("traversal.harnpack");
        fs::write(&pack_path, bytes).expect("pack file");

        let mut stderr = String::new();
        let err = prepare_harnpack(
            &pack_path,
            &HarnpackRunOptions {
                allow_unsigned: true,
                dry_run_verify: false,
            },
            &mut stderr,
        )
        .expect_err("traversing entrypoint must be rejected");
        assert_eq!(err.code, "harnpack.unsafe_path");
    }
}
