use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use harn_hostlib::{fs_watch::FsWatchCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    FsWatchCapability.register_builtins(&mut registry);
    registry
}

fn str_value(value: impl AsRef<str>) -> VmValue {
    VmValue::string(value)
}

fn list(values: &[&str]) -> VmValue {
    VmValue::List(Arc::new(values.iter().map(str_value).collect()))
}

fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[test]
fn subscribe_returns_handle_and_unsubscribe_removes_it() {
    harn_vm::agent_events::reset_all_sinks();
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join(".gitignore"), "ignored.txt\n").expect("write gitignore");
    let session_id = "fs-watch-integration";

    let registry = registry();
    let subscribe = registry
        .find("hostlib_fs_watch_subscribe")
        .expect("subscribe registered");
    let response = (subscribe.handler)(&[dict([
        ("session_id", str_value(session_id)),
        ("root", str_value(temp.path().to_string_lossy())),
        ("globs", list(&["*.txt"])),
        ("debounce_ms", VmValue::Int(0)),
        ("respect_gitignore", VmValue::Bool(true)),
    ])])
    .expect("subscribe succeeds");
    let subscription_id = response
        .as_dict()
        .and_then(|dict| dict.get("subscription_id"))
        .and_then(|value| match value {
            VmValue::String(value) => Some(value.to_string()),
            _ => None,
        })
        .expect("subscription id");

    let unsubscribe = registry
        .find("hostlib_fs_watch_unsubscribe")
        .expect("unsubscribe registered");
    let removed =
        (unsubscribe.handler)(&[dict([("subscription_id", str_value(&subscription_id))])])
            .expect("unsubscribe succeeds");
    assert_eq!(
        removed
            .as_dict()
            .and_then(|dict| dict.get("removed"))
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            }),
        Some(true)
    );
    harn_vm::agent_events::reset_all_sinks();
}

#[test]
fn unsubscribe_reports_unknown_handles() {
    let registry = registry();
    let unsubscribe = registry
        .find("hostlib_fs_watch_unsubscribe")
        .expect("unsubscribe registered");
    let response = (unsubscribe.handler)(&[dict([("subscription_id", str_value("missing"))])])
        .expect("unsubscribe succeeds for missing handle");
    assert_eq!(
        response
            .as_dict()
            .and_then(|dict| dict.get("removed"))
            .and_then(|value| match value {
                VmValue::Bool(value) => Some(*value),
                _ => None,
            }),
        Some(false)
    );
}

#[test]
fn subscribe_rejects_paths_outside_root() {
    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let registry = registry();
    let subscribe = registry
        .find("hostlib_fs_watch_subscribe")
        .expect("subscribe registered");
    let outside_path = outside.path().to_string_lossy().to_string();

    let err = (subscribe.handler)(&[dict([
        ("session_id", str_value("fs-watch-boundary")),
        ("root", str_value(root.path().to_string_lossy())),
        ("paths", list(&[outside_path.as_str()])),
    ])])
    .expect_err("outside path should be rejected");

    let msg = format!("{err}");
    assert!(msg.contains("outside root"), "{msg}");
}
