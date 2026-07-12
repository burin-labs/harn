#![cfg(unix)]

use std::sync::Arc;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use filetime::FileTime;
use harn_hostlib::process::{install_spawner, MockProcessConfig, MockSpawner};
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;
use tempfile::tempdir;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
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
fn command_creation_sweeps_stale_sibling_and_keeps_fresh_output_readable() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "1");
    std::env::set_var("TMPDIR", temp.path());

    let stale = temp
        .path()
        .join(format!("harn-command-cmd_{}_100_1", unused_high_pid()));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("combined.txt"), "stale").unwrap();
    let old = FileTime::from_unix_time(0, 0);
    filetime::set_file_mtime(&stale, old).unwrap();

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "fresh\n"));

    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo fresh"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    assert!(!stale.exists());

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "fresh\n");
}

#[test]
fn command_creation_pressure_sweeps_completed_siblings_from_current_process() {
    let _env_lock = ENV_LOCK.lock().expect("env lock poisoned");
    let temp = tempdir().unwrap();
    let _tmpdir_guard = TmpdirEnvGuard(std::env::var_os("TMPDIR"));
    let _max_dirs_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_MAX_DIRS", "1");
    let _retention_guard = EnvGuard::set("HARN_COMMAND_ARTIFACT_RETENTION_SECS", "86400");
    std::env::set_var("TMPDIR", temp.path());

    let old_completed = temp
        .path()
        .join(format!("harn-command-cmd_{}_100_1", std::process::id()));
    std::fs::create_dir(&old_completed).unwrap();
    std::fs::write(old_completed.join("combined.txt"), "old").unwrap();
    let old = FileTime::from_system_time(
        std::time::SystemTime::now()
            .checked_sub(Duration::from_mins(1))
            .expect("system clock before epoch"),
    );
    filetime::set_file_mtime(&old_completed, old).unwrap();

    let spawner = Arc::new(MockSpawner::new());
    let _guard = install_spawner(spawner.clone());
    spawner.enqueue(MockProcessConfig::with_stdout(0, "new\n"));

    let mut run_req = dict();
    run_req.insert("argv".into(), vlist_str(&["bash", "-c", "echo new"]));
    let run_resp = require_dict(call("hostlib_tools_run_command", run_req).unwrap());

    assert!(!old_completed.exists());
    let output_path = require_str(&run_resp, "output_path");
    assert!(std::path::Path::new(&output_path).exists());

    let mut read_req = dict();
    read_req.insert(
        "command_id".into(),
        vstr(&require_str(&run_resp, "command_id")),
    );
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "new\n");
}
