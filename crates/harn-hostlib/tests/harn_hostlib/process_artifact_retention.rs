#![cfg(unix)]

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};
use std::{process, process::Stdio};

use filetime::FileTime;
use harn_hostlib::process::{install_spawner, MockProcessConfig, MockSpawner};
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;
use tempfile::tempdir;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const FOREIGN_SWEEP_CHILD_ENV: &str = "HARN_TEST_COMMAND_ARTIFACT_FOREIGN_SWEEP_CHILD";

struct ChildGuard(process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    let registry = registry();
    let entry = registry
        .find(builtin)
        .unwrap_or_else(|| panic!("builtin {builtin} not registered"));
    (entry.handler)(&[VmValue::dict(request)])
}

fn dict() -> harn_vm::value::DictMap {
    harn_vm::value::DictMap::new()
}

fn vstr(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vlist_str(values: &[&str]) -> VmValue {
    VmValue::List(Arc::new(values.iter().map(|s| vstr(s)).collect()))
}

fn require_dict(value: VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

fn require_str(map: &harn_vm::value::DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn artifact_namespace(response: &harn_vm::value::DictMap) -> PathBuf {
    Path::new(&require_str(response, "output_path"))
        .parent()
        .and_then(Path::parent)
        .expect("command output must live below its artifact namespace")
        .to_path_buf()
}

fn effective_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

fn artifact_namespace_name() -> String {
    format!("harn-command-artifacts-{}", effective_uid())
}

fn unused_high_pid() -> u32 {
    (900_000..=999_999)
        .find(|pid| !process_is_alive(*pid))
        .expect("test host should have an unused high pid")
}

fn process_is_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    (unsafe { kill(pid as i32, 0) }) == 0
}

struct TmpdirEnvGuard(Option<std::ffi::OsString>);

impl Drop for TmpdirEnvGuard {
    fn drop(&mut self) {
        match self.0.as_ref() {
            Some(value) => std::env::set_var("TMPDIR", value),
            None => std::env::remove_var("TMPDIR"),
        }
    }
}

struct EnvGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn command_creation_sweeps_only_owned_namespace_and_never_ambient_siblings() {
    const AMBIENT_SIBLING_COUNT: usize = 2_048;
    const REPEATED_PERSISTENCE_COUNT: usize = 4;

    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "1");
    std::env::set_var("TMPDIR", temp.path());

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "bootstrap\n"));
    for _ in 0..REPEATED_PERSISTENCE_COUNT {
        spawner.enqueue(MockProcessConfig::with_stdout(0, "fresh\n"));
    }

    let mut bootstrap_req = dict();
    bootstrap_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo bootstrap"]));
    let bootstrap = require_dict(call("hostlib_tools_run_command", bootstrap_req).unwrap());
    let namespace = artifact_namespace(&bootstrap);
    assert_eq!(namespace.parent(), Some(temp.path()));
    assert_ne!(namespace, temp.path());
    assert_eq!(
        std::fs::metadata(&namespace).unwrap().permissions().mode() & 0o777,
        0o700,
        "artifact namespace must be private to the current user"
    );

    let stale = namespace.join(format!("harn-command-cmd_{}_100_1", unused_high_pid()));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("combined.txt"), "stale").unwrap();
    let old = FileTime::from_unix_time(0, 0);
    filetime::set_file_mtime(&stale, old).unwrap();
    let ambient_owner = unused_high_pid();
    for counter in 1..=AMBIENT_SIBLING_COUNT {
        let ambient = temp
            .path()
            .join(format!("harn-command-cmd_{ambient_owner}_200_{counter}"));
        std::fs::create_dir(&ambient).unwrap();
        filetime::set_file_mtime(ambient, old).unwrap();
    }

    let mut run_resp = None;
    for _ in 0..REPEATED_PERSISTENCE_COUNT {
        let mut run_req = dict();
        run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo fresh"]));
        run_resp = Some(require_dict(
            call("hostlib_tools_run_command", run_req).unwrap(),
        ));
    }
    let run_resp = run_resp.expect("persistence loop must run at least once");

    assert!(!stale.exists());
    let ambient_count = std::fs::read_dir(temp.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("harn-command-cmd_{ambient_owner}_200_"))
        })
        .count();
    assert_eq!(
        ambient_count, AMBIENT_SIBLING_COUNT,
        "retention must never enumerate, stat, or delete ambient temp siblings"
    );
    assert_eq!(artifact_namespace(&run_resp), namespace);

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "fresh\n");
}

#[test]
fn command_creation_rejects_a_symlinked_artifact_namespace() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    std::env::set_var("TMPDIR", temp.path());
    let redirect = temp.path().join("redirect");
    std::fs::create_dir(&redirect).unwrap();
    let namespace = temp.path().join(artifact_namespace_name());
    symlink(&redirect, &namespace).unwrap();

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "must-not-run\n"));
    let mut run_req = dict();
    run_req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "echo must-not-run"]),
    );

    let error = call("hostlib_tools_run_command", run_req).unwrap_err();

    assert!(error
        .to_string()
        .contains("artifact namespace must be a real directory"));
    assert_eq!(std::fs::read_dir(&redirect).unwrap().count(), 0);
}

#[test]
fn command_id_read_uses_the_registered_artifact_namespace_not_ambient_tmpdir() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    std::env::set_var("TMPDIR", temp.path());

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "registered\n"));
    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo registered"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    let unavailable_ambient_tmpdir = temp.path().join("does-not-exist");
    std::env::set_var("TMPDIR", &unavailable_ambient_tmpdir);
    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());

    assert_eq!(require_str(&read_resp, "content"), "registered\n");
    assert!(!unavailable_ambient_tmpdir.exists());
}

#[test]
fn explicit_path_cannot_create_an_absent_artifact_namespace_even_with_an_id() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let artifact_path = temp
        .path()
        .join("harn-command-cmd_123_456_1")
        .join("combined.txt");
    let entries_before = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    let mut read_req = dict();
    read_req.insert("command_id".into(), vstr("ignored-command-id"));
    read_req.insert("path".into(), vstr(&artifact_path.to_string_lossy()));

    let error = call("hostlib_tools_read_command_output", read_req).unwrap_err();

    assert!(matches!(
        error,
        HostlibError::InvalidParameter {
            builtin: "hostlib_tools_read_command_output",
            param: "path",
            ..
        }
    ));
    let entries_after = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries_after, entries_before);
}

#[test]
fn explicit_legacy_artifact_path_remains_readable_without_sweeping_its_parent() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let artifact_dir = temp.path().join("harn-command-cmd_123_456_1");
    std::fs::create_dir(&artifact_dir).unwrap();
    let output = artifact_dir.join("combined.txt");
    std::fs::write(&output, "legacy\n").unwrap();
    let namespace_lease = temp
        .path()
        .join(format!(".harn-command-artifacts-{}.lock", effective_uid()));
    std::fs::write(namespace_lease, "").unwrap();

    let mut read_req = dict();
    read_req.insert("path".into(), vstr(&output.to_string_lossy()));
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());

    assert_eq!(require_str(&read_resp, "content"), "legacy\n");
    assert!(artifact_dir.exists());
}

#[test]
fn command_creation_pressure_caps_recent_completed_siblings_and_keeps_new_output_readable() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "1");
    let _retention_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_RETENTION_SECS", "86400");
    std::env::set_var("TMPDIR", temp.path());

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "bootstrap\n"));
    spawner.enqueue(MockProcessConfig::with_stdout(0, "new\n"));

    let mut bootstrap_req = dict();
    bootstrap_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo bootstrap"]));
    let bootstrap = require_dict(call("hostlib_tools_run_command", bootstrap_req).unwrap());
    let namespace = artifact_namespace(&bootstrap);

    let recent_completed = namespace.join(format!("harn-command-cmd_{}_100_1", std::process::id()));
    std::fs::create_dir(&recent_completed).unwrap();
    std::fs::write(recent_completed.join("combined.txt"), "recent").unwrap();
    filetime::set_file_mtime(&recent_completed, FileTime::from_unix_time(0, 0)).unwrap();

    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo new"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    assert!(!recent_completed.exists());
    let output_path = require_str(&run_resp, "output_path");
    assert!(std::path::Path::new(&output_path).exists());
    let artifact_count = std::fs::read_dir(&namespace)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("harn-command-cmd_")
        })
        .count();
    assert_eq!(artifact_count, 1);

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "new\n");
}

#[test]
fn registered_result_survives_a_foreign_pressure_sweep_until_bounded_eviction() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "1");
    let _retention_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_RETENTION_SECS", "86400");
    std::env::set_var("TMPDIR", temp.path());

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "parent\n"));
    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo parent"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    let namespace = artifact_namespace(&run_resp);
    let victim = namespace.join(format!("harn-command-cmd_{}_100_1", unused_high_pid()));
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("combined.txt"), "victim").unwrap();
    filetime::set_file_mtime(&victim, FileTime::from_unix_time(0, 0)).unwrap();

    let child_test = "process_artifact_retention::foreign_pressure_sweep_child";
    let status = process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", child_test, "--nocapture"])
        .env(FOREIGN_SWEEP_CHILD_ENV, "1")
        .status()
        .unwrap();
    assert!(status.success(), "foreign pressure sweep child failed");
    assert!(
        !victim.exists(),
        "foreign child must prove its pressure sweep fired"
    );

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "parent\n");
}

#[test]
fn foreign_pressure_sweep_child() {
    if std::env::var_os(FOREIGN_SWEEP_CHILD_ENV).is_none() {
        return;
    }
    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "child\n"));
    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo child"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());
    assert!(std::path::Path::new(&require_str(&run_resp, "output_path")).exists());
}

#[test]
fn completed_artifacts_from_a_live_foreign_pid_do_not_starve_fresh_output() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "2");
    let _retention_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_RETENTION_SECS", "3153600000");
    std::env::set_var("TMPDIR", temp.path());

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "bootstrap\n"));
    spawner.enqueue(MockProcessConfig::with_stdout(0, "fresh\n"));
    let mut bootstrap_req = dict();
    bootstrap_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo bootstrap"]));
    let bootstrap = require_dict(call("hostlib_tools_run_command", bootstrap_req).unwrap());
    let namespace = artifact_namespace(&bootstrap);

    let mut live_owner = ChildGuard(
        process::Command::new("sh")
            .args(["-c", "read _"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let old = FileTime::from_system_time(SystemTime::UNIX_EPOCH + Duration::from_mins(1));
    for counter in 1..=2 {
        let completed = namespace.join(format!(
            "harn-command-cmd_{}_100_{counter}",
            live_owner.0.id()
        ));
        std::fs::create_dir(&completed).unwrap();
        std::fs::write(completed.join("combined.txt"), "old").unwrap();
        filetime::set_file_mtime(&completed, old).unwrap();
    }

    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo fresh"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    let artifact_count = std::fs::read_dir(&namespace)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("harn-command-cmd_")
        })
        .count();
    assert_eq!(artifact_count, 2);

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "fresh\n");

    drop(live_owner.0.stdin.take());
    live_owner.0.wait().unwrap();
}
