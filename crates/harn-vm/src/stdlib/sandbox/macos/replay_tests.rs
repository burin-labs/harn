//! A running opaque child must stay outside old and future peer workspaces.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};

use super::super::*;
use super::append_boundary_for;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const PROBE: &str = r#"from pathlib import Path
import json
import os
import socket
import sys

workspace, original, sibling, future, alias, alternate = map(Path, sys.argv[1:])
print('BOUNDARY_CHILD_READY', flush=True)
assert sys.stdin.readline() == 'continue\n'
results = {}
def attempt(name, operation):
    try:
        operation()
        results[name] = True
    except OSError:
        results[name] = False

for name, root in [('original', original), ('sibling', sibling), ('future', future), ('alias', alias), ('alternate', alternate)]:
    attempt(name + '_read', lambda: (root / 'canary').read_text())
    attempt(name + '_metadata', lambda: (root / 'canary').stat())
    attempt(name + '_write', lambda: (root / 'new-file').write_text('outside write'))
attempt('future_create', lambda: (future.parent / 'another-peer').mkdir())
attempt('hardlink_original', lambda: os.link(original / 'canary', workspace / 'linked'))
attempt('symlink_original', lambda: (workspace / 'original-link' / 'canary').read_text())
def rename_and_restore(path):
    moved = path.with_name(path.name + '-moved')
    path.rename(moved)
    try:
        assert moved.exists()
    finally:
        moved.rename(path)
attempt('rename_original', lambda: rename_and_restore(original))
attempt('rename_original_parent', lambda: rename_and_restore(original.parent))
attempt('rename_container', lambda: rename_and_restore(workspace.parents[1]))
def connect(root):
    with socket.socket(socket.AF_UNIX) as client:
        client.connect(str(root / 'ipc.sock'))
attempt('original_socket', lambda: connect(original))
attempt('future_socket', lambda: connect(future))
attempt('branch_write', lambda: (workspace / 'branch-file').write_text('branch only'))
attempt('branch_read', lambda: (workspace / 'branch-file').read_text())
print(json.dumps(results), flush=True)
"#;

fn run_matrix(confined: bool) {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let original = root.join("original-parent/workspace");
    let container = root.join("pool");
    let workspace = container.join("arm/workspace");
    let sibling = container.join("sibling/workspace");
    let future = container.join("future/workspace");
    let alias = root.join("recorded-alias");
    let alternate = original
        .display()
        .to_string()
        .replacen("/private/tmp/", "/tmp/", 1);
    assert_ne!(
        alternate,
        original.display().to_string(),
        "exercise the real /tmp alias"
    );
    for path in [&original, &workspace, &sibling] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(original.join("canary"), "original canary").unwrap();
    std::fs::write(sibling.join("canary"), "sibling canary").unwrap();
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    std::os::unix::fs::symlink(&original, workspace.join("original-link")).unwrap();
    let script = workspace.join("probe.py");
    std::fs::write(&script, PROBE).unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![root.display().to_string()],
        process_sandbox: Box::new(crate::orchestration::ProcessSandboxPolicy {
            read_roots: vec![root.display().to_string()],
            write_roots: vec![root.display().to_string()],
            unix_socket_roots: vec![root.display().to_string()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut profile = render_profile_for_program(&policy, "/usr/bin/python3");
    if confined {
        append_boundary_for(
            &mut profile,
            &super::super::super::replay::FilesystemBoundary {
                recorded: vec![original.clone(), alias.clone()],
                workspace: workspace.clone(),
                container,
            },
        );
    }
    let original_socket = UnixListener::bind(original.join("ipc.sock")).unwrap();
    let mut child = ChildGuard(
        Command::new(SANDBOX_EXEC_PATH)
            .args(["-p", &profile, "--", "/usr/bin/python3", "-I", "-u"])
            .arg(&script)
            .args([&workspace, &original, &sibling, &future, &alias])
            .arg(alternate)
            .current_dir(&workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = BufReader::new(child.0.stdout.take().unwrap());
    let mut ready = String::new();
    stdout.read_line(&mut ready).unwrap();
    assert_eq!(
        ready, "BOUNDARY_CHILD_READY\n",
        "the real sandboxed child must reach the barrier"
    );
    // This peer and socket did not exist when the profile was rendered or installed.
    std::fs::create_dir_all(&future).unwrap();
    std::fs::write(future.join("canary"), "future canary").unwrap();
    let future_socket = UnixListener::bind(future.join("ipc.sock")).unwrap();
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(b"continue\n")
        .unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    let status = child.0.wait().unwrap();
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "{status}: {output}\n{stderr}");
    let results: std::collections::BTreeMap<String, bool> = serde_json::from_str(&output).unwrap();
    assert_eq!(
        results.len(),
        25,
        "every operation must report a result: {results:?}"
    );
    for (name, allowed) in results {
        let expected = !confined || name.starts_with("branch_");
        assert_eq!(allowed, expected, "{name}, confined={confined}: {stderr}");
    }
    assert_eq!(
        std::fs::read_to_string(original.join("canary")).unwrap(),
        "original canary"
    );
    assert_eq!(
        std::fs::read_to_string(sibling.join("canary")).unwrap(),
        "sibling canary"
    );
    assert_eq!(
        std::fs::read_to_string(future.join("canary")).unwrap(),
        "future canary"
    );
    drop((original_socket, future_socket));
}

#[test]
fn a_running_child_cannot_reach_original_or_future_peer_workspaces() {
    run_matrix(true);
}

#[test]
fn without_the_final_boundary_the_same_child_reaches_every_probe() {
    run_matrix(false);
}

#[test]
fn an_inherited_parent_cannot_substitute_for_the_replay_boundary() {
    const CASE: &str = "stdlib::sandbox::macos::replay::tests::an_inherited_parent_cannot_substitute_for_the_replay_boundary";
    const CHILD: &str = "REPLAY_BOUNDARY_PARENT_PROBE";
    if std::env::var_os(CHILD).is_some() {
        assert!(nested::process_is_sandboxed());
        let refusal =
            check_replay_supported().expect_err("an inherited profile cannot be narrowed");
        assert!(
            refusal.to_string().contains("inherited macOS sandbox"),
            "{refusal}"
        );
        println!("REPLAY_INHERITED_PARENT_REFUSED");
        return;
    }
    match crate::sandbox_host::unavailable_worktree_sandbox() {
        None => check_replay_supported().expect("the control host supports replay"),
        Some(expected) => {
            let refusal =
                check_replay_supported().expect_err("the measured gap must refuse replay");
            assert!(
                refusal
                    .to_string()
                    .contains("without an available macOS filesystem sandbox"),
                "{refusal}; {expected}"
            );
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let denied = temp.path().join("parent-denied");
    let profile = format!(
        "(version 1)\n(allow default)\n(deny file-write-data (literal \"{}\"))\n",
        sandbox_profile_escape(&denied.display().to_string()),
    );
    let output = Command::new(SANDBOX_EXEC_PATH)
        .args(["-p", &profile, "--"])
        .arg(std::env::current_exe().unwrap())
        .args([CASE, "--exact", "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("REPLAY_INHERITED_PARENT_REFUSED"));
}
