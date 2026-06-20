//! Integration tests for session-scoped staged filesystem mode.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use harn_hostlib::fs::{self as host_fs, FsCapability, FsMode};
use harn_hostlib::tools::permissions;
use harn_hostlib::{tools::ToolsCapability, BuiltinRegistry, HostlibCapability};
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
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
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
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "draft"));

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
fn remove_session_state_deletes_transient_overlay_artifacts() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("transient.txt");
    let session = unique_session("transient-cleanup");
    let reg = registry();

    host_fs::set_mode(&session, FsMode::Staged, Some(dir.path())).unwrap();
    let write = reg.find("hostlib_tools_write_file").unwrap();
    (write.handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("preview")),
    ]))
    .unwrap();

    let session_dir = dir
        .path()
        .join(".harn")
        .join("state")
        .join("staged")
        .join(&session);
    assert!(
        session_dir.join("manifest.json").exists(),
        "staged mode should persist preview metadata before cleanup"
    );

    host_fs::discard_staged(&session, &[]).unwrap();
    host_fs::remove_session_state(&session, Some(dir.path())).unwrap();

    assert!(
        !session_dir.exists(),
        "transient cleanup should remove the staged-fs session directory"
    );
    assert!(
        !file.exists(),
        "transient staged writes must never reach the working tree"
    );
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
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "disk"));
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
    assert!(matches!(dict_get(&result, "content"), VmValue::String(s) if s.as_str() == "implicit"));
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
        matches!(dict_get(entry, "name"), VmValue::String(name) if name.as_str() == "new-parent")
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
        matches!(dict_get(entry, "name"), VmValue::String(name) if name.as_str() == "file.txt")
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

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn dict_str<'a>(value: &'a VmValue, key: &str) -> &'a str {
    match dict_get(value, key) {
        VmValue::String(s) => s.as_str(),
        other => panic!("expected string at `{key}`, got {other:?}"),
    }
}

fn dict_bool(value: &VmValue, key: &str) -> bool {
    match dict_get(value, key) {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool at `{key}`, got {other:?}"),
    }
}

fn dict_int(value: &VmValue, key: &str) -> i64 {
    match dict_get(value, key) {
        VmValue::Int(i) => *i,
        other => panic!("expected int at `{key}`, got {other:?}"),
    }
}

#[test]
fn safe_text_patch_applies_when_expected_hash_matches() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "alpha\n").unwrap();
    let reg = registry();
    let pre_hash = sha256_of(b"alpha\n");
    let after_hash = sha256_of(b"beta\n");

    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("beta\n")),
        ("expected_hash", vm_string(&pre_hash)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "applied");
    assert!(dict_bool(&response, "applied"));
    assert!(!dict_bool(&response, "stale_base"));
    assert_eq!(dict_str(&response, "current_hash"), pre_hash);
    assert_eq!(dict_str(&response, "before_sha256"), pre_hash);
    assert_eq!(dict_str(&response, "after_sha256"), after_hash);
    assert_eq!(fs::read_to_string(&file).unwrap(), "beta\n");
}

#[test]
fn safe_text_patch_rejects_stale_base_without_writing() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes.txt");
    fs::write(&file, "concurrent winner\n").unwrap();
    let reg = registry();
    let stale_expected = sha256_of(b"caller-stale\n");
    let actual_current = sha256_of(b"concurrent winner\n");

    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("agent-edit\n")),
        ("expected_hash", vm_string(&stale_expected)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "stale_base");
    assert!(!dict_bool(&response, "applied"));
    assert!(dict_bool(&response, "stale_base"));
    assert_eq!(dict_str(&response, "current_hash"), actual_current);
    assert_eq!(dict_str(&response, "expected_hash"), stale_expected);
    assert_eq!(fs::read_to_string(&file).unwrap(), "concurrent winner\n");
}

#[test]
fn safe_text_patch_treats_missing_file_as_empty_pre_image() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("fresh.txt");
    let reg = registry();
    let empty_hash = sha256_of(b"");

    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("hello\n")),
        ("expected_hash", vm_string(&empty_hash)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "applied");
    assert!(dict_bool(&response, "created"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello\n");
}

#[test]
fn safe_text_patch_short_circuits_on_no_op() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("idempotent.txt");
    fs::write(&file, "same\n").unwrap();
    let reg = registry();
    let pre_hash = sha256_of(b"same\n");
    let mtime_before = fs::metadata(&file).unwrap().modified().unwrap();

    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("same\n")),
        ("expected_hash", vm_string(&pre_hash)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "no_op");
    assert!(!dict_bool(&response, "applied"));
    assert_eq!(dict_int(&response, "bytes_written"), 0);
    let mtime_after = fs::metadata(&file).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "no_op must not touch the file on disk"
    );
}

#[test]
fn safe_text_patch_routes_through_staged_overlay() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("staged.txt");
    fs::write(&file, "disk-original\n").unwrap();
    let session = unique_session("safe-text-patch");
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
        ("content", vm_string("overlay-seed\n")),
    ]))
    .unwrap();

    let overlay_hash = sha256_of(b"overlay-seed\n");
    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("overlay-final\n")),
        ("expected_hash", vm_string(&overlay_hash)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "applied");
    assert_eq!(
        dict_str(&response, "current_hash"),
        overlay_hash,
        "must hash the overlay pre-image, not the on-disk content"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "disk-original\n",
        "working tree must stay untouched until commit"
    );

    let read = (reg.find("hostlib_tools_read_file").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("path", vm_string(&path_str(&file))),
    ]))
    .unwrap();
    assert_eq!(dict_str(&read, "content"), "overlay-final\n");

    (reg.find("hostlib_fs_commit_staged").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "overlay-final\n");
}

#[test]
fn safe_text_patch_skips_stale_base_when_no_expected_hash() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("unchecked.txt");
    fs::write(&file, "anything\n").unwrap();
    let reg = registry();

    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("replaced\n")),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "applied");
    assert_eq!(fs::read_to_string(&file).unwrap(), "replaced\n");
}

/// Regression: with multiple agents driving the same staged session, the
/// read → CAS check → write inside `hostlib_fs_safe_text_patch` must hold
/// the sessions mutex through the whole operation. Otherwise a sibling
/// agent can stage a write between the snapshot and the commit and have
/// its bytes silently clobbered.
///
/// The test drives the staged overlay from two threads anchored on the
/// same `expected_hash`. Exactly one of them must succeed; the other
/// must report `stale_base`. If the helper races, both succeed and the
/// later one wins — which is the bug.
#[test]
fn safe_text_patch_serializes_concurrent_staged_commits() {
    use std::thread;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("contended.txt");
    fs::write(&file, "v0\n").unwrap();
    let session = unique_session("safe-text-patch-race");
    let reg = Arc::new(registry());

    (reg.find("hostlib_fs_set_mode").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("mode", vm_string("staged")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let pre_hash = sha256_of(b"v0\n");
    let path_s = path_str(&file);
    let mut handles = Vec::new();
    for label in ["A", "B"] {
        let reg = Arc::clone(&reg);
        let session = session.clone();
        let pre_hash = pre_hash.clone();
        let path_s = path_s.clone();
        let body = format!("winner-{label}\n");
        handles.push(thread::spawn(move || {
            // tools:deterministic is thread-local; each spawned thread
            // must re-enable the gate the registry helper installed on
            // the main thread before driving `hostlib_fs_safe_text_patch`,
            // which is gated on the deterministic-tools feature.
            permissions::enable_for_test();
            let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
                ("session_id", vm_string(&session)),
                ("path", vm_string(&path_s)),
                ("content", vm_string(&body)),
                ("expected_hash", vm_string(&pre_hash)),
            ]))
            .unwrap();
            dict_str(&response, "result").to_string()
        }));
    }

    let mut applied = 0;
    let mut stale_base = 0;
    for handle in handles {
        let result = handle.join().unwrap();
        match result.as_str() {
            "applied" => applied += 1,
            "stale_base" => stale_base += 1,
            other => panic!("unexpected result `{other}`"),
        }
    }
    assert_eq!(applied, 1, "exactly one writer must commit");
    assert_eq!(stale_base, 1, "the other must observe stale_base");
}

#[test]
fn safe_text_patch_disk_rejects_missing_parent_when_create_parents_false() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("missing-parent").join("file.txt");
    let reg = registry();
    let empty_hash = sha256_of(b"");

    let err = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&nested))),
        ("content", vm_string("never written\n")),
        ("expected_hash", vm_string(&empty_hash)),
        ("create_parents", VmValue::Bool(false)),
    ]))
    .expect_err("must reject when parent directory does not exist");
    let message = format!("{err:?}");
    assert!(
        message.contains("parent directory") && message.contains("create_parents=true"),
        "error must explain the cause and the fix: {message}"
    );
    assert!(
        !nested.exists() && !nested.parent().unwrap().exists(),
        "must not have written the file or created the parent directory"
    );
}

#[test]
fn read_text_rejects_non_utf8_with_clear_diagnostic() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("blob.bin");
    fs::write(&file, [0xffu8, 0xfe, 0xfd]).unwrap();
    let reg = registry();

    let err = (reg.find("hostlib_fs_read_text").unwrap().handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&file)),
    )]))
    .expect_err("must reject non-UTF8 input");
    let message = format!("{err:?}");
    assert!(
        message.contains("UTF-8"),
        "error must mention UTF-8 as the cause: {message}"
    );
}

#[test]
fn safe_text_patch_round_trips_large_payload() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("large.txt");
    // ~1.5 MB — enough to verify the read + hash + write loop does not
    // truncate or hash a sliced view of the bytes.
    let payload = "abcdefghij".repeat(150_000);
    fs::write(&file, &payload).unwrap();
    let reg = registry();
    let pre_hash = sha256_of(payload.as_bytes());

    let new_payload = "xyzwvutsrq".repeat(150_000);
    let response = (reg.find("hostlib_fs_safe_text_patch").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string(&new_payload)),
        ("expected_hash", vm_string(&pre_hash)),
    ]))
    .unwrap();
    assert_eq!(dict_str(&response, "result"), "applied");
    assert_eq!(
        dict_int(&response, "bytes_written") as usize,
        new_payload.len()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), new_payload);
}

#[test]
fn emit_safe_text_patch_result_routes_to_active_session() {
    use harn_vm::agent_events::{
        register_wildcard_sink, unregister_wildcard_sink, AgentEvent, AgentEventSink,
    };
    use std::sync::{Arc, Mutex};

    struct Capture {
        events: Mutex<Vec<AgentEvent>>,
    }
    impl AgentEventSink for Capture {
        fn handle_event(&self, event: &AgentEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    let sink = Arc::new(Capture {
        events: Mutex::new(Vec::new()),
    });
    let handle = register_wildcard_sink(sink.clone());

    let reg = registry();
    let session = unique_session("emit-event");
    let _guard = harn_vm::agent_sessions::enter_current_session(session.clone());

    let response = (reg
        .find("hostlib_fs_emit_safe_text_patch_result")
        .unwrap()
        .handler)(&dict_arg(&[
        ("path", vm_string("/tmp/never-written.txt")),
        ("result", vm_string("stale_base")),
        ("hunks_count", VmValue::Int(3)),
        ("bytes_written", VmValue::Int(0)),
    ]))
    .unwrap();
    assert!(matches!(response, VmValue::Bool(true)));

    let events = sink.events.lock().unwrap();
    let event = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SafeTextPatchResult {
                session_id: sid,
                path,
                result,
                hunks_count,
                ..
            } if sid == &session => Some((path.clone(), result.clone(), *hunks_count)),
            _ => None,
        })
        .expect("event must route to the active session");
    drop(events);
    assert_eq!(event.0, "/tmp/never-written.txt");
    assert_eq!(event.1, "stale_base");
    assert_eq!(event.2, 3);

    unregister_wildcard_sink(handle);
}
