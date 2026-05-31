//! End-to-end smoke coverage for the real-process spawn path.
//!
//! `tests/process_tools.rs` exercises the process-tool builtins against
//! a [`MockSpawner`](harn_hostlib::process::MockSpawner) and is the
//! deterministic default. This file keeps a small smoke suite that
//! actually spawns real subprocesses through
//! [`harn_hostlib::process::default_spawner`] so the trait wiring isn't
//! drifting away from real semantics.
//!
//! These tests are wall-clock-dependent (they spawn `bash`, `sleep`,
//! etc.) and therefore live in their own integration target. When the
//! test-suite tiering work in issue #1069 lands, the goal is to tag
//! this target into the slow E2E job so it runs on schedule rather
//! than every push.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: BTreeMap<String, VmValue>) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
    let registry = registry();
    let entry = registry
        .find(builtin)
        .unwrap_or_else(|| panic!("builtin {builtin} not registered"));
    let arg = VmValue::Dict(Arc::new(request));
    (entry.handler)(&[arg])
}

fn dict() -> BTreeMap<String, VmValue> {
    BTreeMap::new()
}

fn vstr(value: &str) -> VmValue {
    VmValue::String(Arc::from(value))
}

fn vlist_str(values: &[&str]) -> VmValue {
    VmValue::List(Arc::new(values.iter().map(|s| vstr(s)).collect()))
}

fn require_dict(value: VmValue) -> BTreeMap<String, VmValue> {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

fn require_int(map: &BTreeMap<String, VmValue>, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(i)) => *i,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

fn require_str(map: &BTreeMap<String, VmValue>, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn require_bool(map: &BTreeMap<String, VmValue>, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(b)) => *b,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

#[test]
fn real_run_command_echoes_stdout_and_reports_exit_zero() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "echo hello"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(require_str(&resp, "stdout").trim(), "hello");
    assert_eq!(require_str(&resp, "status"), "completed");
    assert!(!require_bool(&resp, "timed_out"));
}

#[test]
fn real_run_command_kills_child_when_timeout_elapses() {
    // Smoke: the real `wait_with_timeout` should fire SIGKILL when the
    // child blocks past the deadline. Use a very short sleep so the test
    // doesn't bloat the slow suite — under 250 ms wall-clock total.
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "5"]));
    req.insert("timeout_ms".into(), VmValue::Int(150));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
}
