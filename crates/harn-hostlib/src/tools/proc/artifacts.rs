use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

use crate::error::HostlibError;

static ARTIFACTS: LazyLock<Mutex<BTreeMap<String, CommandArtifacts>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static LAST_RETENTION_SWEEP: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

const RETENTION_ENV: &str = "HARN_COMMAND_ARTIFACT_RETENTION_SECS";
const DEFAULT_RETENTION: Duration = Duration::from_hours(168);
const SWEEP_INTERVAL: Duration = Duration::from_hours(1);
const SWEEP_MAX_CANDIDATES: usize = 4096;
const SWEEP_MAX_DELETIONS: usize = 256;
const ARTIFACT_PREFIX: &str = "harn-command-cmd_";

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
    let output_sha256 = format!("sha256:{}", hex::encode(Sha256::digest(&combined)));
    let artifacts = CommandArtifacts {
        output_path: artifacts.output_path,
        stdout_path: artifacts.stdout_path,
        stderr_path: artifacts.stderr_path,
        line_count: crate::text::count_lines(&combined),
        byte_count: combined.len() as u64,
        output_sha256,
    };
    register_artifacts(command_id, handle_id, &artifacts);
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

fn register_artifacts(command_id: &str, handle_id: Option<&str>, artifacts: &CommandArtifacts) {
    let mut store = ARTIFACTS.lock().expect("command artifact store poisoned");
    store.insert(command_id.to_string(), artifacts.clone());
    if let Some(handle_id) = handle_id {
        store.insert(handle_id.to_string(), artifacts.clone());
    }
}

fn maybe_sweep_stale_artifacts() {
    let Some(retention) = retention_duration() else {
        return;
    };
    let now = Instant::now();
    {
        let mut last = LAST_RETENTION_SWEEP
            .lock()
            .expect("command artifact retention state poisoned");
        if last
            .map(|last_run| now.duration_since(last_run) < SWEEP_INTERVAL)
            .unwrap_or(false)
        {
            return;
        }
        *last = Some(now);
    }
    sweep_command_artifact_dirs(&std::env::temp_dir(), retention, SystemTime::now());
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

fn sweep_command_artifact_dirs(temp_dir: &Path, retention: Duration, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return;
    };
    let mut candidates = 0;
    let mut deletions = 0;
    for entry in entries.flatten() {
        if candidates >= SWEEP_MAX_CANDIDATES || deletions >= SWEEP_MAX_DELETIONS {
            break;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = parse_command_artifact_dir_name(name) else {
            continue;
        };
        candidates += 1;
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now
            .duration_since(modified)
            .map(|age| age < retention)
            .unwrap_or(true)
        {
            continue;
        }
        if process_is_alive(pid) {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            deletions += 1;
        }
    }
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

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return true;
    }
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let result = unsafe { kill(pid as i32, 0) };
    result == 0
        || std::io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code != 3)
            .unwrap_or(true)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use std::ffi::c_void;

    if pid == 0 {
        return true;
    }

    const ERROR_ACCESS_DENIED: i32 = 5;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    type Handle = *mut c_void;

    extern "system" {
        fn CloseHandle(hObject: Handle) -> i32;
        fn GetExitCodeProcess(hProcess: Handle, lpExitCode: *mut u32) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> Handle;
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code == ERROR_ACCESS_DENIED)
            .unwrap_or(true);
    }

    let mut exit_code = 0;
    let alive =
        unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0 && exit_code == STILL_ACTIVE;
    let _ = unsafe { CloseHandle(handle) };
    alive
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(pid: u32) -> bool {
    // Without a portable liveness probe, be conservative for this safety check.
    let _ = pid;
    true
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
            .find(|pid| !process_is_alive(*pid))
            .expect("test host should have an unused high pid")
    }

    #[test]
    fn command_artifact_sweep_deletes_stale_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let stale = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        set_dir_mtime(&stale, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), now);

        assert!(!stale.exists());
    }

    #[test]
    fn command_artifact_sweep_preserves_recent_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let recent = create_artifact_dir(temp.path(), dead_pid(), 100, 1);
        set_dir_mtime(&recent, now - Duration::from_secs(3));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), now);

        assert!(recent.exists());
    }

    #[test]
    fn command_artifact_sweep_preserves_live_pid_artifact_dirs() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let live = create_artifact_dir(temp.path(), std::process::id(), 100, 1);
        set_dir_mtime(&live, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), now);

        assert!(live.exists());
    }

    #[test]
    fn command_artifact_sweep_preserves_malformed_names() {
        let temp = tempdir().unwrap();
        let now = SystemTime::now();
        let malformed = temp.path().join("harn-command-cmd_123_not-nanos_1");
        std::fs::create_dir(&malformed).unwrap();
        set_dir_mtime(&malformed, now - Duration::from_secs(10));

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), now);

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

        sweep_command_artifact_dirs(temp.path(), Duration::from_secs(5), now);

        assert!(link.exists());
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "keep"
        );
    }
}
