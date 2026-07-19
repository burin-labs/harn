use std::path::{Component, Path};

use super::{
    hash_label, paths::sanitize_component, safe_text_patch, scope_conditional_replace_lock_root,
    session_dir, set_mode, stage_write_or_none, staged_pending_paths, staged_status, FsMode,
    SafeTextPatchResult, STATE_REL,
};

#[test]
fn staged_pending_paths_preserve_native_filesystem_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical tempdir");
    let path = root.join("src").join("lib.rs");
    let session_id = format!(
        "native-paths-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    );

    set_mode(&session_id, FsMode::Staged, Some(&root)).expect("set staged mode");
    stage_write_or_none(
        "test",
        &path,
        b"fn beta() {}\n",
        true,
        true,
        Some(&session_id),
    )
    .expect("stage write")
    .expect("staged write");

    let native_paths = staged_pending_paths(&session_id).expect("pending paths");
    assert_eq!(
        native_paths,
        std::collections::BTreeSet::from([path.clone()])
    );
    let status = staged_status(&session_id).expect("staged status");
    assert_eq!(status.pending_writes.len(), 1);
    assert_eq!(
        status.pending_writes[0].path,
        crate::tools::args::to_agent_path(&path)
    );

    let _ = super::remove_session_state(&session_id, Some(&root));
}

#[test]
fn immediate_safe_patch_delegates_closed_outcomes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _locks = scope_conditional_replace_lock_root(dir.path().join("locks"));
    let path = dir.path().join("state.txt");
    std::fs::write(&path, b"one").expect("preimage");
    let observed = hash_label(b"one");

    let applied = safe_text_patch(&path, "two", Some(&observed), None, false, true)
        .expect("applied replacement");
    assert_eq!(applied.result, SafeTextPatchResult::Applied);
    assert!(!applied.created);

    let stale =
        safe_text_patch(&path, "three", Some(&observed), None, false, true).expect("stale receipt");
    assert_eq!(stale.result, SafeTextPatchResult::StaleBase);
    assert_eq!(std::fs::read(&path).expect("postimage"), b"two");

    let current = hash_label(b"two");
    let no_op =
        safe_text_patch(&path, "two", Some(&current), None, false, true).expect("no-op receipt");
    assert_eq!(no_op.result, SafeTextPatchResult::NoOp);
}

#[test]
fn dotted_session_ids_are_never_traversal_tokens() {
    for unsafe_id in ["..", ".", "...", ""] {
        let safe = sanitize_component(unsafe_id);
        assert_ne!(safe, unsafe_id, "`{unsafe_id}` passed through unsanitized");
        assert!(
            !safe.bytes().all(|byte| byte == b'.'),
            "`{unsafe_id}` -> `{safe}` is still all dots"
        );
        let components: Vec<_> = Path::new(&safe).components().collect();
        assert!(
            components
                .iter()
                .all(|component| matches!(component, Component::Normal(_))),
            "`{safe}` contains a traversal component"
        );
    }
}

#[test]
fn ordinary_session_ids_pass_through() {
    assert_eq!(sanitize_component("abc-123_v2.0"), "abc-123_v2.0");
}

#[test]
fn session_dir_stays_under_staged_root() {
    let dir = session_dir(Path::new("/workspace"), "..");
    assert!(
        !dir.components()
            .any(|component| matches!(component, Component::ParentDir)),
        "session_dir({dir:?}) escapes via `..`"
    );
    let mut staged = std::path::PathBuf::from("/workspace");
    staged.extend(STATE_REL);
    assert!(dir.starts_with(&staged), "{dir:?} not under {staged:?}");
}
