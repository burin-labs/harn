//! `harn run <bundle.harnpack>` — verify the embedded OpenTrustGraph
//! signature, replay the archive into the content-addressed pack cache,
//! and execute the bundled entrypoint.
//!
//! See issue #1784 (epic #1779). The verify path reuses the helpers
//! shipped with E6.1/E6.3 (`workflow_bundle.rs`) so signing and
//! verification share the same canonical-hash code path.

use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use harn_vm::bytecode_cache;
use harn_vm::orchestration::{
    read_harnpack, verify_workflow_bundle_signature, workflow_bundle_hash, HarnpackEntry,
    WorkflowBundle, WorkflowBundleError,
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

    check_harn_version_compat(&manifest.harn_version, stderr)?;
    let bundle_hash = workflow_bundle_hash(&manifest, &contents)?;
    let cache_dir = bytecode_cache::packs_cache_dir().join(sanitize_bundle_hash(&bundle_hash));
    let cache_hit = manifest_already_replayed(&cache_dir, &manifest)?;
    if !cache_hit {
        replay_archive(&cache_dir, &manifest, &contents)?;
    }

    let entrypoint_path = cache_dir.join("sources").join(&manifest.entrypoint);
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
    })
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
    contents: &[HarnpackEntry],
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

    for entry in contents {
        let dest = join_safe(&staging_path, &entry.path)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| io_err("harnpack.replay_failed", err, parent))?;
        }
        fs::write(&dest, &entry.bytes)
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
    // whichever loses sees the destination already present and treats
    // it as authoritative (content addressing guarantees equivalence).
    // `TempDir::into_path()` defuses the auto-cleanup so the rename
    // owns the directory.
    let staged = staging.keep();
    match fs::rename(&staged, cache_dir) {
        Ok(()) => Ok(()),
        Err(err) if cache_dir.join("harnpack.json").exists() => {
            let _ = fs::remove_dir_all(&staged);
            // The other writer's tree is now in place — pretend we won.
            let _ = err;
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&staged);
            Err(io_err("harnpack.replay_failed", err, cache_dir))
        }
    }
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
}
