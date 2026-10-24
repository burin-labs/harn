//! Shared helpers for the `harn_hostlib` integration-test modules.
//!
//! Every module in this binary builds builtin registries, packs request
//! dicts, invokes handlers, and unpacks `VmValue` responses. These helpers
//! are the one copy of that plumbing; modules import what they use with
//! `use super::support::*;` (or a narrower list).

use harn_vm::value::VmDictExt as _;
use std::path::Path;
use std::sync::Arc;

use harn_hostlib::ast::AstCapability;
use harn_hostlib::process::{
    install_spawner, MockHandleController, MockProcessConfig, MockSpawner, SpawnerGuard,
};
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::value::DictMap;
use harn_vm::VmValue;

// -------- fixtures --------

/// Private copy of a committed fixture tree. An index rebuild writes
/// `.burin/index/snapshot.bin` under its root, so tests rebuild the copy and
/// the committed fixture stays unchanged.
pub fn private_fixture_copy(source: &Path) -> tempfile::TempDir {
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create fixture copy dir");
        for entry in std::fs::read_dir(from).expect("read fixture dir") {
            let entry = entry.expect("fixture entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("fixture entry type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy fixture file");
            }
        }
    }
    let copy = tempfile::tempdir().expect("fixture copy tempdir");
    copy_tree(source, copy.path());
    copy
}

// -------- registries --------

/// Registry carrying every builtin of each capability in `capabilities`.
pub fn registry_with(capabilities: &[&dyn HostlibCapability]) -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    for capability in capabilities {
        capability.register_builtins(&mut registry);
    }
    registry
}

/// Registry carrying only the `tools` capability.
pub fn tools_registry() -> BuiltinRegistry {
    registry_with(&[&ToolsCapability])
}

/// Registry carrying only the `ast` capability.
pub fn ast_registry() -> BuiltinRegistry {
    registry_with(&[&AstCapability])
}

// -------- request builders --------

pub fn vm_string(s: &str) -> VmValue {
    VmValue::string(s)
}

pub fn vlist_str(values: &[&str]) -> VmValue {
    VmValue::list(values.iter().map(|s| vm_string(s)).collect())
}

/// An empty request map, for tests that build requests key by key.
pub fn dict_map() -> DictMap {
    DictMap::new()
}

/// A dict value from `(key, value)` pairs.
pub fn dict(entries: &[(&str, VmValue)]) -> VmValue {
    let mut map = DictMap::new();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    VmValue::dict(map)
}

/// A single-dict builtin argument list from `(key, value)` pairs.
pub fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    vec![dict(entries)]
}

pub fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// -------- invocation --------

/// Invoke builtin `name` with a single `payload` argument.
pub fn try_call(
    registry: &BuiltinRegistry,
    name: &str,
    payload: VmValue,
) -> Result<VmValue, HostlibError> {
    let entry = registry
        .find(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"));
    (entry.handler)(&[payload])
}

/// [`try_call`], panicking when the builtin fails.
pub fn call(registry: &BuiltinRegistry, name: &str, payload: VmValue) -> VmValue {
    try_call(registry, name, payload).unwrap_or_else(|err| panic!("builtin {name} failed: {err:?}"))
}

/// Invoke a `tools` builtin against a fresh tools registry.
pub fn call_tool(builtin: &str, request: DictMap) -> Result<VmValue, HostlibError> {
    try_call(&tools_registry(), builtin, VmValue::dict(request))
}

// -------- response accessors (value level) --------

pub fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d
            .get(key)
            .unwrap_or_else(|| panic!("missing key `{key}` in {value:?}")),
        other => panic!("not a dict: {other:?}"),
    }
}

/// Owned copy of `value[key]`, panicking when the key is missing.
pub fn dict_field(value: &VmValue, key: &str) -> VmValue {
    dict_get(value, key).clone()
}

pub fn as_string(value: &VmValue) -> &str {
    match value {
        VmValue::String(s) => s.as_str(),
        other => panic!("not a string: {other:?}"),
    }
}

pub fn extract_dict(value: &VmValue) -> Arc<DictMap> {
    match value {
        VmValue::Dict(d) => d.clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

pub fn extract_list(value: &VmValue) -> Arc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    }
}

pub fn extract_int(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

pub fn extract_bool(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

pub fn extract_str(value: &VmValue) -> String {
    as_string(value).to_string()
}

/// Owned copy of a dict value.
pub fn as_dict(value: &VmValue) -> DictMap {
    (*extract_dict(value)).clone()
}

/// Owned copy of a dict response.
pub fn require_dict(value: VmValue) -> DictMap {
    as_dict(&value)
}

// -------- response accessors (map level) --------

pub fn require_int(map: &DictMap, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(i)) => *i,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

pub fn require_str(map: &DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

pub fn require_bool(map: &DictMap, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(b)) => *b,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

pub fn require_nil(map: &DictMap, key: &str) {
    assert!(
        matches!(map.get(key), Some(VmValue::Nil)),
        "expected nil at {key}, got {:?}",
        map.get(key)
    );
}

pub fn require_nested_dict(map: &DictMap, key: &str) -> DictMap {
    match map.get(key) {
        Some(VmValue::Dict(value)) => (**value).clone(),
        other => panic!("expected dict at {key}, got {other:?}"),
    }
}

pub fn require_list(map: &DictMap, key: &str) -> Vec<VmValue> {
    match map.get(key) {
        Some(VmValue::List(value)) => value.as_ref().clone(),
        other => panic!("expected list at {key}, got {other:?}"),
    }
}

// -------- schema --------

/// Validate a `tools.<method>` response against its published schema.
pub fn assert_response_matches_schema(method: &str, response: &VmValue) {
    let schema_body =
        harn_hostlib::schemas::lookup("tools", method, harn_hostlib::schemas::SchemaKind::Response)
            .unwrap_or_else(|| panic!("missing response schema for tools.{method}"));
    let schema_json: serde_json::Value =
        serde_json::from_str(schema_body).expect("response schema must be valid JSON");
    let schema = harn_vm::schema::json_to_vm_value(&schema_json);
    harn_vm::schema::validate_value_against_schema(response, &schema, false)
        .unwrap_or_else(|message| panic!("tools.{method} response schema mismatch: {message}"));
}

// -------- process mocks --------

/// Install a fresh `MockSpawner` for the calling thread. Keep the returned
/// guard alive for the test; it restores the previous spawner on drop.
pub fn install_mock() -> (Arc<MockSpawner>, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let guard = install_spawner(spawner.clone());
    (spawner, guard)
}

/// [`install_mock`] with a single `config` already enqueued.
pub fn install_mock_with(
    config: MockProcessConfig,
) -> (Arc<MockSpawner>, MockHandleController, SpawnerGuard) {
    let (spawner, guard) = install_mock();
    let controller = spawner.enqueue(config);
    (spawner, controller, guard)
}

/// Session id unique to this process and thread.
pub fn unique_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    )
}

pub fn snapshot_binding_fixture() -> DictMap {
    let mut files = dict_map();
    files.insert("src/lib.rs".into(), vm_string("sha256:abc123"));

    let mut binding = dict_map();
    binding.put_int("schema_version", 1);
    binding.insert("case_fingerprint".into(), vm_string("case-123"));
    binding.put("files", VmValue::dict(files));
    binding
}

/// Assert `map.snapshot_binding` echoes [`snapshot_binding_fixture`].
pub fn assert_snapshot_binding(map: &DictMap) {
    let binding = require_nested_dict(map, "snapshot_binding");
    assert_eq!(require_int(&binding, "schema_version"), 1);
    assert_eq!(require_str(&binding, "case_fingerprint"), "case-123");
    let files = require_nested_dict(&binding, "files");
    assert_eq!(require_str(&files, "src/lib.rs"), "sha256:abc123");
}
