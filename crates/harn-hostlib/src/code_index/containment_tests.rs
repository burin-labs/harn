//! Every refactoring stays inside the indexed workspace and the sandbox's
//! path scope. Each test aims an op at a file outside, then checks that
//! file is byte-identical (or was never created).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::stdlib::process::set_thread_execution_context;
use harn_vm::VmValue;
use tempfile::{tempdir, TempDir};

use crate::code_index::{CodeIndexCapability, IndexState};

fn string(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert(harn_vm::value::intern_key(k), v.clone());
    }
    VmValue::dict(map)
}

fn write(root: &Path, path: &str, body: &str) {
    let abs = root.join(path);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, body).unwrap();
}

/// A canonical temporary directory, so sandbox roots compare equal to the
/// paths the builtins resolve.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn indexed(root: &Path) -> CodeIndexCapability {
    let capability = CodeIndexCapability::new();
    let (state, _) = IndexState::build_from_root(root);
    *capability.shared().lock().unwrap() = Some(state);
    capability
}

/// After indexing, swap the real directory `ws/src` for a symlink to
/// `outside`, which holds the same files. The index still lists
/// `src/<file>`, so an unchecked write lands in `outside`.
#[cfg(unix)]
fn swap_src_for_symlink(ws: &Path, outside: &Path) {
    fs::remove_dir_all(ws.join("src")).unwrap();
    std::os::unix::fs::symlink(outside, ws.join("src")).unwrap();
}

/// Restricted sandbox scoped to `roots`; popped on drop.
struct PolicyGuard;

impl PolicyGuard {
    fn worktree(roots: &[&Path]) -> Self {
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect(),
            ..CapabilityPolicy::default()
        });
        PolicyGuard
    }
}

impl Drop for PolicyGuard {
    fn drop(&mut self) {
        pop_execution_policy();
        set_thread_execution_context(None);
    }
}

const PY_FETCH: &str = "def fetch_rows():\n    return []\n\n\nfetch_rows()\n";

fn rename_request(path: &str) -> VmValue {
    dict(&[
        (
            "symbol_ref",
            dict(&[("name", string("fetch_rows")), ("path", string(path))]),
        ),
        ("new_name", string("load_rows")),
        ("scope", string("workspace")),
    ])
}

#[cfg(unix)]
#[test]
fn rename_does_not_write_through_a_directory_swapped_for_a_symlink() {
    let (_ws_dir, ws) = canonical_tempdir();
    let (_out_dir, outside) = canonical_tempdir();
    write(&ws, "src/a.py", PY_FETCH);
    write(&outside, "a.py", PY_FETCH);
    let capability = indexed(&ws);
    swap_src_for_symlink(&ws, &outside);

    let result = super::super::rename::run(&capability.shared(), &[rename_request("src/a.py")]);
    assert!(result.is_err(), "rename must refuse: {result:?}");
    assert_eq!(fs::read_to_string(outside.join("a.py")).unwrap(), PY_FETCH);
}

#[cfg(unix)]
#[test]
fn change_signature_does_not_write_through_a_directory_swapped_for_a_symlink() {
    const PY_AREA: &str = "def area(scale):\n    return scale\n\n\narea(2)\n";
    let (_ws_dir, ws) = canonical_tempdir();
    let (_out_dir, outside) = canonical_tempdir();
    write(&ws, "src/a.py", PY_AREA);
    write(&outside, "a.py", PY_AREA);
    let capability = indexed(&ws);
    swap_src_for_symlink(&ws, &outside);

    let request = dict(&[
        (
            "symbol_ref",
            dict(&[("name", string("area")), ("path", string("src/a.py"))]),
        ),
        (
            "params",
            VmValue::List(Arc::new(vec![dict(&[
                ("name", string("factor")),
                ("from", string("scale")),
            ])])),
        ),
    ]);
    let result = super::super::change_signature::run(&capability.shared(), &[request]);
    assert!(result.is_err(), "change_signature must refuse: {result:?}");
    assert_eq!(fs::read_to_string(outside.join("a.py")).unwrap(), PY_AREA);
}

#[test]
fn extract_without_an_index_respects_the_sandbox_scope() {
    const RUST_TOTAL: &str = "pub fn total(items: &[u32], bonus: u32) -> u32 {\n    let base = items.len() as u32;\n    let doubled = base * 2 + bonus;\n    doubled + 1\n}\n";
    let (_ws_dir, ws) = canonical_tempdir();
    let (_out_dir, outside) = canonical_tempdir();
    write(&outside, "lib.rs", RUST_TOTAL);
    let target = outside.join("lib.rs");
    let _policy = PolicyGuard::worktree(&[&ws]);

    let request = dict(&[
        ("path", string(&target.to_string_lossy())),
        ("start_line", VmValue::Int(2)),
        ("end_line", VmValue::Int(3)),
        ("new_name", string("scaled")),
        (
            "signature",
            string("fn scaled(items: &[u32], bonus: u32) -> u32"),
        ),
    ]);
    let result = super::super::extract::run(&CodeIndexCapability::new().shared(), &[request]);
    assert!(result.is_err(), "extract must refuse: {result:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), RUST_TOTAL);
}

#[test]
fn rename_respects_a_sandbox_scope_narrower_than_the_index() {
    let (_ws_dir, ws) = canonical_tempdir();
    write(&ws, "allowed/a.py", PY_FETCH);
    write(
        &ws,
        "other/b.py",
        "from allowed.a import fetch_rows\n\nfetch_rows()\n",
    );
    let capability = indexed(&ws);
    let _policy = PolicyGuard::worktree(&[&ws.join("allowed")]);

    let result = super::super::rename::run(&capability.shared(), &[rename_request("allowed/a.py")]);
    assert!(result.is_err(), "rename must refuse: {result:?}");
    assert_eq!(
        fs::read_to_string(ws.join("other/b.py")).unwrap(),
        "from allowed.a import fetch_rows\n\nfetch_rows()\n"
    );
    assert_eq!(
        fs::read_to_string(ws.join("allowed/a.py")).unwrap(),
        PY_FETCH
    );
}

#[test]
fn rename_refuses_when_an_out_of_scope_file_only_names_the_symbol() {
    // No call here, so no graph node puts `other/b.py` in scope; only the
    // word sweep finds it, and it must refuse rather than skip it.
    const USE: &str = "from allowed.a import fetch_rows\n\nhandler = fetch_rows\n";
    let (_ws_dir, ws) = canonical_tempdir();
    write(&ws, "allowed/a.py", PY_FETCH);
    write(&ws, "other/b.py", USE);
    let capability = indexed(&ws);
    let _policy = PolicyGuard::worktree(&[&ws.join("allowed")]);

    let result = super::super::rename::run(&capability.shared(), &[rename_request("allowed/a.py")]);
    assert!(result.is_err(), "rename must refuse: {result:?}");
    assert_eq!(fs::read_to_string(ws.join("other/b.py")).unwrap(), USE);
    assert_eq!(
        fs::read_to_string(ws.join("allowed/a.py")).unwrap(),
        PY_FETCH
    );
}
