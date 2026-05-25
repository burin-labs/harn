//! Integration tests for session-scoped staged filesystem mode.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harn_hostlib::tools::permissions;
use harn_hostlib::{fs::FsCapability, tools::ToolsCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use tempfile::TempDir;

fn registry() -> BuiltinRegistry {
    permissions::reset();
    permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
    FsCapability.register_builtins(&mut registry);
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::Dict(Rc::new(map))]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn unique_session(name: &str) -> String {
    static NEXT_SESSION: AtomicUsize = AtomicUsize::new(1);
    format!("{name}-{}", NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn staged_write_is_read_through_until_commit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("note.txt");
    let session = unique_session("staged-write");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let write = reg.find("hostlib_tools_write_file").unwrap();
    (write.handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("draft")),
    ]))
    .unwrap();

    assert!(
        !file.exists(),
        "staged writes must not touch the working tree"
    );

    let read = reg.find("hostlib_tools_read_file").unwrap();
    let result = (read.handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_ref() == "draft"));

    let status = (reg.find("hostlib_fs_staged_status").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    assert!(matches!(
        dict_get(&status, "total_bytes_pending"),
        VmValue::Int(5)
    ));

    let commit = (reg.find("hostlib_fs_commit_staged").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    let committed = match dict_get(&commit, "committed_paths") {
        VmValue::List(paths) => paths,
        other => panic!("expected committed path list, got {other:?}"),
    };
    assert_eq!(committed.len(), 1);
    assert_eq!(fs::read_to_string(&file).unwrap(), "draft");
}

#[test]
fn staged_delete_masks_disk_and_discard_restores_view() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("keep.txt");
    fs::write(&file, "disk").unwrap();
    let session = unique_session("staged-delete");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let delete = reg.find("hostlib_tools_delete_file").unwrap();
    let deleted = (delete.handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&deleted, "removed"), VmValue::Bool(true)));
    assert!(
        file.exists(),
        "staged deletes must leave the working tree untouched"
    );

    let read = reg.find("hostlib_tools_read_file").unwrap();
    assert!(
        (read.handler)(&dict_arg(&[
            ("session_id", vm_string(&session)),
            ("path", vm_string(&path_str(&file))),
        ]))
        .is_err(),
        "read-through overlay should mask a staged delete"
    );

    let discard = (reg.find("hostlib_fs_discard_staged").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    let discarded = match dict_get(&discard, "discarded_paths") {
        VmValue::List(paths) => paths,
        other => panic!("expected discarded path list, got {other:?}"),
    };
    assert_eq!(discarded.len(), 1);

    let result = (read.handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_ref() == "disk"));
}

#[test]
fn staged_overlay_uses_current_agent_session_when_args_omit_session_id() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("implicit.txt");
    let session = unique_session("implicit-session");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let _session_guard = harn_vm::agent_sessions::enter_current_session(session);
    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("implicit")),
    ]))
    .unwrap();

    assert!(!file.exists());
    let result = (reg.find("hostlib_tools_read_file").unwrap().handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&file)),
    )]))
    .unwrap();
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_ref() == "implicit"));
}

#[test]
fn staged_write_with_new_parent_is_visible_in_directory_overlay() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("new-parent").join("file.txt");
    let session = unique_session("staged-implicit-parent");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&nested))),
        ("content", vm_string("nested")),
        ("create_parents", VmValue::Bool(true)),
    ]))
    .unwrap();

    let list_root = (reg.find("hostlib_tools_list_directory").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();
    let entries = match dict_get(&list_root, "entries") {
        VmValue::List(entries) => entries,
        other => panic!("expected directory entries, got {other:?}"),
    };
    let parent = entries.iter().find(|entry| {
        matches!(dict_get(entry, "name"), VmValue::String(name) if name.as_ref() == "new-parent")
    });
    assert!(matches!(
        parent.map(|entry| dict_get(entry, "is_dir")),
        Some(VmValue::Bool(true))
    ));

    let list_nested = (reg.find("hostlib_tools_list_directory").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(nested.parent().unwrap()))),
    ]))
    .unwrap();
    let entries = match dict_get(&list_nested, "entries") {
        VmValue::List(entries) => entries,
        other => panic!("expected nested directory entries, got {other:?}"),
    };
    assert!(entries.iter().any(|entry| {
        matches!(dict_get(entry, "name"), VmValue::String(name) if name.as_ref() == "file.txt")
    }));
    assert!(!nested.exists());
}

#[test]
fn staged_delete_of_overlay_parent_drops_child_writes() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("new-parent").join("file.txt");
    let session = unique_session("staged-delete-parent");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&nested))),
        ("content", vm_string("nested")),
        ("create_parents", VmValue::Bool(true)),
    ]))
    .unwrap();

    let deleted = (reg.find("hostlib_tools_delete_file").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&dir.path().join("new-parent")))),
        ("recursive", VmValue::Bool(true)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&deleted, "removed"), VmValue::Bool(true)));

    let status = (reg.find("hostlib_fs_staged_status").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    let pending = match dict_get(&status, "pending_writes") {
        VmValue::List(pending) => pending,
        other => panic!("expected pending list, got {other:?}"),
    };
    assert!(pending.is_empty());
    assert!(!nested.exists());
}

#[test]
fn staged_discard_prunes_unreferenced_body_blobs() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("secret.txt");
    let session = unique_session("staged-prune");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("secret")),
    ]))
    .unwrap();

    let body_dir = dir
        .path()
        .join(".harn")
        .join("state")
        .join("staged")
        .join(&session)
        .join("bodies");
    assert_eq!(fs::read_dir(&body_dir).unwrap().count(), 1);

    (reg.find("hostlib_fs_discard_staged").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();

    assert_eq!(fs::read_dir(&body_dir).unwrap().count(), 0);
}

#[test]
fn staged_directory_overlay_preserves_missing_directory_errors() {
    let dir = TempDir::new().unwrap();
    let session = unique_session("staged-missing-dir");
    let reg = registry();

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let result = (reg.find("hostlib_tools_list_directory").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&dir.path().join("missing")))),
    ]));
    assert!(
        result.is_err(),
        "missing staged directories should not list as empty"
    );
}
