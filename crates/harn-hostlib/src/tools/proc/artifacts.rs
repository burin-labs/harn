use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::error::HostlibError;

static ARTIFACTS: LazyLock<Mutex<BTreeMap<String, CommandArtifacts>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static ACTIVE_ARTIFACT_LEASES: LazyLock<Mutex<BTreeMap<PathBuf, File>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static LAST_RETENTION_SWEEP: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

const RETENTION_ENV: &str = "HARN_COMMAND_ARTIFACT_RETENTION_SECS";
const MAX_DIRS_ENV: &str = "HARN_COMMAND_ARTIFACT_MAX_DIRS";
const DEFAULT_RETENTION: Duration = Duration::from_hours(168);
const DEFAULT_MAX_DIRS: usize = 512;
const SWEEP_INTERVAL: Duration = Duration::from_hours(1);
const PRESSURE_GRACE: Duration = Duration::from_mins(5);
const ARTIFACT_PREFIX: &str = "harn-command-cmd_";
const ACTIVE_LEASE_FILE: &str = ".active.lock";

#[derive(Clone, Debug)]
struct ArtifactDir {
    path: PathBuf,
    modified: SystemTime,
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

pub(crate) fn persist_artifacts(
    command_id: &str,
    stdout: &[u8],
    stderr: &[u8],
    handle_id: Option<&str>,
) -> Result<CommandArtifacts, HostlibError> {
    maybe_sweep_stale_artifacts();
    let artifacts = planned_artifact_paths(command_id);
    std::fs::create_dir_all(artifacts.output_path.parent().unwrap()).map_err(|e| {
        HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create command artifact dir: {e}"),
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            artifacts.output_path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        );
    }
    mark_artifacts_active(&artifacts)?;
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
    mark_artifacts_inactive(&artifacts);
    let artifacts = persisted?;
    register_artifacts(command_id, handle_id, &artifacts);
    maybe_sweep_stale_artifacts();
    Ok(artifacts)
}

pub(crate) fn register_live_artifacts(
    command_id: &str,
    handle_id: Option<&str>,
) -> Result<CommandArtifacts, HostlibError> {
    maybe_sweep_stale_artifacts();
    let artifacts = planned_artifact_paths(command_id);
    std::fs::create_dir_all(artifacts.output_path.parent().unwrap()).map_err(|e| {
        HostlibError::Backend {
            builtin: "hostlib_tools_run_command",
            message: format!("failed to create command artifact dir: {e}"),
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            artifacts.output_path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        );
    }
    mark_artifacts_active(&artifacts)?;
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
    if let Err(error) = created {
        mark_artifacts_inactive(&artifacts);
        return Err(error);
    }
    register_artifacts(command_id, handle_id, &artifacts);
    maybe_sweep_stale_artifacts();
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

pub(crate) fn resolve_output_path(
    command_id: Option<&str>,
    handle_id: Option<&str>,
    path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = path {
        return Some(PathBuf::from(path));
    }
    let artifacts = ARTIFACTS.lock().expect("command artifact store poisoned");
    command_id
        .and_then(|id| artifacts.get(id))
        .or_else(|| handle_id.and_then(|id| artifacts.get(id)))
        .map(|a| a.output_path.clone())
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
    store.insert(command_id.to_string(), artifacts.clone());
    if let Some(handle_id) = handle_id {
        store.insert(handle_id.to_string(), artifacts.clone());
    }
}

fn artifact_dir(artifacts: &CommandArtifacts) -> Option<PathBuf> {
    artifacts.output_path.parent().map(Path::to_path_buf)
}

fn mark_artifacts_active(artifacts: &CommandArtifacts) -> Result<(), HostlibError> {
    let Some(dir) = artifact_dir(artifacts) else {
        return Ok(());
    };
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
    FileExt::lock_exclusive(&lease).map_err(|error| HostlibError::Backend {
        builtin: "hostlib_tools_run_command",
        message: format!("failed to lock command artifact lease: {error}"),
    })?;
    ACTIVE_ARTIFACT_LEASES
        .lock()
        .expect("active command artifact lease store poisoned")
        .insert(dir, lease);
    Ok(())
}

fn mark_artifacts_inactive(artifacts: &CommandArtifacts) {
    if let Some(dir) = artifact_dir(artifacts) {
        if let Some(lease) = ACTIVE_ARTIFACT_LEASES
            .lock()
            .expect("active command artifact lease store poisoned")
            .remove(&dir)
        {
            let _ = FileExt::unlock(&lease);
        }
        let _ = std::fs::remove_file(dir.join(ACTIVE_LEASE_FILE));
    }
}

fn lookup_artifacts(command_id: Option<&str>, handle_id: Option<&str>) -> Option<CommandArtifacts> {
    let store = ARTIFACTS.lock().expect("command artifact store poisoned");
    command_id
        .and_then(|id| store.get(id))
        .or_else(|| handle_id.and_then(|id| store.get(id)))
        .cloned()
}

fn maybe_sweep_stale_artifacts() {
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
    sweep_command_artifact_dirs(&temp_dir, retention, max_dirs, SystemTime::now());
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

fn sweep_command_artifact_dirs(
    temp_dir: &Path,
    retention: Duration,
    max_dirs: usize,
    now: SystemTime,
) {
    let mut dirs = collect_command_artifact_dirs(temp_dir);
    dirs.sort_by_key(|dir| dir.modified);
    let mut live_count = dirs.len();
    for dir in &dirs {
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
            || now
                .duration_since(dir.modified)
                .map(|age| age < PRESSURE_GRACE)
                .unwrap_or(true)
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
    match FileExt::try_lock_exclusive(&lease) {
        Ok(()) => {
            let _ = FileExt::unlock(&lease);
            false
        }
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => true,
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
        FileExt::lock_exclusive(&lease).unwrap();
        set_dir_mtime(&active, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
        assert!(active.exists());

        FileExt::unlock(&lease).unwrap();
        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), DEFAULT_MAX_DIRS, now);
        assert!(!active.exists());
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
