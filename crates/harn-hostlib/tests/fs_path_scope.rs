//! Workspace-root path-scope enforcement for the path-touching hostlib
//! builtins (issue #2600, follow-up to the coarse `tools:deterministic`
//! gate from #2548).
//!
//! Under a restricted `SandboxProfile` with explicit `workspace_roots`,
//! every builtin that resolves a host filesystem path must reject paths
//! outside the roots — for reads, writes, deletes, patches, AST edits, and
//! the staged-fs commit flush — matching what `harness.fs.*` enforces
//! VM-side. In-root paths must still succeed, and relative paths must be
//! resolved before the check so the two surfaces agree.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use harn_hostlib::tools::permissions;
use harn_hostlib::{
    ast::AstCapability, fs::FsCapability, tools::ToolsCapability, BuiltinRegistry,
    HostlibCapability, HostlibError,
};
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::stdlib::process::set_thread_execution_context;
use harn_vm::VmValue;
use tempfile::TempDir;

/// Build a registry carrying every path-touching capability surface, with
/// the deterministic-tools feature enabled so the coarse gate never gets in
/// the way of the scope assertions.
fn registry() -> BuiltinRegistry {
    permissions::reset();
    permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    FsCapability.register_builtins(&mut registry);
    AstCapability.register_builtins(&mut registry);
    registry
}

/// Push a restricted `Worktree` profile scoped to `roots`, returning a
/// guard that pops the policy (and clears any thread execution context) on
/// drop so a panicking assertion can't leak policy into a sibling test.
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

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::dict(map)]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Arc::from(s))
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn call(
    reg: &BuiltinRegistry,
    name: &str,
    entries: &[(&str, VmValue)],
) -> Result<VmValue, HostlibError> {
    let entry = reg
        .find(name)
        .unwrap_or_else(|| panic!("{name} registered"));
    (entry.handler)(&dict_arg(entries))
}

/// Assert a call rejected with the typed sandbox violation pointing at the
/// builtin and carrying the canonical out-of-root message.
fn assert_rejected(result: Result<VmValue, HostlibError>, expect_builtin: &str) {
    match result {
        Err(HostlibError::SandboxViolation {
            builtin, message, ..
        }) => {
            assert_eq!(builtin, expect_builtin, "violation names the builtin");
            assert!(
                message.contains("outside workspace_roots"),
                "message describes the scope rejection: {message}"
            );
        }
        other => panic!("expected SandboxViolation from {expect_builtin}, got {other:?}"),
    }
}

#[test]
fn read_write_delete_list_respect_workspace_roots() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let in_file = root.path().join("in.txt");
    fs::write(&in_file, "hello").unwrap();
    let out_file = outside.path().join("out.txt");
    fs::write(&out_file, "secret").unwrap();

    let reg = registry();
    let _guard = PolicyGuard::worktree(&[root.path()]);

    // Reads
    call(
        &reg,
        "hostlib_tools_read_file",
        &[("path", vm_string(&path_string(&in_file)))],
    )
    .expect("in-root read succeeds");
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_read_file",
            &[("path", vm_string(&path_string(&out_file)))],
        ),
        "hostlib_tools_read_file",
    );

    // Lists
    call(
        &reg,
        "hostlib_tools_list_directory",
        &[("path", vm_string(&path_string(root.path())))],
    )
    .expect("in-root list succeeds");
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_list_directory",
            &[("path", vm_string(&path_string(outside.path())))],
        ),
        "hostlib_tools_list_directory",
    );

    // Writes
    let new_in = root.path().join("created.txt");
    call(
        &reg,
        "hostlib_tools_write_file",
        &[
            ("path", vm_string(&path_string(&new_in))),
            ("content", vm_string("x")),
        ],
    )
    .expect("in-root write succeeds");
    assert!(new_in.exists());
    let new_out = outside.path().join("created.txt");
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_write_file",
            &[
                ("path", vm_string(&path_string(&new_out))),
                ("content", vm_string("x")),
            ],
        ),
        "hostlib_tools_write_file",
    );
    assert!(!new_out.exists(), "rejected write must not touch disk");

    // Deletes
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_delete_file",
            &[("path", vm_string(&path_string(&out_file)))],
        ),
        "hostlib_tools_delete_file",
    );
    assert!(out_file.exists(), "rejected delete must not touch disk");
    call(
        &reg,
        "hostlib_tools_delete_file",
        &[("path", vm_string(&path_string(&in_file)))],
    )
    .expect("in-root delete succeeds");
    assert!(!in_file.exists());
}

#[test]
fn search_and_outline_respect_workspace_roots() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(root.path().join("a.rs"), "fn alpha() {}\n").unwrap();
    fs::write(outside.path().join("b.rs"), "fn beta() {}\n").unwrap();

    let reg = registry();
    let _guard = PolicyGuard::worktree(&[root.path()]);

    call(
        &reg,
        "hostlib_tools_search",
        &[
            ("pattern", vm_string("fn ")),
            ("path", vm_string(&path_string(root.path()))),
        ],
    )
    .expect("in-root search succeeds");
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_search",
            &[
                ("pattern", vm_string("fn ")),
                ("path", vm_string(&path_string(outside.path()))),
            ],
        ),
        "hostlib_tools_search",
    );

    call(
        &reg,
        "hostlib_tools_get_file_outline",
        &[("path", vm_string(&path_string(&root.path().join("a.rs"))))],
    )
    .expect("in-root outline succeeds");
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_get_file_outline",
            &[(
                "path",
                vm_string(&path_string(&outside.path().join("b.rs"))),
            )],
        ),
        "hostlib_tools_get_file_outline",
    );
}

#[test]
fn safe_text_patch_and_read_text_respect_workspace_roots() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let in_file = root.path().join("p.txt");
    fs::write(&in_file, "v1\n").unwrap();
    let out_file = outside.path().join("p.txt");
    fs::write(&out_file, "v1\n").unwrap();

    let reg = registry();
    let _guard = PolicyGuard::worktree(&[root.path()]);

    call(
        &reg,
        "hostlib_fs_read_text",
        &[("path", vm_string(&path_string(&in_file)))],
    )
    .expect("in-root read_text succeeds");
    assert_rejected(
        call(
            &reg,
            "hostlib_fs_read_text",
            &[("path", vm_string(&path_string(&out_file)))],
        ),
        "hostlib_fs_read_text",
    );

    call(
        &reg,
        "hostlib_fs_safe_text_patch",
        &[
            ("path", vm_string(&path_string(&in_file))),
            ("content", vm_string("v2\n")),
        ],
    )
    .expect("in-root patch succeeds");
    assert_eq!(fs::read_to_string(&in_file).unwrap(), "v2\n");
    assert_rejected(
        call(
            &reg,
            "hostlib_fs_safe_text_patch",
            &[
                ("path", vm_string(&path_string(&out_file))),
                ("content", vm_string("v2\n")),
            ],
        ),
        "hostlib_fs_safe_text_patch",
    );
    assert_eq!(
        fs::read_to_string(&out_file).unwrap(),
        "v1\n",
        "rejected patch must not touch disk"
    );
}

#[test]
fn ast_edits_respect_workspace_roots() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let src = "fn alpha() { return 1 }\n";
    let in_file = root.path().join("edit.rs");
    fs::write(&in_file, src).unwrap();
    let out_file = outside.path().join("edit.rs");
    fs::write(&out_file, src).unwrap();

    let reg = registry();
    let _guard = PolicyGuard::worktree(&[root.path()]);

    // apply_node rewrites the integer literal in-place.
    call(
        &reg,
        "hostlib_ast_apply_node",
        &[
            ("path", vm_string(&path_string(&in_file))),
            ("query", vm_string("(integer_literal) @target")),
            ("replacement", vm_string("2")),
        ],
    )
    .expect("in-root apply_node succeeds");
    assert!(fs::read_to_string(&in_file).unwrap().contains("return 2"));
    assert_rejected(
        call(
            &reg,
            "hostlib_ast_apply_node",
            &[
                ("path", vm_string(&path_string(&out_file))),
                ("query", vm_string("(integer_literal) @target")),
                ("replacement", vm_string("2")),
            ],
        ),
        "hostlib_ast_apply_node",
    );
    assert_eq!(
        fs::read_to_string(&out_file).unwrap(),
        src,
        "rejected AST edit must not touch disk"
    );

    assert_rejected(
        call(
            &reg,
            "hostlib_ast_insert_at_anchor",
            &[
                ("path", vm_string(&path_string(&out_file))),
                ("query", vm_string("(function_item) @anchor")),
                ("content", vm_string("// added\n")),
                ("position", vm_string("before")),
            ],
        ),
        "hostlib_ast_insert_at_anchor",
    );
}

#[test]
fn staged_commit_enforces_scope_against_target_path() {
    // The overlay path always lives inside the workspace; commit flushes to
    // the logical target. A target outside the roots active at commit time
    // must be refused even though staging it under a permissive context
    // succeeded.
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let session = "fs-path-scope-commit";
    let in_target = root.path().join("staged_in.txt");
    let out_target = outside.path().join("staged_out.txt");

    let reg = registry();

    // Stage two writes with no active policy (unrestricted), so both land in
    // the overlay regardless of where the target points.
    call(
        &reg,
        "hostlib_fs_set_mode",
        &[
            ("session_id", vm_string(session)),
            ("mode", vm_string("staged")),
            ("root", vm_string(&path_string(root.path()))),
        ],
    )
    .expect("set staged mode");
    call(
        &reg,
        "hostlib_tools_write_file",
        &[
            ("session_id", vm_string(session)),
            ("path", vm_string(&path_string(&in_target))),
            ("content", vm_string("in\n")),
        ],
    )
    .expect("stage in-root write");
    call(
        &reg,
        "hostlib_tools_write_file",
        &[
            ("session_id", vm_string(session)),
            ("path", vm_string(&path_string(&out_target))),
            ("content", vm_string("out\n")),
        ],
    )
    .expect("stage out-of-root write");

    // Now commit under a restricted profile scoped to `root`. The in-root
    // target flushes; the out-of-root target is rejected and left unwritten.
    let result = {
        let _guard = PolicyGuard::worktree(&[root.path()]);
        call(
            &reg,
            "hostlib_fs_commit_staged",
            &[("session_id", vm_string(session))],
        )
        .expect("commit returns a result envelope")
    };

    assert!(in_target.exists(), "in-root target flushed to disk");
    assert!(!out_target.exists(), "out-of-root target must not flush");

    let committed = match dict_field(&result, "committed_paths") {
        VmValue::List(items) => items.clone(),
        other => panic!("committed_paths is a list, got {other:?}"),
    };
    assert_eq!(committed.len(), 1, "exactly one path committed");
    let failed = match dict_field(&result, "failed_paths_with_reasons") {
        VmValue::List(items) => items.clone(),
        other => panic!("failed_paths_with_reasons is a list, got {other:?}"),
    };
    assert_eq!(failed.len(), 1, "the out-of-root target is reported failed");
    let reason = dict_field(&failed[0], "reason");
    match reason {
        VmValue::String(s) => assert!(
            s.contains("outside workspace_roots"),
            "failure reason is the scope rejection: {s}"
        ),
        other => panic!("reason is a string, got {other:?}"),
    }

    call(
        &reg,
        "hostlib_fs_discard_staged",
        &[("session_id", vm_string(session))],
    )
    .ok();
}

#[test]
fn relative_paths_are_resolved_before_the_scope_check() {
    // Hostlib resolves relative paths against the process CWD, which cargo
    // sets to the crate directory. Anchoring the workspace root there keeps
    // the scope check and the actual I/O consistent, and lets us exercise
    // `..` normalization against real files without chdir-ing the process.
    let root = std::env::current_dir().unwrap();

    let reg = registry();
    let _guard = PolicyGuard::worktree(&[root.as_path()]);

    // `src/../Cargo.toml` only lands in-root *after* `..` collapses — a
    // literal prefix check would not recognize it as in-scope, so this
    // proves the path is normalized before the scope decision.
    call(
        &reg,
        "hostlib_tools_read_file",
        &[("path", vm_string("src/../Cargo.toml"))],
    )
    .expect("relative in-root read succeeds after normalization");

    // `..` that escapes the crate root (into a sibling crate) is rejected.
    assert_rejected(
        call(
            &reg,
            "hostlib_tools_read_file",
            &[("path", vm_string("../harn-vm/Cargo.toml"))],
        ),
        "hostlib_tools_read_file",
    );
}

fn dict_field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).unwrap_or_else(|| panic!("key {key} present")),
        other => panic!("not a dict: {other:?}"),
    }
}
