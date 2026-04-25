//! Integration tests for `hostlib_tools_git` against a real fixture repo.
//!
//! These tests require `git` on `$PATH`. CI installs it; local dev usually
//! has it. If it's missing the tests are skipped via [`ensure_git`].

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use harn_hostlib::tools::permissions;
use harn_hostlib::{tools::ToolsCapability, BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;
use tempfile::TempDir;

fn registry() -> BuiltinRegistry {
    permissions::reset();
    permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
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

fn ensure_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// Initialize a tiny git repo with two commits, configured locally so the
/// test never reads global git config.
fn fixture_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "tester@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Tester"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);

    std::fs::write(dir.path().join("a.txt"), "first\n").unwrap();
    run_git(dir.path(), &["add", "a.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "first commit"]);

    std::fs::write(dir.path().join("a.txt"), "first\nsecond\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "new file\n").unwrap();
    run_git(dir.path(), &["add", "a.txt", "b.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "second commit"]);

    run_git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    );

    dir
}

fn run_git(repo: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    // Drop ambient GIT_* env vars so the test isn't perturbed by an
    // outer `git push` hook (which would otherwise leak GIT_DIR etc.
    // into the subprocess and silently break our tempdir isolation).
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    let output = cmd
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "git {} {} failed: {err}",
                args.first().unwrap_or(&""),
                repo.display()
            )
        });
    assert!(
        output.status.success(),
        "git {} (cwd={}) status={:?} stderr={}",
        args.join(" "),
        repo.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn list_of(value: &VmValue) -> &Rc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l,
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn git_log_returns_structured_entries() {
    if !ensure_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("log")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    let commits = list_of(data);
    assert_eq!(commits.len(), 2);
    if let VmValue::Dict(latest) = &commits[0] {
        if let Some(VmValue::String(s)) = latest.get("subject") {
            assert_eq!(s.as_ref(), "second commit");
        }
        if let Some(VmValue::String(sha)) = latest.get("sha") {
            assert_eq!(sha.len(), 40);
        }
    }
}

#[test]
fn git_log_max_count_limits_results() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("log")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
        ("max_count", VmValue::Int(1)),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    assert_eq!(list_of(data).len(), 1);
}

#[test]
fn git_status_reports_dirty_paths() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    std::fs::write(repo.path().join("c.txt"), "untracked\n").unwrap();

    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("status")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    let entries = list_of(data);
    assert!(entries.iter().any(|e| match dict_get(e, "path") {
        VmValue::String(s) => s.as_ref() == "c.txt",
        _ => false,
    }));
}

#[test]
fn git_current_branch_returns_main() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("current_branch")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    if let VmValue::String(s) = data {
        assert_eq!(s.as_ref(), "main");
    } else {
        panic!("expected string branch name, got {data:?}");
    }
}

#[test]
fn git_remote_list_returns_origin() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("remote_list")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    let remotes = list_of(data);
    assert_eq!(remotes.len(), 1);
    if let VmValue::Dict(d) = &remotes[0] {
        assert!(matches!(d.get("name"), Some(VmValue::String(s)) if s.as_ref() == "origin"));
        assert!(matches!(
            d.get("url"),
            Some(VmValue::String(s)) if s.as_ref() == "https://example.invalid/repo.git"
        ));
    }
}

#[test]
fn git_blame_returns_authors_per_line() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("blame")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
        ("path", vm_string("a.txt")),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    let lines = list_of(data);
    assert_eq!(lines.len(), 2);
    if let VmValue::Dict(first) = &lines[0] {
        assert!(matches!(
            first.get("author"),
            Some(VmValue::String(s)) if s.as_ref() == "Tester"
        ));
        assert!(matches!(first.get("line"), Some(VmValue::Int(1))));
    }
}

#[test]
fn git_show_emits_patch_text() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("show")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
        ("rev", vm_string("HEAD")),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    if let VmValue::String(s) = data {
        assert!(s.contains("second commit"));
        assert!(s.contains("b.txt"));
    } else {
        panic!("expected string");
    }
}

#[test]
fn git_diff_handles_clean_repo() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("diff")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    if let VmValue::String(s) = data {
        assert!(s.is_empty(), "expected empty diff, got `{s}`");
    }
}

#[test]
fn git_branch_list_returns_main() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let result = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("branch_list")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
    ]))
    .unwrap();
    let data = dict_get(&result, "data");
    let branches = list_of(data);
    assert_eq!(branches.len(), 1);
    if let VmValue::Dict(d) = &branches[0] {
        assert!(matches!(d.get("name"), Some(VmValue::String(s)) if s.as_ref() == "main"));
    }
}

#[test]
fn git_rejects_flag_lookalike_revs() {
    if !ensure_git() {
        return;
    }
    let repo = fixture_repo();
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let err = (entry.handler)(&dict_arg(&[
        ("operation", vm_string("show")),
        ("repo", vm_string(&repo.path().to_string_lossy())),
        ("rev", vm_string("--exec=rm")),
    ]))
    .unwrap_err();
    assert!(matches!(
        err,
        HostlibError::InvalidParameter { param: "rev", .. }
    ));
}

#[test]
fn git_rejects_unknown_operation() {
    let reg = registry();
    let entry = reg.find("hostlib_tools_git").unwrap();
    let err = (entry.handler)(&dict_arg(&[("operation", vm_string("rm-rf"))])).unwrap_err();
    assert!(matches!(
        err,
        HostlibError::InvalidParameter {
            param: "operation",
            ..
        }
    ));
}

#[test]
fn enable_builtin_flips_the_gate() {
    permissions::reset();
    let mut reg = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut reg);

    // Pre-enable: search must refuse.
    let search = reg.find("hostlib_tools_search").unwrap();
    let err = (search.handler)(&dict_arg(&[("pattern", vm_string("foo"))])).unwrap_err();
    assert!(matches!(err, HostlibError::Backend { .. }));

    // hostlib_enable can be called with either a bare string or a dict.
    let enable = reg.find("hostlib_enable").unwrap();
    let result = (enable.handler)(&[vm_string("tools:deterministic")]).unwrap();
    if let VmValue::Dict(d) = &result {
        assert!(matches!(d.get("enabled"), Some(VmValue::Bool(true))));
        assert!(matches!(d.get("newly_enabled"), Some(VmValue::Bool(true))));
    } else {
        panic!("expected dict");
    }

    // After enable: search returns Ok (with no matches in a tmpdir).
    let dir = tempfile::TempDir::new().unwrap();
    let result = (search.handler)(&dict_arg(&[
        ("pattern", vm_string("foo")),
        ("path", vm_string(&dir.path().to_string_lossy())),
    ]))
    .unwrap();
    assert!(matches!(&result, VmValue::Dict(_)));

    // Calling enable a second time reports the feature as already on.
    let again = (enable.handler)(&[vm_string("tools:deterministic")]).unwrap();
    if let VmValue::Dict(d) = &again {
        assert!(matches!(d.get("newly_enabled"), Some(VmValue::Bool(false))));
    }
}

#[test]
fn enable_builtin_rejects_unknown_features() {
    let mut reg = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut reg);
    let enable = reg.find("hostlib_enable").unwrap();
    let err = (enable.handler)(&[vm_string("tools:exec")]).unwrap_err();
    assert!(matches!(
        err,
        HostlibError::InvalidParameter {
            param: "feature",
            ..
        }
    ));
}
