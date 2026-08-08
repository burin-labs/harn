//! Sandbox scope tests for accesses an active testbench overlay absorbs.
//!
//! Kept beside the other axis-specific suites so `tests.rs` stays within
//! the source-file-length cap.

use super::super::*;
use crate::orchestration::{pop_execution_policy, push_execution_policy};

#[test]
fn an_active_overlay_absorbs_mutations_but_not_pass_through_reads() {
    use crate::testbench::overlay_fs::{install_overlay, OverlayFs};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    // The overlay root sits OUTSIDE the workspace root on purpose: that is
    // the shape that used to fail, because scope was checked before the
    // overlay ever saw the access.
    let workspace = tmp.path().join("workspace");
    let overlay_root = tmp.path().join("fixture");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&overlay_root).unwrap();
    let on_disk = overlay_root.join("tracked.txt");
    std::fs::write(&on_disk, b"real bytes").unwrap();

    let overlay = Arc::new(OverlayFs::rooted_at(&overlay_root));
    let _overlay_guard = install_overlay(Arc::clone(&overlay));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let virtual_path = overlay_root.join("new.txt");
    assert!(
        check_fs_path_scope(&virtual_path, FsAccess::Write).is_ok(),
        "an overlay write never reaches disk, so scope must not reject it"
    );
    assert!(
        check_fs_path_scope(&on_disk, FsAccess::Delete).is_ok(),
        "an overlay delete is recorded in the layer, not applied to disk"
    );
    // A read the overlay would pass through to the real file is a genuine
    // disk read. Exempting it would let confined code read outside its
    // roots whenever a testbench happened to be active.
    assert!(
        check_fs_path_scope(&on_disk, FsAccess::Read).is_err(),
        "a pass-through read is not absorbed and must stay gated"
    );
    // Once the layer holds the path, the read is served from memory.
    overlay.write(&virtual_path, b"alpha").unwrap();
    assert!(
        check_fs_path_scope(&virtual_path, FsAccess::Read).is_ok(),
        "reading a path the layer holds never touches the filesystem"
    );
    // Outside the overlay root nothing changes.
    let elsewhere = tmp.path().join("elsewhere/secret");
    assert!(
        check_fs_path_scope(&elsewhere, FsAccess::Write).is_err(),
        "the carve-out is bounded by the overlay root"
    );

    pop_execution_policy();
}
