//! Lifecycle for the optional standalone agent-hook runtime.
//!
//! The capability marker enrolls a machine. Upgrades never create that marker,
//! so installing Harn cannot silently opt a machine into hook execution. Once
//! enrolled, the updater publishes immutable provenance keyed by the extracted
//! binary digest before atomically replacing the binary. Every crash boundary
//! therefore leaves either the old executable or a new executable whose typed
//! provenance is already durable.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const CAPABILITY: &str = "harn-run-standalone-v1";
const PROVENANCE_SCHEMA_VERSION: u32 = 1;
const PROVENANCE_DIR: &str = "provenance-v1";
const UPGRADE_LOCK: &str = ".upgrade.lock";
const UPGRADE_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HookRuntimeRelease {
    pub version: String,
    pub source_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct HookRuntimeRefreshReport {
    pub status: HookRuntimeRefreshStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum HookRuntimeRefreshStatus {
    NotEnrolled,
    Refreshed,
    SkippedUnverified,
}

impl HookRuntimeRefreshReport {
    pub(super) fn not_enrolled() -> Self {
        Self {
            status: HookRuntimeRefreshStatus::NotEnrolled,
            binary_sha256: None,
            version: None,
            source_revision: None,
        }
    }

    pub(super) fn skipped_unverified() -> Self {
        Self {
            status: HookRuntimeRefreshStatus::SkippedUnverified,
            binary_sha256: None,
            version: None,
            source_revision: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookRuntimeProvenance {
    schema_version: u32,
    capability: String,
    version: String,
    source_revision: String,
    binary_name: String,
    binary_sha256: String,
}

pub(super) fn configured_runtime_path() -> Option<PathBuf> {
    runtime_path_from_environment(
        std::env::var_os("AGENT_SHELL_GUARD_HARN_BIN").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        harn_vm::user_dirs::home_dir().as_deref(),
        cfg!(target_os = "windows"),
    )
}

fn runtime_path_from_environment(
    override_path: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&Path>,
    windows: bool,
) -> Option<PathBuf> {
    let binary_name = if windows { "harn.exe" } else { "harn" };
    non_blank_path(override_path).or_else(|| {
        non_blank_path(xdg_cache_home)
            .map(|root| root.join("harn/hook-bin").join(binary_name))
            .or_else(|| home.map(|root| root.join(".cache/harn/hook-bin").join(binary_name)))
    })
}

fn non_blank_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

pub(super) fn enrolled_runtime_path() -> Result<Option<PathBuf>, String> {
    let Some(runtime_path) = configured_runtime_path() else {
        return Ok(None);
    };
    if marker_enrolls(&marker_path(&runtime_path))? {
        Ok(Some(runtime_path))
    } else {
        Ok(None)
    }
}

fn marker_path(runtime_path: &Path) -> PathBuf {
    let name = runtime_path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("harn");
    runtime_path.with_file_name(format!("{name}.standalone-v1"))
}

fn marker_enrolls(path: &Path) -> Result<bool, String> {
    let marker = match fs::read_to_string(path) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "failed to read hook runtime marker {}: {error}",
                path.display()
            ));
        }
    };
    Ok(matches!(
        marker.as_str(),
        CAPABILITY | "harn-run-standalone-v1\n" | "harn-run-standalone-v1\r\n"
    ))
}

pub(super) fn refresh_runtime_if_enrolled(
    runtime_path: &Path,
    candidate: &Path,
    release: &HookRuntimeRelease,
) -> Result<HookRuntimeRefreshReport, String> {
    if !marker_enrolls(&marker_path(runtime_path))? {
        return Ok(HookRuntimeRefreshReport::not_enrolled());
    }
    refresh(runtime_path, candidate, release)
}

fn refresh(
    runtime_path: &Path,
    candidate: &Path,
    release: &HookRuntimeRelease,
) -> Result<HookRuntimeRefreshReport, String> {
    refresh_with_boundary(runtime_path, candidate, release, || Ok(()))
}

fn refresh_with_boundary(
    runtime_path: &Path,
    candidate: &Path,
    release: &HookRuntimeRelease,
    before_binary_replace: impl FnOnce() -> Result<(), String>,
) -> Result<HookRuntimeRefreshReport, String> {
    validate_release(release)?;
    let parent = runtime_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", runtime_path.display()))?;
    let lock_path = parent.join(UPGRADE_LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("failed to open {}: {error}", lock_path.display()))?;
    harn_flock::lock_with_deadline(
        &lock,
        &lock_path,
        harn_flock::LockMode::Exclusive,
        UPGRADE_LOCK_TIMEOUT,
    )
    .map_err(|error| format!("failed to serialize hook runtime refresh: {error}"))?;

    // Enrollment is mutable user intent. Re-read it under the writer lock so
    // a queued upgrade cannot recreate a runtime after it was unenrolled.
    if !marker_enrolls(&marker_path(runtime_path))? {
        return Ok(HookRuntimeRefreshReport::not_enrolled());
    }

    let digest = super::file_sha256_hex(candidate)?;
    let binary_name = runtime_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("{} has no UTF-8 file name", runtime_path.display()))?;
    let provenance = HookRuntimeProvenance {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        capability: CAPABILITY.to_string(),
        version: release.version.clone(),
        source_revision: release.source_revision.clone(),
        binary_name: binary_name.to_string(),
        binary_sha256: digest.clone(),
    };
    let provenance_path = provenance_path(runtime_path, &digest)?;
    publish_provenance(&provenance_path, &provenance)?;
    before_binary_replace()?;
    super::atomic_replace(candidate, runtime_path)?;

    let installed = read_installed_provenance(runtime_path)?;
    if installed != provenance {
        return Err(format!(
            "hook runtime read-back did not match {}",
            provenance_path.display()
        ));
    }
    Ok(HookRuntimeRefreshReport {
        status: HookRuntimeRefreshStatus::Refreshed,
        binary_sha256: Some(digest),
        version: Some(release.version.clone()),
        source_revision: Some(release.source_revision.clone()),
    })
}

fn validate_release(release: &HookRuntimeRelease) -> Result<(), String> {
    let version = release
        .version
        .strip_prefix('v')
        .unwrap_or(&release.version);
    let version_parts: Vec<_> = version.split('.').collect();
    if version_parts.len() != 3
        || version_parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err(format!(
            "hook runtime release version is not vX.Y.Z: {}",
            release.version
        ));
    }
    if release.source_revision.len() != 40
        || !release
            .source_revision
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return Err("hook runtime source revision must be 40 lowercase hex characters".to_string());
    }
    Ok(())
}

fn provenance_path(runtime_path: &Path, digest: &str) -> Result<PathBuf, String> {
    let parent = runtime_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", runtime_path.display()))?;
    Ok(parent.join(PROVENANCE_DIR).join(format!("{digest}.json")))
}

fn publish_provenance(path: &Path, provenance: &HookRuntimeProvenance) -> Result<(), String> {
    if path.exists() {
        let existing = read_provenance(path)?;
        return if existing == *provenance {
            Ok(())
        } else {
            Err(format!(
                "immutable hook runtime provenance conflicts at {}",
                path.display()
            ))
        };
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(provenance)
        .map_err(|error| format!("failed to encode hook runtime provenance: {error}"))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "failed to stage provenance in {}: {error}",
            parent.display()
        )
    })?;
    staged
        .write_all(&bytes)
        .and_then(|()| staged.write_all(b"\n"))
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    match staged.persist_noclobber(path) {
        Ok(_) => sync_parent_directory(parent),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_provenance(path)?;
            if existing == *provenance {
                Ok(())
            } else {
                Err(format!(
                    "immutable hook runtime provenance conflicts at {}",
                    path.display()
                ))
            }
        }
        Err(error) => Err(format!(
            "failed to publish {}: {}",
            path.display(),
            error.error
        )),
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), String> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync {}: {error}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), String> {
    Ok(())
}

fn read_provenance(path: &Path) -> Result<HookRuntimeProvenance, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn read_installed_provenance(runtime_path: &Path) -> Result<HookRuntimeProvenance, String> {
    let digest = super::file_sha256_hex(runtime_path)?;
    let path = provenance_path(runtime_path, &digest)?;
    let provenance = read_provenance(&path)?;
    if provenance.binary_sha256 != digest {
        return Err(format!(
            "hook runtime provenance digest does not match {}",
            runtime_path.display()
        ));
    }
    Ok(provenance)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Barrier};

    use super::*;

    const SOURCE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SOURCE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn release(version: &str, source_revision: &str) -> HookRuntimeRelease {
        HookRuntimeRelease {
            version: version.to_string(),
            source_revision: source_revision.to_string(),
        }
    }

    fn enrolled_fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("temp dir");
        let runtime = root
            .path()
            .join(if cfg!(windows) { "harn.exe" } else { "harn" });
        fs::write(&runtime, b"old-runtime").expect("old runtime");
        fs::write(marker_path(&runtime), format!("{CAPABILITY}\n")).expect("marker");
        (root, runtime)
    }

    #[test]
    fn path_resolution_matches_the_guard_on_unix_and_windows() {
        let home = Path::new("/users/example");
        assert_eq!(
            runtime_path_from_environment(None, Some(OsStr::new("/cache")), Some(home), false),
            Some(PathBuf::from("/cache/harn/hook-bin/harn"))
        );
        assert_eq!(
            runtime_path_from_environment(None, None, Some(home), false),
            Some(PathBuf::from("/users/example/.cache/harn/hook-bin/harn"))
        );
        assert_eq!(
            runtime_path_from_environment(None, Some(OsStr::new("C:/cache")), None, true),
            Some(PathBuf::from("C:/cache/harn/hook-bin/harn.exe"))
        );
        assert_eq!(
            runtime_path_from_environment(
                Some(OsStr::new("/custom/harn")),
                Some(OsStr::new("/cache")),
                Some(home),
                false,
            ),
            Some(PathBuf::from("/custom/harn"))
        );
    }

    #[test]
    fn absent_or_malformed_marker_never_mutates_the_hook_cache() {
        for marker in [
            None,
            Some("wrong\n"),
            Some("harn-run-standalone-v1\nextra\n"),
        ] {
            let root = tempfile::tempdir().expect("temp dir");
            let runtime = root.path().join("harn");
            let candidate = root.path().join("candidate");
            fs::write(&runtime, b"old-runtime").expect("old runtime");
            fs::write(&candidate, b"new-runtime").expect("candidate");
            if let Some(marker) = marker {
                fs::write(marker_path(&runtime), marker).expect("marker");
            }
            let report =
                refresh_runtime_if_enrolled(&runtime, &candidate, &release("v1.2.3", SOURCE_A))
                    .expect("unenrolled refresh is a no-op");
            assert_eq!(report.status, HookRuntimeRefreshStatus::NotEnrolled);
            assert_eq!(fs::read(&runtime).expect("runtime"), b"old-runtime");
            assert!(!root.path().join(PROVENANCE_DIR).exists());
            assert!(!root.path().join(UPGRADE_LOCK).exists());
        }
    }

    #[test]
    fn enrolled_refresh_publishes_matching_typed_provenance_and_executable() {
        let (_root, runtime) = enrolled_fixture();
        let candidate = runtime.with_file_name("candidate");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        let report =
            refresh_runtime_if_enrolled(&runtime, &candidate, &release("v1.2.3", SOURCE_A))
                .expect("refresh enrolled runtime");
        assert_eq!(report.status, HookRuntimeRefreshStatus::Refreshed);
        assert_eq!(fs::read(&runtime).expect("runtime"), b"new-runtime");
        let provenance = read_installed_provenance(&runtime).expect("read-back provenance");
        assert_eq!(provenance.version, "v1.2.3");
        assert_eq!(provenance.source_revision, SOURCE_A);
        assert_eq!(provenance.capability, CAPABILITY);
        assert_eq!(
            provenance.binary_name,
            runtime.file_name().unwrap().to_string_lossy()
        );
        assert!(marker_enrolls(&marker_path(&runtime)).expect("marker"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&runtime)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
        }
    }

    #[test]
    fn interruption_after_provenance_keeps_the_prior_pair_usable() {
        let (_root, runtime) = enrolled_fixture();
        let candidate = runtime.with_file_name("candidate");
        fs::write(&candidate, b"new-runtime").expect("candidate");
        let error =
            refresh_with_boundary(&runtime, &candidate, &release("v1.2.3", SOURCE_A), || {
                Err("injected interruption".to_string())
            })
            .expect_err("injected boundary must stop refresh");
        assert_eq!(error, "injected interruption");
        assert_eq!(fs::read(&runtime).expect("runtime"), b"old-runtime");
        assert!(marker_enrolls(&marker_path(&runtime)).expect("marker"));
        let candidate_digest = super::super::file_sha256_hex(&candidate).expect("candidate hash");
        let staged = provenance_path(&runtime, &candidate_digest).expect("provenance path");
        assert!(
            staged.is_file(),
            "new immutable provenance is safely unused"
        );
    }

    #[test]
    fn concurrent_refreshes_serialize_and_leave_one_complete_pair() {
        let (_root, runtime) = enrolled_fixture();
        let candidate_a = runtime.with_file_name("candidate-a");
        let candidate_b = runtime.with_file_name("candidate-b");
        fs::write(&candidate_a, b"runtime-a").expect("candidate a");
        fs::write(&candidate_b, b"runtime-b").expect("candidate b");
        let started = Arc::new(Barrier::new(2));
        let (attempted_tx, attempted_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let runtime_a = runtime.clone();
            let started_a = Arc::clone(&started);
            let first = scope.spawn(move || {
                refresh_with_boundary(
                    &runtime_a,
                    &candidate_a,
                    &release("v1.2.3", SOURCE_A),
                    || {
                        started_a.wait();
                        attempted_rx.recv().expect("second attempted refresh");
                        Ok(())
                    },
                )
            });
            let runtime_b = runtime.clone();
            let started_b = Arc::clone(&started);
            let second = scope.spawn(move || {
                started_b.wait();
                attempted_tx.send(()).expect("signal attempted refresh");
                refresh_runtime_if_enrolled(&runtime_b, &candidate_b, &release("v1.2.4", SOURCE_B))
            });
            first.join().expect("first thread").expect("first refresh");
            second
                .join()
                .expect("second thread")
                .expect("second refresh");
        });
        assert_eq!(fs::read(&runtime).expect("runtime"), b"runtime-b");
        let provenance = read_installed_provenance(&runtime).expect("matching provenance");
        assert_eq!(provenance.version, "v1.2.4");
        assert_eq!(provenance.source_revision, SOURCE_B);
        assert!(marker_enrolls(&marker_path(&runtime)).expect("marker"));
    }
}
