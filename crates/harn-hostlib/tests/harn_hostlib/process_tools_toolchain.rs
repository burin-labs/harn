//! Focused mock coverage for configured toolchain probes and cache facts.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{MockProcessConfig, ProcessError};
use harn_vm::VmValue;
use tempfile::tempdir;

use super::support::*;

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

    let mut cargo_version = dict_map();
    cargo_version.insert("parser".into(), vm_string("regex"));
    cargo_version.insert("pattern".into(), vm_string(r"cargo\s+([0-9.]+)"));
    cargo_version.insert("group".into(), VmValue::Int(1));

    let mut cargo_env = dict_map();
    cargo_env.insert(
        "CARGO_HOME".into(),
        vm_string(cargo_home.to_string_lossy().as_ref()),
    );

    let mut cargo_probe = dict_map();
    cargo_probe.insert("name".into(), vm_string("cargo"));
    cargo_probe.insert("argv".into(), vlist_str(&["cargo", "--version"]));
    cargo_probe.insert("version".into(), VmValue::dict(cargo_version));
    cargo_probe.insert("env".into(), VmValue::dict(cargo_env));
    cargo_probe.insert("env_mode".into(), vm_string("replace"));
    cargo_probe.insert("cache_env".into(), vlist_str(&["CARGO_HOME"]));

    let mut zig_probe = dict_map();
    zig_probe.insert("name".into(), vm_string("zig-fixture"));
    zig_probe.insert("argv".into(), vlist_str(&["zig", "version"]));
    zig_probe.insert(
        "cwd".into(),
        vm_string(workspace.path().to_string_lossy().as_ref()),
    );
    zig_probe.insert(
        "env".into(),
        VmValue::dict({
            let mut env = dict_map();
            env.insert(
                "ZIG_LOCAL_CACHE_DIR".into(),
                vm_string(zig_cache.to_string_lossy().as_ref()),
            );
            env
        }),
    );
    zig_probe.insert("env_mode".into(), vm_string("replace"));
    zig_probe.insert(
        "cache_env".into(),
        vlist_str(&["ZIG_LOCAL_CACHE_DIR", "UNSET_CACHE"]),
    );
    zig_probe.insert("state_paths".into(), vlist_str(&[".zig-cache"]));

    let mut req = dict_map();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![
            VmValue::dict(cargo_probe),
            VmValue::dict(zig_probe),
        ])),
    );
    let resp = require_dict(call_tool("hostlib_tools_toolchain_facts", req).unwrap());
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

    let mut probe = dict_map();
    probe.insert("name".into(), vm_string("missing-tool"));
    probe.insert("argv".into(), vlist_str(&["missing-tool", "--version"]));

    let mut req = dict_map();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![VmValue::dict(probe)])),
    );
    let resp = require_dict(call_tool("hostlib_tools_toolchain_facts", req).unwrap());
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
