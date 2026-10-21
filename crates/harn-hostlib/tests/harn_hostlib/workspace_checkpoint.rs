//! Conformance fixtures for turn-boundary workspace checkpoints.
//!
//! Every fixture drives the registered `hostlib_fs_checkpoint_*` builtins
//! against a real git worktree, the way a host brackets an agent turn: capture
//! a boundary, bracket each tool or shell call with begin/end, then restore.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use harn_hostlib::workspace_checkpoint::WorkspaceCheckpointCapability;
use harn_hostlib::BuiltinRegistry;
use serde_json::{json, Value};
use tempfile::TempDir;

use super::support::*;

fn registry() -> BuiltinRegistry {
    registry_with(&[&WorkspaceCheckpointCapability])
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A committed worktree with a.txt, b.txt, c.txt, d.txt, and a binary blob.
fn repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "t"]);
    fs::write(root.join("a.txt"), "a original\n").unwrap();
    fs::write(root.join("b.txt"), "b original\n").unwrap();
    fs::write(root.join("c.txt"), "c original\n").unwrap();
    fs::write(root.join("d.txt"), "d original\n").unwrap();
    fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 255, 0, 1]).unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    dir
}

struct Session {
    reg: BuiltinRegistry,
    id: String,
    root: String,
}

impl Session {
    fn new(dir: &Path) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self {
            reg: registry(),
            id: format!(
                "checkpoint-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            root: path_str(dir),
        }
    }

    fn call(&self, name: &str, mut entries: Vec<(&str, harn_vm::VmValue)>) -> Value {
        entries.push(("session_id", vm_string(&self.id)));
        let value = call(
            &self.reg,
            &format!("hostlib_fs_checkpoint_{name}"),
            dict(&entries),
        );
        harn_vm::llm::vm_value_to_json(&value)
    }

    fn capture(&self, boundary: &str) -> Value {
        self.call(
            "capture",
            vec![
                ("boundary_id", vm_string(boundary)),
                ("root", vm_string(&self.root)),
            ],
        )
    }

    /// Run `body` between call_begin and call_end; returns the attribution.
    fn call_with(&self, call_id: &str, side_effect: Option<&str>, body: impl FnOnce()) -> Value {
        let begin = self.call("call_begin", vec![("call_id", vm_string(call_id))]);
        assert_eq!(begin["available"], true, "{begin}");
        body();
        let mut entries = vec![("call_id", vm_string(call_id))];
        if let Some(level) = side_effect {
            entries.push(("side_effect", vm_string(level)));
        }
        self.call("call_end", entries)
    }

    fn plan(&self, boundary: &str) -> Value {
        self.call("plan", vec![("boundary_id", vm_string(boundary))])
    }

    fn restore(&self, boundary: &str) -> Value {
        self.call("restore", vec![("boundary_id", vm_string(boundary))])
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        harn_hostlib::workspace_checkpoint::drop_session(&self.id);
    }
}

fn paths(receipt: &Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = receipt[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} missing in {receipt}"))
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

#[test]
fn tool_edit_is_attributed_and_restored() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    assert_eq!(s.capture("turn-1")["available"], true);

    let attributed = s.call_with("tool-1", Some("workspace_write"), || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    assert_eq!(attributed["changes"][0]["path"], "a.txt");
    assert_eq!(attributed["changes"][0]["kind"], "modified");
    assert!(attributed["changes"][0]["post_hash"].is_string());

    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["a.txt"]);
    assert_eq!(receipt["restored"][0]["action"], "restore");
    assert!(paths(&receipt, "refused").is_empty());
    assert_eq!(read(root, "a.txt"), "a original\n");
}

#[cfg(unix)]
#[test]
fn shell_edit_create_and_delete_are_attributed_and_restored() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");

    let attributed = s.call_with("shell-1", Some("process_exec"), || {
        let status = Command::new("sh")
            .current_dir(root)
            .args([
                "-c",
                "echo 'c rewritten by shell' > c.txt; rm d.txt; echo new > new.txt",
            ])
            .status()
            .unwrap();
        assert!(status.success());
    });
    let kinds: Vec<(String, String)> = attributed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().into(),
                c["kind"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("c.txt".into(), "modified".into()),
            ("d.txt".into(), "deleted".into()),
            ("new.txt".into(), "created".into()),
        ]
    );

    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["c.txt", "d.txt", "new.txt"]);
    let actions: Vec<&str> = receipt["restored"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["restore", "recreate", "delete"]);
    assert_eq!(read(root, "c.txt"), "c original\n");
    assert_eq!(read(root, "d.txt"), "d original\n");
    assert!(!root.join("new.txt").exists());
}

#[test]
fn created_file_in_a_new_directory_is_deleted_with_its_directory() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::create_dir_all(root.join("gen/nested")).unwrap();
        fs::write(root.join("gen/nested/out.txt"), "generated").unwrap();
    });
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["gen/nested/out.txt"]);
    assert!(!root.join("gen").exists());
}

#[test]
fn binary_file_round_trips_byte_for_byte() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    // An untracked binary already present at the boundary is stored, not
    // recovered from git.
    let untracked = [255u8, 254, 0, 0, 1, 2, 3];
    fs::write(root.join("scratch.bin"), untracked).unwrap();
    let captured = s.capture("turn-1");
    assert_eq!(captured["stored_files"], 1, "{captured}");

    s.call_with("tool-1", None, || {
        fs::write(root.join("blob.bin"), [9u8, 9, 9]).unwrap();
        fs::write(root.join("scratch.bin"), [7u8; 40]).unwrap();
    });
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["blob.bin", "scratch.bin"]);
    assert_eq!(
        fs::read(root.join("blob.bin")).unwrap(),
        [0u8, 159, 146, 150, 255, 0, 1]
    );
    assert_eq!(fs::read(root.join("scratch.bin")).unwrap(), untracked);
}

#[test]
fn file_dirty_at_the_boundary_restores_to_its_dirty_content_not_head() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    fs::write(root.join("a.txt"), "a uncommitted work\n").unwrap();
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a agent rewrite, different length\n").unwrap();
    });
    s.restore("turn-1");
    assert_eq!(read(root, "a.txt"), "a uncommitted work\n");
}

#[test]
fn concurrent_writer_to_a_different_file_is_kept() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    // A person edits b.txt between the agent's calls.
    fs::write(root.join("b.txt"), "b edited by a person\n").unwrap();
    s.call_with("tool-2", None, || {
        fs::write(root.join("c.txt"), "c edited by the agent\n").unwrap();
    });

    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["a.txt", "c.txt"]);
    assert_eq!(read(root, "b.txt"), "b edited by a person\n");
    assert_eq!(read(root, "a.txt"), "a original\n");
}

#[test]
fn concurrent_writer_to_the_same_file_is_refused() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    let attributed = s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    fs::write(root.join("a.txt"), "a overwritten by a person\n").unwrap();

    let plan = s.plan("turn-1");
    assert_eq!(plan["entries"][0]["action"], "refuse");

    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "refused"), ["a.txt"]);
    assert!(paths(&receipt, "restored").is_empty());
    let refused = &receipt["refused"][0];
    assert_eq!(refused["reason"], "changed outside the session");
    assert_eq!(
        refused["expected_hash"],
        attributed["changes"][0]["post_hash"]
    );
    assert_ne!(refused["found_hash"], refused["expected_hash"]);
    assert_eq!(read(root, "a.txt"), "a overwritten by a person\n");
}

#[test]
fn restore_reports_effects_it_cannot_undo() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("push-1", Some("network"), || {});
    s.call_with("read-1", Some("network_read"), || {});
    let receipt = s.restore("turn-1");
    assert_eq!(
        receipt["irreversible"],
        json!([{"call_id": "push-1", "side_effect": "network"}])
    );
}

#[test]
fn a_path_already_at_the_boundary_is_skipped_not_rewritten() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    fs::write(root.join("a.txt"), "a original\n").unwrap();
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "skipped"), ["a.txt"]);
    assert!(paths(&receipt, "restored").is_empty());
}

#[test]
fn ignored_files_are_never_attributed() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    let attributed = s.call_with("tool-1", None, || {
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/cache.bin"), "x").unwrap();
    });
    assert_eq!(attributed["changes"], json!([]));
}

#[test]
fn non_git_directory_is_explicitly_unavailable() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "not in a repo").unwrap();
    let s = Session::new(dir.path());

    let captured = s.capture("turn-1");
    assert_eq!(captured["available"], false, "{captured}");
    assert_eq!(captured["reason"], "not_a_git_worktree");

    // Nothing was captured, so no later method may report an empty success.
    for method in ["call_begin", "call_end"] {
        let result = s.call(method, vec![("call_id", vm_string("tool-1"))]);
        assert_eq!(result["available"], false, "{method}: {result}");
        assert_eq!(result["reason"], "no_checkpoint");
    }
    let restore = s.restore("turn-1");
    assert_eq!(restore["available"], false);
}

#[test]
fn unknown_boundary_is_unavailable() {
    let dir = repo();
    let s = Session::new(dir.path());
    s.capture("turn-1");
    let unknown = s.plan("turn-9");
    assert_eq!(unknown["available"], false);
    assert_eq!(unknown["reason"], "unknown_boundary");
}

#[test]
fn worktree_inside_an_ignored_directory_of_an_outer_repo_is_still_scanned() {
    let outer = repo();
    let inner = outer.path().join("ignored/inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-q"]);
    git(&inner, &["config", "user.email", "t@example.com"]);
    git(&inner, &["config", "user.name", "t"]);
    fs::write(inner.join("a.txt"), "a original\n").unwrap();
    git(&inner, &["add", "-A"]);
    git(&inner, &["commit", "-q", "-m", "init"]);

    let s = Session::new(&inner);
    s.capture("turn-1");
    let attributed = s.call_with("tool-1", None, || {
        fs::write(inner.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    assert_eq!(attributed["changes"][0]["path"], "a.txt", "{attributed}");
}
