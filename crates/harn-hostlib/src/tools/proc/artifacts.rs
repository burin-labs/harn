use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

use crate::error::HostlibError;

static ARTIFACTS: LazyLock<Mutex<ArtifactRegistry>> =
    LazyLock::new(|| Mutex::new(ArtifactRegistry::default()));
static ACTIVE_ARTIFACT_LEASES: LazyLock<Mutex<BTreeMap<PathBuf, File>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static LAST_RETENTION_SWEEP: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

const RETENTION_ENV: &str = "HARN_COMMAND_ARTIFACT_RETENTION_SECS";
const MAX_DIRS_ENV: &str = "HARN_COMMAND_ARTIFACT_MAX_DIRS";
const DEFAULT_RETENTION: Duration = Duration::from_hours(168);
// Completed registrations are bounded per process, so N live processes may
// retain up to N * this value. The shared sweep removes oldest unleased
// directories under pressure; it never revokes another process's live lease.
const DEFAULT_MAX_DIRS: usize = 512;
const SWEEP_INTERVAL: Duration = Duration::from_hours(1);
const ARTIFACT_PREFIX: &str = "harn-command-cmd_";
const ACTIVE_LEASE_FILE: &str = ".active.lock";
const NAMESPACE_LEASE_PREFIX: &str = ".harn-command-artifacts";
// Command IDs are unique. Contention therefore indicates a stale process or
// an identity collision, not useful work whose duration should be inherited.
const ACTIVE_LEASE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
struct ArtifactDir {
    path: PathBuf,
    modified: SystemTime,
}

#[derive(Default)]
struct ArtifactRegistry {
    by_id: BTreeMap<String, CommandArtifacts>,
    // Completed results remain leased and repeat-readable while registered.
    // Count or age retirement removes every alias before releasing the lease.
    completed: VecDeque<CompletedArtifact>,
}

struct CompletedArtifact {
    path: PathBuf,
    completed_at: SystemTime,
}

#[derive(Clone, Copy)]
enum ArtifactLeaseCleanup {
    NamespaceHeld,
    LeaseOnly,
}

/// Releases a newly acquired active lease unless ownership is transferred to
/// the completed or live artifact registry. Drop deliberately does not take
/// the namespace lock: it is the last-resort cleanup when that lock caused the
/// registration failure in the first place.
struct ActiveArtifactLeaseGuard {
    dir: Option<PathBuf>,
}

impl ActiveArtifactLeaseGuard {
    fn new(artifacts: &CommandArtifacts) -> Self {
        Self {
            dir: artifact_dir(artifacts),
        }
    }

    fn keep_registered(mut self) {
        self.dir = None;
    }
}

impl Drop for ActiveArtifactLeaseGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            release_artifact_lease(&dir);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CommandArtifacts {
    pub(crate) output_path: PathBuf,
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
    pub(crate) line_count: u64,
    pub(crate) byte_count: u64,
    pub(crate) output_sha256: String,
}

pub(crate) struct CommandArtifactRead {
    pub(crate) path: PathBuf,
    pub(crate) offset: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) total_bytes: u64,
}

pub(crate) fn persist_artifacts(
    command_id: &str,
    stdout: &[u8],
    stderr: &[u8],
    handle_id: Option<&str>,
) -> Result<CommandArtifacts, HostlibError> {
    maybe_sweep_stale_artifacts(None);
    let artifacts = planned_artifact_paths(command_id);
    create_and_mark_artifacts_active(&artifacts)?;
    let active_lease = ActiveArtifactLeaseGuard::new(&artifacts);
    let persisted = (|| -> Result<CommandArtifacts, HostlibError> {
        std::fs::write(&artifacts.stdout_path, stdout).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to write stdout artifact: {e}"),
        })?;
        std::fs::write(&artifacts.stderr_path, stderr).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to write stderr artifact: {e}"),
        })?;
        let mut combined = Vec::with_capacity(stdout.len() + stderr.len());
        combined.extend_from_slice(stdout);
        combined.extend_from_slice(stderr);
        std::fs::write(&artifacts.output_path, &combined).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to write combined output artifact: {e}"),
        })?;
        Ok(CommandArtifacts {
            output_path: artifacts.output_path.clone(),
            stdout_path: artifacts.stdout_path.clone(),
            stderr_path: artifacts.stderr_path.clone(),
            line_count: crate::text::count_lines(&combined),
            byte_count: combined.len() as u64,
            output_sha256: format!("sha256:{}", hex::encode(Sha256::digest(&combined))),
        })
    })();
    let artifacts = persisted?;
    register_completed_artifacts_with_guard(command_id, handle_id, &artifacts, active_lease)?;
    let current_dir = artifact_dir(&artifacts);
    maybe_sweep_stale_artifacts(current_dir.as_deref());
    Ok(artifacts)
}

pub(crate) fn register_live_artifacts(
    command_id: &str,
    handle_id: Option<&str>,
) -> Result<CommandArtifacts, HostlibError> {
    maybe_sweep_stale_artifacts(None);
    let artifacts = planned_artifact_paths(command_id);
    create_and_mark_artifacts_active(&artifacts)?;
    let active_lease = ActiveArtifactLeaseGuard::new(&artifacts);
    let created = (|| -> Result<(), HostlibError> {
        std::fs::File::create(&artifacts.stdout_path).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create stdout artifact: {e}"),
        })?;
        std::fs::File::create(&artifacts.stderr_path).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create stderr artifact: {e}"),
        })?;
        std::fs::File::create(&artifacts.output_path).map_err(|e| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create combined output artifact: {e}"),
        })?;
        Ok(())
    })();
    created?;
    register_artifacts(command_id, handle_id, &artifacts);
    active_lease.keep_registered();
    let current_dir = artifact_dir(&artifacts);
    maybe_sweep_stale_artifacts(current_dir.as_deref());
    Ok(artifacts)
}

pub(crate) fn planned_artifact_paths(command_id: &str) -> CommandArtifacts {
    let dir = std::env::temp_dir().join(format!("harn-command-{command_id}"));
    CommandArtifacts {
        output_path: dir.join("combined.txt"),
        stdout_path: dir.join("stdout.txt"),
        stderr_path: dir.join("stderr.txt"),
        line_count: 0,
        byte_count: 0,
        output_sha256: String::new(),
    }
}

/// Build the terminal artifact metadata from captured bytes when persistence
/// is unavailable. The command result still needs a complete, truthful
/// terminal shape so waiters are not left without a result merely because
/// artifact storage had a transient failure.
pub(crate) fn summarize_artifacts(
    command_id: &str,
    stdout: &[u8],
    stderr: &[u8],
    handle_id: Option<&str>,
) -> CommandArtifacts {
    let mut combined = Vec::with_capacity(stdout.len() + stderr.len());
    combined.extend_from_slice(stdout);
    combined.extend_from_slice(stderr);
    let artifacts = CommandArtifacts {
        line_count: crate::text::count_lines(&combined),
        byte_count: combined.len() as u64,
        output_sha256: format!("sha256:{}", hex::encode(Sha256::digest(&combined))),
        ..planned_artifact_paths(command_id)
    };
    register_fallback_artifacts(command_id, handle_id, &artifacts);
    artifacts
}

fn resolve_output_path(command_id: Option<&str>, handle_id: Option<&str>) -> Option<PathBuf> {
    let artifacts = ARTIFACTS.lock().expect("command artifact store poisoned");
    command_id
        .and_then(|id| artifacts.by_id.get(id))
        .or_else(|| handle_id.and_then(|id| artifacts.by_id.get(id)))
        .map(|a| a.output_path.clone())
}

pub(crate) fn read_output(
    command_id: Option<&str>,
    handle_id: Option<&str>,
    path: Option<&Path>,
    offset: u64,
    length: u64,
) -> Result<Option<CommandArtifactRead>, HostlibError> {
    let temp_dir = path
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map_or_else(std::env::temp_dir, Path::to_path_buf);
    with_artifact_namespace_lock(&temp_dir, ACTIVE_LEASE_LOCK_TIMEOUT, || {
        let path = path
            .map(Path::to_path_buf)
            .or_else(|| resolve_output_path(command_id, handle_id));
        let Some(path) = path else {
            return Ok(None);
        };
        let mut file = File::open(&path).map_err(|error| HostlibError::Backend {
            builtin: "hostlib_tools_read_command_output",
            message: format!(
                "failed to open command output '{}': {error}",
                path.display()
            ),
        })?;
        let total_bytes = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| HostlibError::Backend {
                builtin: "hostlib_tools_read_command_output",
                message: format!(
                    "failed to seek command output '{}': {error}",
                    path.display()
                ),
            })?;
        let mut bytes = vec![
            0_u8;
            usize::try_from(length)
                .unwrap_or(usize::MAX)
                .min(1024 * 1024)
        ];
        let bytes_read = file
            .read(&mut bytes)
            .map_err(|error| HostlibError::Backend {
                builtin: "hostlib_tools_read_command_output",
                message: format!(
                    "failed to read command output '{}': {error}",
                    path.display()
                ),
            })?;
        bytes.truncate(bytes_read);
        Ok(Some(CommandArtifactRead {
            path,
            offset,
            bytes,
            total_bytes,
        }))
    })
}

pub(crate) fn live_artifact_snapshot(
    command_id: Option<&str>,
    handle_id: Option<&str>,
) -> Option<CommandArtifacts> {
    let mut artifacts = lookup_artifacts(command_id, handle_id)?;
    artifacts.byte_count = std::fs::metadata(&artifacts.output_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    Some(artifacts)
}

pub(crate) fn live_artifact_tail(
    command_id: Option<&str>,
    handle_id: Option<&str>,
    max_bytes: u64,
) -> Option<String> {
    let artifacts = lookup_artifacts(command_id, handle_id)?;
    let mut file = std::fs::File::open(&artifacts.output_path).ok()?;
    let len = file.metadata().ok()?.len();
    let offset = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(offset)).ok()?;

    let mut bytes = Vec::with_capacity(len.saturating_sub(offset) as usize);
    file.take(max_bytes).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn register_artifacts(command_id: &str, handle_id: Option<&str>, artifacts: &CommandArtifacts) {
    let mut store = ARTIFACTS.lock().expect("command artifact store poisoned");
    register_artifact_aliases(&mut store, command_id, handle_id, artifacts);
}

fn register_artifact_aliases(
    store: &mut ArtifactRegistry,
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
) {
    store
        .by_id
        .insert(command_id.to_string(), artifacts.clone());
    if let Some(handle_id) = handle_id {
        store.by_id.insert(handle_id.to_string(), artifacts.clone());
    }
}

fn register_fallback_artifacts(
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
) {
    let mut store = ARTIFACTS.lock().expect("command artifact store poisoned");
    register_completed_artifacts_in_store(
        &mut store,
        command_id,
        handle_id,
        artifacts,
        max_artifact_dirs(),
        ArtifactLeaseCleanup::LeaseOnly,
    );
}

fn register_completed_artifacts_in_store(
    store: &mut ArtifactRegistry,
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
    max_dirs: usize,
    lease_cleanup: ArtifactLeaseCleanup,
) {
    register_artifact_aliases(store, command_id, handle_id, artifacts);
    let Some(dir) = artifact_dir(artifacts) else {
        return;
    };
    if !store.completed.iter().any(|entry| entry.path == dir) {
        store.completed.push_back(CompletedArtifact {
            path: dir,
            completed_at: SystemTime::now(),
        });
    }
    retire_completed_artifacts(store, max_dirs, None, lease_cleanup);
}

fn register_completed_artifacts_with_guard(
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
    active_lease: ActiveArtifactLeaseGuard,
) -> Result<(), HostlibError> {
    register_completed_artifacts_with_guard_options(
        command_id,
        handle_id,
        artifacts,
        active_lease,
        max_artifact_dirs(),
        ACTIVE_LEASE_LOCK_TIMEOUT,
    )
}

fn register_completed_artifacts_with_guard_options(
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
    active_lease: ActiveArtifactLeaseGuard,
    max_dirs: usize,
    timeout: Duration,
) -> Result<(), HostlibError> {
    register_completed_artifacts_with_options(command_id, handle_id, artifacts, max_dirs, timeout)?;
    active_lease.keep_registered();
    Ok(())
}

fn register_completed_artifacts_with_options(
    command_id: &str,
    handle_id: Option<&str>,
    artifacts: &CommandArtifacts,
    max_dirs: usize,
    timeout: Duration,
) -> Result<(), HostlibError> {
    let Some(dir) = artifact_dir(artifacts) else {
        register_artifacts(command_id, handle_id, artifacts);
        return Ok(());
    };
    let temp_dir = dir.parent().unwrap_or_else(|| Path::new("."));
    with_artifact_namespace_lock(temp_dir, timeout, || {
        let mut store = ARTIFACTS.lock().expect("command artifact store poisoned");
        register_completed_artifacts_in_store(
            &mut store,
            command_id,
            handle_id,
            artifacts,
            max_dirs,
            ArtifactLeaseCleanup::NamespaceHeld,
        );
        Ok(())
    })
}

fn retire_completed_artifacts_under_namespace(
    store: &mut ArtifactRegistry,
    max_dirs: usize,
    expired_before: Option<SystemTime>,
) {
    retire_completed_artifacts(
        store,
        max_dirs,
        expired_before,
        ArtifactLeaseCleanup::NamespaceHeld,
    );
}

fn retire_completed_artifacts(
    store: &mut ArtifactRegistry,
    max_dirs: usize,
    expired_before: Option<SystemTime>,
    lease_cleanup: ArtifactLeaseCleanup,
) {
    loop {
        let over_limit = max_dirs != 0 && store.completed.len() > max_dirs;
        let expired = expired_before
            .zip(store.completed.front())
            .is_some_and(|(cutoff, artifact)| artifact.completed_at <= cutoff);
        if !over_limit && !expired {
            break;
        }
        let Some(retired) = store.completed.pop_front() else {
            break;
        };
        store
            .by_id
            .retain(|_, artifacts| artifact_dir(artifacts).as_ref() != Some(&retired.path));
        match lease_cleanup {
            ArtifactLeaseCleanup::NamespaceHeld => {
                mark_artifact_dir_inactive_under_namespace(&retired.path);
            }
            ArtifactLeaseCleanup::LeaseOnly => {
                // A persistence failure can itself be caused by namespace-lock
                // contention. Bound the process registry immediately and leave
                // the unlocked marker for a later namespace sweep to remove.
                release_artifact_lease(&retired.path);
            }
        }
    }
}

fn artifact_dir(artifacts: &CommandArtifacts) -> Option<PathBuf> {
    artifacts.output_path.parent().map(Path::to_path_buf)
}

fn create_and_mark_artifacts_active(artifacts: &CommandArtifacts) -> Result<(), HostlibError> {
    create_and_mark_artifacts_active_with_timeout(artifacts, ACTIVE_LEASE_LOCK_TIMEOUT)
}

fn create_and_mark_artifacts_active_with_timeout(
    artifacts: &CommandArtifacts,
    timeout: Duration,
) -> Result<(), HostlibError> {
    let Some(dir) = artifact_dir(artifacts) else {
        return Ok(());
    };
    let temp_dir = dir.parent().unwrap_or_else(|| Path::new("."));
    with_artifact_namespace_lock(temp_dir, timeout, || {
        std::fs::create_dir_all(&dir).map_err(|error| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create command artifact dir: {error}"),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        mark_artifacts_active_under_namespace(artifacts, timeout)
    })
}

#[cfg(test)]
fn mark_artifacts_active(artifacts: &CommandArtifacts) -> Result<(), HostlibError> {
    mark_artifacts_active_with_timeout(artifacts, ACTIVE_LEASE_LOCK_TIMEOUT)
}

#[cfg(test)]
fn mark_artifacts_active_with_timeout(
    artifacts: &CommandArtifacts,
    timeout: Duration,
) -> Result<(), HostlibError> {
    let Some(dir) = artifact_dir(artifacts) else {
        return Ok(());
    };
    let temp_dir = dir.parent().unwrap_or_else(|| Path::new("."));
    with_artifact_namespace_lock(temp_dir, timeout, || {
        mark_artifacts_active_under_namespace(artifacts, timeout)
    })
}

fn mark_artifacts_active_under_namespace(
    artifacts: &CommandArtifacts,
    timeout: Duration,
) -> Result<(), HostlibError> {
    let Some(dir) = artifact_dir(artifacts) else {
        return Ok(());
    };
    let mut active_leases = ACTIVE_ARTIFACT_LEASES
        .lock()
        .expect("active command artifact lease store poisoned");
    if active_leases.contains_key(&dir) {
        return Ok(());
    }
    let lease_path = dir.join(ACTIVE_LEASE_FILE);
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lease_path)
        .map_err(|error| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to open command artifact lease: {error}"),
        })?;
    harn_flock::lock_with_deadline(
        &lease,
        &lease_path,
        harn_flock::LockMode::Exclusive,
        timeout,
    )
    .map_err(|error| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to lock command artifact lease: {error}"),
    })?;
    active_leases.insert(dir, lease);
    Ok(())
}

fn mark_artifacts_inactive(artifacts: &CommandArtifacts) {
    if let Some(dir) = artifact_dir(artifacts) {
        let temp_dir = dir.parent().unwrap_or_else(|| Path::new("."));
        let _ = with_artifact_namespace_lock(temp_dir, ACTIVE_LEASE_LOCK_TIMEOUT, || {
            mark_artifact_dir_inactive_under_namespace(&dir);
            Ok(())
        });
    }
}

fn mark_artifact_dir_inactive_under_namespace(dir: &Path) {
    release_artifact_lease(dir);
    let _ = std::fs::remove_file(dir.join(ACTIVE_LEASE_FILE));
}

fn release_artifact_lease(dir: &Path) {
    if let Some(lease) = ACTIVE_ARTIFACT_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(dir)
    {
        let _ = lease.unlock();
    }
}

fn with_artifact_namespace_lock<T>(
    temp_dir: &Path,
    timeout: Duration,
    operation: impl FnOnce() -> Result<T, HostlibError>,
) -> Result<T, HostlibError> {
    let lease_path = artifact_namespace_lease_path(temp_dir);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lease = options
        .open(&lease_path)
        .map_err(|error| HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to open command artifact namespace lease: {error}"),
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if lease
            .metadata()
            .map(|metadata| metadata.uid() != unsafe { libc::geteuid() })
            .unwrap_or(true)
        {
            return Err(HostlibError::Backend {
                builtin: "hostlib_tools_run_command",
                message: "command artifact namespace lease is not owned by the current user"
                    .to_string(),
            });
        }
    }
    harn_flock::lock_with_deadline(
        &lease,
        &lease_path,
        harn_flock::LockMode::Exclusive,
        timeout,
    )
    .map_err(|error| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to lock command artifact namespace: {error}"),
    })?;
    let result = operation();
    let _ = lease.unlock();
    result
}

fn artifact_namespace_lease_path(temp_dir: &Path) -> PathBuf {
    #[cfg(unix)]
    let suffix = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let suffix = "user".to_string();
    temp_dir.join(format!("{NAMESPACE_LEASE_PREFIX}-{suffix}.lock"))
}

fn lookup_artifacts(command_id: Option<&str>, handle_id: Option<&str>) -> Option<CommandArtifacts> {
    let store = ARTIFACTS.lock().expect("command artifact store poisoned");
    command_id
        .and_then(|id| store.by_id.get(id))
        .or_else(|| handle_id.and_then(|id| store.by_id.get(id)))
        .cloned()
}

fn maybe_sweep_stale_artifacts(current_dir: Option<&Path>) {
    let Some(retention) = retention_duration() else {
        return;
    };
    let temp_dir = std::env::temp_dir();
    let max_dirs = max_artifact_dirs();
    let now = Instant::now();
    {
        let mut last = LAST_RETENTION_SWEEP
            .lock()
            .expect("command artifact retention state poisoned");
        if last
            .map(|last_run| now.duration_since(last_run) < SWEEP_INTERVAL)
            .unwrap_or(false)
            && !command_artifact_dir_count_exceeds(&temp_dir, max_dirs)
        {
            return;
        }
        *last = Some(now);
    }
    let _ = with_artifact_namespace_lock(&temp_dir, ACTIVE_LEASE_LOCK_TIMEOUT, || {
        let expired_before = SystemTime::now().checked_sub(retention);
        retire_completed_artifacts_under_namespace(
            &mut ARTIFACTS.lock().expect("command artifact store poisoned"),
            max_dirs,
            expired_before,
        );
        sweep_command_artifact_dirs_except(
            &temp_dir,
            retention,
            max_dirs,
            SystemTime::now(),
            current_dir,
        );
        Ok(())
    });
}

fn retention_duration() -> Option<Duration> {
    let secs = std::env::var(RETENTION_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RETENTION.as_secs());
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

fn max_artifact_dirs() -> usize {
    std::env::var(MAX_DIRS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_DIRS)
}

fn command_artifact_dir_count_exceeds(temp_dir: &Path, max_dirs: usize) -> bool {
    if max_dirs == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return false;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if parse_command_artifact_dir_name(name).is_none() {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() {
            continue;
        }
        count += 1;
        if count > max_dirs {
            return true;
        }
    }
    false
}

fn collect_command_artifact_dirs(temp_dir: &Path) -> Vec<ArtifactDir> {
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if parse_command_artifact_dir_name(name).is_none() {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        dirs.push(ArtifactDir { path, modified });
    }
    dirs
}

#[cfg(test)]
fn sweep_command_artifact_dirs(
    temp_dir: &Path,
    retention: Duration,
    max_dirs: usize,
    now: SystemTime,
) {
    sweep_command_artifact_dirs_except(temp_dir, retention, max_dirs, now, None);
}

fn sweep_command_artifact_dirs_except(
    temp_dir: &Path,
    retention: Duration,
    max_dirs: usize,
    now: SystemTime,
    current_dir: Option<&Path>,
) {
    let mut dirs = collect_command_artifact_dirs(temp_dir);
    dirs.sort_by_key(|dir| dir.modified);
    let mut live_count = dirs.len();
    for dir in &dirs {
        if current_dir == Some(dir.path.as_path()) {
            continue;
        }
        if now
            .duration_since(dir.modified)
            .map(|age| age < retention)
            .unwrap_or(true)
        {
            continue;
        }
        if should_preserve_artifact_dir(dir) {
            continue;
        }
        if remove_artifact_dir(&dir.path) {
            live_count = live_count.saturating_sub(1);
        }
    }
    if max_dirs == 0 || live_count <= max_dirs {
        return;
    }
    for dir in &dirs {
        if live_count <= max_dirs {
            break;
        }
        if !dir.path.exists()
            || current_dir == Some(dir.path.as_path())
            || should_preserve_artifact_dir(dir)
        {
            continue;
        }
        if remove_artifact_dir(&dir.path) {
            live_count = live_count.saturating_sub(1);
        }
    }
}

fn should_preserve_artifact_dir(dir: &ArtifactDir) -> bool {
    if ACTIVE_ARTIFACT_LEASES
        .lock()
        .expect("active command artifact lease store poisoned")
        .contains_key(&dir.path)
    {
        return true;
    }
    let lease_path = dir.path.join(ACTIVE_LEASE_FILE);
    let Ok(metadata) = std::fs::symlink_metadata(&lease_path) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    let Ok(lease) = OpenOptions::new().read(true).write(true).open(lease_path) else {
        return true;
    };
    match lease.try_lock() {
        Ok(()) => {
            let _ = lease.unlock();
            false
        }
        // Contended or unreadable: either way, treat the lease as live.
        Err(_) => true,
    }
}

fn remove_artifact_dir(dir: &Path) -> bool {
    if std::fs::remove_dir_all(dir).is_err() {
        return false;
    }
    ARTIFACTS
        .lock()
        .expect("command artifact store poisoned")
        .by_id
        .retain(|_, artifacts| artifact_dir(artifacts).as_deref() != Some(dir));
    true
}

fn parse_command_artifact_dir_name(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix(ARTIFACT_PREFIX)?;
    let mut parts = suffix.split('_');
    let pid = parts.next()?.parse::<u32>().ok()?;
    parts.next()?.parse::<u128>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use tempfile::tempdir;

    fn artifact_dir(parent: &Path, pid: u32, nanos: u128, counter: u64) -> PathBuf {
        parent.join(format!("harn-command-cmd_{pid}_{nanos}_{counter}"))
    }

    fn create_artifact_dir(parent: &Path, pid: u32, nanos: u128, counter: u64) -> PathBuf {
        let path = artifact_dir(parent, pid, nanos, counter);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("combined.txt"), "output").unwrap();
        path
    }

    fn set_dir_mtime(path: &Path, time: SystemTime) {
        let file_time = FileTime::from_system_time(time);
        filetime::set_file_mtime(path, file_time).unwrap();
    }

    fn artifacts_in(dir: &Path) -> CommandArtifacts {
        CommandArtifacts {
            output_path: dir.join("combined.txt"),
            stdout_path: dir.join("stdout.txt"),
            stderr_path: dir.join("stderr.txt"),
            line_count: 0,
            byte_count: 0,
            output_sha256: String::new(),
        }
    }

    fn dead_pid() -> u32 {
        (900_000..=999_999)
            .find(|pid| {
                crate::process_liveness::process_liveness(*pid)
                    == crate::process_liveness::ProcessLiveness::Dead
            })
            .expect("test host should have an unused high pid")
    }

    #[test]
    fn command_artifact_sweep_deletes_stale_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let stale = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        set_dir_mtime(&stale, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(!stale.exists());
    }

    #[test]
    fn command_artifact_sweep_preserves_recent_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let recent = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        set_dir_mtime(&recent, now - Duration::from_secs(3));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(recent.exists());
    }

    #[test]
    fn command_artifact_sweep_removes_completed_current_process_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let completed = create_artifact_dir(temp.path(), std::process::id(), 100, 1);
        set_dir_mtime(&completed, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(!completed.exists());
    }

    #[test]
    fn command_artifact_sweep_preserves_active_current_process_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let active = create_artifact_dir(temp.path(), std::process::id(), 100, 1);
        let artifacts = CommandArtifacts {
            output_path: active.join("combined.txt"),
            stdout_path: active.join("stdout.txt"),
            stderr_path: active.join("stderr.txt"),
            line_count: 0,
            byte_count: 0,
            output_sha256: String::new(),
        };
        mark_artifacts_active(&artifacts).unwrap();
        set_dir_mtime(&active, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(active.exists());
        mark_artifacts_inactive(&artifacts);
    }

    #[test]
    fn command_artifact_sweep_uses_cross_process_active_lease_not_pid_liveness() {
        let temp = tempdir().unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
        let active = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        let lease_path = active.join(ACTIVE_LEASE_FILE);
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lease_path)
            .unwrap();
        lease.lock().unwrap();
        set_dir_mtime(&active, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
        assert!(active.exists());

        lease.unlock().unwrap();
        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
        assert!(!active.exists());
    }

    #[test]
    fn contended_command_artifact_lease_names_itself() {
        let temp = tempdir().unwrap();
        let active = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        let artifacts = CommandArtifacts {
            output_path: active.join("combined.txt"),
            stdout_path: active.join("stdout.txt"),
            stderr_path: active.join("stderr.txt"),
            line_count: 0,
            byte_count: 0,
            output_sha256: String::new(),
        };
        let lease_path = active.join(ACTIVE_LEASE_FILE);
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lease_path)
            .unwrap();
        holder.lock().unwrap();

        let error = mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap_err();

        assert!(error
            .to_string()
            .contains(&lease_path.display().to_string()));
        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn namespace_admission_precedes_artifact_directory_publication() {
        let temp = tempdir().unwrap();
        let dir = artifact_dir(temp.path(), std::process::id(), 200, 1);
        let artifacts = CommandArtifacts {
            output_path: dir.join("combined.txt"),
            stdout_path: dir.join("stdout.txt"),
            stderr_path: dir.join("stderr.txt"),
            line_count: 0,
            byte_count: 0,
            output_sha256: String::new(),
        };
        let namespace_path = artifact_namespace_lease_path(temp.path());
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&namespace_path)
            .unwrap();
        holder.lock().unwrap();

        let error =
            create_and_mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap_err();

        assert!(error
            .to_string()
            .contains(&namespace_path.display().to_string()));
        assert!(!dir.exists(), "directory became visible before its lease");
    }

    #[test]
    fn registration_failure_releases_active_lease_without_namespace_reacquisition() {
        let temp = tempdir().unwrap();
        let dir = artifact_dir(temp.path(), std::process::id(), 300, 1);
        let artifacts = artifacts_in(&dir);
        create_and_mark_artifacts_active_with_timeout(&artifacts, Duration::ZERO).unwrap();

        let namespace_path = artifact_namespace_lease_path(temp.path());
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&namespace_path)
            .unwrap();
        holder.lock().unwrap();

        let error = register_completed_artifacts_with_guard_options(
            "command-registration-failure",
            Some("handle-registration-failure"),
            &artifacts,
            ActiveArtifactLeaseGuard::new(&artifacts),
            1,
            Duration::ZERO,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains(&namespace_path.display().to_string()));
        assert!(
            !ACTIVE_ARTIFACT_LEASES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&dir),
            "failure guard must release without waiting for the held namespace lock"
        );
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(ACTIVE_LEASE_FILE))
            .unwrap();
        probe.try_lock().unwrap();
        probe.unlock().unwrap();
        drop(probe);
        holder.unlock().unwrap();
        drop(holder);
    }

    #[test]
    fn completed_fifo_evicts_oldest_aliases_and_releases_its_lease() {
        let temp = tempdir().unwrap();
        let first_dir = artifact_dir(temp.path(), std::process::id(), 400, 1);
        let second_dir = artifact_dir(temp.path(), std::process::id(), 400, 2);
        let first = artifacts_in(&first_dir);
        let second = artifacts_in(&second_dir);
        create_and_mark_artifacts_active_with_timeout(&first, Duration::ZERO).unwrap();
        create_and_mark_artifacts_active_with_timeout(&second, Duration::ZERO).unwrap();

        let mut store = ArtifactRegistry::default();
        store.by_id.insert("command-first".into(), first.clone());
        store.by_id.insert("handle-first".into(), first.clone());
        store.by_id.insert("command-second".into(), second.clone());
        store.by_id.insert("handle-second".into(), second.clone());
        store.completed.push_back(CompletedArtifact {
            path: first_dir.clone(),
            completed_at: SystemTime::UNIX_EPOCH,
        });
        store.completed.push_back(CompletedArtifact {
            path: second_dir.clone(),
            completed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        });

        retire_completed_artifacts_under_namespace(&mut store, 1, None);

        assert!(!store.by_id.contains_key("command-first"));
        assert!(!store.by_id.contains_key("handle-first"));
        assert!(store.by_id.contains_key("command-second"));
        assert!(store.by_id.contains_key("handle-second"));
        assert_eq!(store.completed.len(), 1);
        let active = ACTIVE_ARTIFACT_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!active.contains_key(&first_dir));
        assert!(active.contains_key(&second_dir));
        drop(active);
        mark_artifacts_inactive(&second);
    }

    #[test]
    fn fallback_registration_uses_the_same_bounded_alias_fifo() {
        let temp = tempdir().unwrap();
        let first_dir = artifact_dir(temp.path(), std::process::id(), 500, 1);
        let second_dir = artifact_dir(temp.path(), std::process::id(), 500, 2);
        let first = artifacts_in(&first_dir);
        let second = artifacts_in(&second_dir);
        let mut store = ArtifactRegistry::default();

        register_completed_artifacts_in_store(
            &mut store,
            "fallback-command-first",
            Some("fallback-handle-first"),
            &first,
            1,
            ArtifactLeaseCleanup::LeaseOnly,
        );
        register_completed_artifacts_in_store(
            &mut store,
            "fallback-command-second",
            Some("fallback-handle-second"),
            &second,
            1,
            ArtifactLeaseCleanup::LeaseOnly,
        );

        assert!(!store.by_id.contains_key("fallback-command-first"));
        assert!(!store.by_id.contains_key("fallback-handle-first"));
        assert!(store.by_id.contains_key("fallback-command-second"));
        assert!(store.by_id.contains_key("fallback-handle-second"));
        assert_eq!(store.completed.len(), 1);
        assert_eq!(store.completed.front().unwrap().path, second_dir);
    }

    #[test]
    fn command_artifact_sweep_preserves_malformed_names() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let malformed = temp.path().join("harn-command-cmd_123_not-nanos_1");
        std::fs::create_dir(&malformed).unwrap();
        set_dir_mtime(&malformed, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(malformed.exists());
    }

    #[cfg(unix)]
    #[test]
    fn command_artifact_sweep_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "keep").unwrap();
        let link = artifact_dir(temp.path(), dead_pid(), 100, 1);
        symlink(&target, &link).unwrap();

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);

        assert!(link.exists());
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn command_artifact_pressure_sweep_removes_oldest_dead_dirs_over_limit() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let pid = dead_pid();
        let first = create_artifact_dir(temp.path(), pid, 100, 1);
        let second = create_artifact_dir(temp.path(), pid, 200, 1);
        let third = create_artifact_dir(temp.path(), pid, 300, 1);
        set_dir_mtime(&first, now - Duration::from_mins(30));
        set_dir_mtime(&second, now - Duration::from_mins(20));
        set_dir_mtime(&third, now - Duration::from_mins(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_hours(1), 2, now);

        assert!(!first.exists());
        assert!(second.exists());
        assert!(third.exists());
    }
}
