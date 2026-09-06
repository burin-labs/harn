//! Focused mock coverage for configured toolchain probes and cache facts.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, MockProcessConfig, MockSpawner, ProcessError, SpawnerGuard,
};
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;
use tempfile::tempdir;

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
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
    VmValue::List(Arc::new(values.iter().map(|value| vstr(value)).collect()))
}

fn require_dict(value: VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

fn require_int(map: &harn_vm::value::DictMap, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(value)) => *value,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

fn require_str(map: &harn_vm::value::DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(value)) => value.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn require_bool(map: &harn_vm::value::DictMap, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(value)) => *value,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

fn require_nil(map: &harn_vm::value::DictMap, key: &str) {
    assert!(
        matches!(map.get(key), Some(VmValue::Nil)),
        "expected nil at {key}, got {:?}",
        map.get(key)
    );
}

fn require_nested_dict(map: &harn_vm::value::DictMap, key: &str) -> harn_vm::value::DictMap {
    match map.get(key) {
        Some(VmValue::Dict(value)) => (**value).clone(),
        other => panic!("expected dict at {key}, got {other:?}"),
    }
}

fn require_list(map: &harn_vm::value::DictMap, key: &str) -> Vec<VmValue> {
    match map.get(key) {
        Some(VmValue::List(value)) => value.as_ref().clone(),
        other => panic!("expected list at {key}, got {other:?}"),
    }
}

fn as_dict(value: &VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (**map).clone(),
        other => panic!("expected dict value, got {other:?}"),
    }
}

fn install_mock() -> (Arc<MockSpawner>, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let guard = install_spawner(spawner.clone());
    (spawner, guard)
}

#[test]
fn toolchain_facts_runs_config_declared_probes_and_cache_state() {
    let (spawner, guard) = install_mock();
    let _cargo = spawner.enqueue(MockProcessConfig::with_stdout(
        0,
        "cargo 1.90.0 (1159e78c4 2025-09-14)\n",
    ));
    let _zig = spawner.enqueue(MockProcessConfig::with_stdout(0, "0.13.0\n"));

    let workspace = tempdir().unwrap();
    let cargo_home = workspace.path().join(".cargo-home");
    let zig_cache = workspace.path().join(".zig-cache");
    std::fs::create_dir_all(&cargo_home).unwrap();
    std::fs::create_dir_all(&zig_cache).unwrap();

    let mut cargo_version = dict();
    cargo_version.insert("parser".into(), vstr("regex"));
    cargo_version.insert("pattern".into(), vstr(r"cargo\s+([0-9.]+)"));
    cargo_version.insert("group".into(), VmValue::Int(1));

    let mut cargo_env = dict();
    cargo_env.insert(
        "CARGO_HOME".into(),
        vstr(cargo_home.to_string_lossy().as_ref()),
    );

    let mut cargo_probe = dict();
    cargo_probe.insert("name".into(), vstr("cargo"));
    cargo_probe.insert("argv".into(), vlist_str(&["cargo", "--version"]));
    cargo_probe.insert("version".into(), VmValue::dict(cargo_version));
    cargo_probe.insert("env".into(), VmValue::dict(cargo_env));
    cargo_probe.insert("env_mode".into(), vstr("replace"));
    cargo_probe.insert("cache_env".into(), vlist_str(&["CARGO_HOME"]));

    let mut zig_probe = dict();
    zig_probe.insert("name".into(), vstr("zig-fixture"));
    zig_probe.insert("argv".into(), vlist_str(&["zig", "version"]));
    zig_probe.insert(
        "cwd".into(),
        vstr(workspace.path().to_string_lossy().as_ref()),
    );
    zig_probe.insert(
        "env".into(),
        VmValue::dict({
            let mut env = dict();
            env.insert(
                "ZIG_LOCAL_CACHE_DIR".into(),
                vstr(zig_cache.to_string_lossy().as_ref()),
            );
            env
        }),
    );
    zig_probe.insert("env_mode".into(), vstr("replace"));
    zig_probe.insert(
        "cache_env".into(),
        vlist_str(&["ZIG_LOCAL_CACHE_DIR", "UNSET_CACHE"]),
    );
    zig_probe.insert("state_paths".into(), vlist_str(&[".zig-cache"]));

    let mut req = dict();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![
            VmValue::dict(cargo_probe),
            VmValue::dict(zig_probe),
        ])),
    );
    let resp = require_dict(call("hostlib_tools_toolchain_facts", req).unwrap());
    let toolchains = require_list(&resp, "toolchains");
    assert_eq!(toolchains.len(), 2);

    let cargo = as_dict(&toolchains[0]);
    assert_eq!(require_str(&cargo, "name"), "cargo");
    assert_eq!(require_str(&cargo, "status"), "ok");
    assert!(require_bool(&cargo, "available"));
    assert_eq!(require_str(&cargo, "version"), "1.90.0");
    assert_eq!(require_int(&cargo, "exit_code"), 0);
    let cargo_cache = require_nested_dict(&cargo, "cache_env");
    let cargo_home_fact = require_nested_dict(&cargo_cache, "CARGO_HOME");
    assert_eq!(require_str(&cargo_home_fact, "source"), "probe_env");
    assert_eq!(
        require_str(&cargo_home_fact, "value"),
        cargo_home.to_string_lossy().as_ref()
    );

    let zig = as_dict(&toolchains[1]);
    assert_eq!(require_str(&zig, "name"), "zig-fixture");
    assert_eq!(require_str(&zig, "status"), "ok");
    assert_eq!(require_str(&zig, "version"), "0.13.0");
    let zig_cache_facts = require_nested_dict(&zig, "cache_env");
    let zig_cache_fact = require_nested_dict(&zig_cache_facts, "ZIG_LOCAL_CACHE_DIR");
    assert_eq!(require_str(&zig_cache_fact, "source"), "probe_env");
    assert_eq!(
        require_str(&zig_cache_fact, "value"),
        zig_cache.to_string_lossy().as_ref()
    );
    let unset_cache = require_nested_dict(&zig_cache_facts, "UNSET_CACHE");
    assert_eq!(require_str(&unset_cache, "source"), "unset");
    require_nil(&unset_cache, "value");

    let state_paths = require_list(&zig, "state_paths");
    let zig_state = as_dict(&state_paths[0]);
    assert_eq!(require_str(&zig_state, "path"), ".zig-cache");
    assert!(require_bool(&zig_state, "exists"));
    assert_eq!(require_str(&zig_state, "kind"), "directory");

    let captured = spawner.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].program, "cargo");
    assert_eq!(captured[0].args, vec!["--version".to_string()]);
    assert_eq!(captured[1].program, "zig");
    assert_eq!(captured[1].args, vec!["version".to_string()]);
    drop(guard);
}

#[test]
fn toolchain_facts_reports_spawn_failure_without_aborting_batch() {
    let (spawner, guard) = install_mock();
    spawner.enqueue(MockProcessConfig {
        spawn_error: Some(ProcessError::Spawn("program not found".to_string())),
        ..MockProcessConfig::default()
    });

    let mut probe = dict();
    probe.insert("name".into(), vstr("missing-tool"));
    probe.insert("argv".into(), vlist_str(&["missing-tool", "--version"]));

    let mut req = dict();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![VmValue::dict(probe)])),
    );
    let resp = require_dict(call("hostlib_tools_toolchain_facts", req).unwrap());
    let toolchains = require_list(&resp, "toolchains");
    let missing = as_dict(&toolchains[0]);
    assert_eq!(require_str(&missing, "name"), "missing-tool");
    assert_eq!(require_str(&missing, "status"), "spawn_failed");
    assert!(!require_bool(&missing, "available"));
    require_nil(&missing, "version");
    require_nil(&missing, "exit_code");
    assert!(require_str(&missing, "error").contains("program not found"));
    drop(guard);
}
