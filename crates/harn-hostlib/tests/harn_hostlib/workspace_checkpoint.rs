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
pub(super) fn repo() -> TempDir {
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
    let capture = s.capture("turn-1");
    assert_eq!(capture["available"], true);
    assert_eq!(capture["writer_policy"], "legacy_scan");

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

#[test]
fn corrupt_checkpoint_blob_is_reported_without_overwriting_the_workspace() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "dirty baseline\n").unwrap();
    let s = Session::new(root);
    assert_eq!(s.capture("turn-1")["available"], true);
    s.call_with("tool-1", Some("workspace_write"), || {
        std::fs::write(root.join("a.txt"), "agent edit\n").unwrap();
    });
    let blobs = root
        .join(".harn/state/checkpoints")
        .join(&s.id)
        .join("blobs");
    let blob = std::fs::read_dir(blobs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(blob, "corrupt checkpoint\n").unwrap();
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "failed"), ["a.txt"], "{receipt}");
    assert!(receipt["failed"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("hash mismatch"));
    assert_eq!(read(root, "a.txt"), "agent edit\n");
}

/// A subprocess, not an in-process write, changes the tree: the stat diff must
/// attribute what a shell command did without seeing the command itself.
#[test]
fn subprocess_edit_create_and_delete_are_attributed_and_restored() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");

    let attributed = s.call_with("shell-1", Some("process_exec"), || {
        git(root, &["rm", "-q", "d.txt"]);
        git(root, &["mv", "c.txt", "renamed.txt"]);
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
            ("c.txt".into(), "deleted".into()),
            ("d.txt".into(), "deleted".into()),
            ("renamed.txt".into(), "created".into()),
        ]
    );

    let receipt = s.restore("turn-1");
    assert_eq!(
        paths(&receipt, "restored"),
        ["c.txt", "d.txt", "renamed.txt"]
    );
    let actions: Vec<&str> = receipt["restored"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["recreate", "recreate", "delete"]);
    assert_eq!(read(root, "c.txt"), "c original\n");
    assert_eq!(read(root, "d.txt"), "d original\n");
    assert!(!root.join("renamed.txt").exists());
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

fn change_kinds(receipt: &Value) -> Vec<(String, String)> {
    receipt["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("changes missing in {receipt}"))
        .iter()
        .map(|c| {
            (
                c["path"].as_str().unwrap().into(),
                c["kind"].as_str().unwrap().into(),
            )
        })
        .collect()
}

/// The scan reads only what git lists, so a checkout that rewrites clean
/// files is seen through the HEAD change, not through a full-tree walk.
#[test]
fn a_checkout_that_rewrites_clean_files_is_attributed_and_restored() {
    let dir = repo();
    let root = dir.path();
    git(root, &["checkout", "-q", "-b", "other"]);
    fs::write(root.join("a.txt"), "a on other\n").unwrap();
    git(root, &["commit", "-q", "-am", "other"]);
    git(root, &["checkout", "-q", "-"]);
    let s = Session::new(root);
    s.capture("turn-1");

    let attributed = s.call_with("shell-1", Some("process_exec"), || {
        git(root, &["checkout", "-q", "other"]);
    });
    assert_eq!(
        change_kinds(&attributed),
        [("a.txt".into(), "modified".into())]
    );
    s.restore("turn-1");
    assert_eq!(read(root, "a.txt"), "a original\n");
}

/// A dirty file put back to HEAD leaves git's list; committing a dirty file
/// leaves it too but changes nothing on disk.
#[test]
fn leaving_the_dirty_list_is_a_change_only_when_the_file_changed() {
    let dir = repo();
    let root = dir.path();
    fs::write(root.join("a.txt"), "a uncommitted work\n").unwrap();
    fs::write(root.join("b.txt"), "b ready to commit\n").unwrap();
    let s = Session::new(root);
    s.capture("turn-1");

    let attributed = s.call_with("shell-1", Some("process_exec"), || {
        git(root, &["checkout", "--", "a.txt"]);
        git(root, &["commit", "-q", "-m", "b", "b.txt"]);
    });
    assert_eq!(
        change_kinds(&attributed),
        [("a.txt".into(), "modified".into())]
    );
    s.restore("turn-1");
    assert_eq!(read(root, "a.txt"), "a uncommitted work\n");
    assert_eq!(read(root, "b.txt"), "b ready to commit\n");
}

/// Per-call cost follows what git lists, not the size of the tree.
#[test]
fn a_call_scan_stats_only_the_files_git_lists() {
    let dir = repo();
    let root = dir.path();
    fs::create_dir_all(root.join("clean")).unwrap();
    for n in 0..200 {
        fs::write(root.join(format!("clean/{n}.txt")), n.to_string()).unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "clean files"]);
    fs::write(root.join("a.txt"), "a dirty\n").unwrap();
    let s = Session::new(root);
    let captured = s.capture("turn-1");
    assert_eq!(captured["scanned_files"], 1, "{captured}");
    let begin = s.call("call_begin", vec![("call_id", vm_string("tool-1"))]);
    assert_eq!(begin["scanned_files"], 1, "{begin}");
    fs::write(root.join("clean/7.txt"), "edited").unwrap();
    let end = s.call("call_end", vec![("call_id", vm_string("tool-1"))]);
    assert_eq!(
        change_kinds(&end),
        [("clean/7.txt".into(), "modified".into())]
    );
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
    s.call_with("push-1", Some("network"), || {});
    let receipt = s.restore("turn-1");
    let effects = receipt["irreversible"].as_array().expect("irreversible");
    assert_eq!(effects.len(), 2, "{receipt}");
    for effect in effects {
        assert_eq!(effect["call_id"], "push-1");
        assert_eq!(effect["side_effect"], "network");
    }
    assert_ne!(
        effects[0]["invocation"], effects[1]["invocation"],
        "a reused call id is still two effects: {receipt}"
    );
    assert!(effects[0]["invocation"].is_u64());
}

#[test]
fn a_fork_never_reuses_an_inherited_invocation() {
    let dir = repo();
    let root = dir.path();
    let source = Session::new(root);
    let fork = Session::new(root);
    source.capture("turn-1");
    source.call_with("net", Some("network"), || {});
    let source_plan = source.plan("turn-1");
    let forked = serde_json::to_value(
        harn_hostlib::workspace_checkpoint::fork_session(&source.id, &fork.id, root).unwrap(),
    )
    .unwrap();
    assert_eq!(forked["available"], true, "{forked}");
    fork.capture("turn-2");
    fork.call_with("net", Some("network"), || {});

    let plan = fork.plan("turn-1");
    let effects = plan["irreversible"].as_array().expect("irreversible");
    assert_eq!(effects.len(), 2, "{plan}");
    assert_ne!(
        effects[0]["invocation"], effects[1]["invocation"],
        "the fork's call reused an inherited invocation: {plan}"
    );
    let receipt = fork.restore("turn-1");
    assert_eq!(receipt["irreversible"], plan["irreversible"], "{receipt}");
    assert_eq!(
        source.plan("turn-1"),
        source_plan,
        "the fork's history leaks into its source"
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

#[test]
fn partial_restore_keeps_attribution_for_paths_it_did_not_restore() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
        fs::write(root.join("c.txt"), "c edited by the agent\n").unwrap();
    });
    let first = s.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("paths", vlist_str(&["a.txt"])),
        ],
    );
    assert_eq!(paths(&first, "restored"), ["a.txt"]);
    assert_eq!(read(root, "c.txt"), "c edited by the agent\n");

    let second = s.restore("turn-1");
    assert_eq!(paths(&second, "restored"), ["c.txt"], "{second}");
    assert_eq!(read(root, "c.txt"), "c original\n");
}

#[test]
fn an_empty_path_selection_is_rejected_not_widened_to_everything() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
    });
    let reg = registry();
    let err = try_call(
        &reg,
        "hostlib_fs_checkpoint_restore",
        dict(&[
            ("session_id", vm_string(&s.id)),
            ("boundary_id", vm_string("turn-1")),
            ("paths", vlist_str(&[])),
        ]),
    );
    assert!(err.is_err());
    assert_eq!(read(root, "a.txt"), "a edited by the agent\n");
}

#[test]
fn a_refused_path_keeps_its_attribution_after_the_rest_is_restored() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
        fs::write(root.join("c.txt"), "c edited by the agent\n").unwrap();
    });
    fs::write(root.join("a.txt"), "a overwritten by a person\n").unwrap();
    let first = s.restore("turn-1");
    assert_eq!(paths(&first, "refused"), ["a.txt"]);
    let second = s.restore("turn-1");
    assert_eq!(paths(&second, "refused"), ["a.txt"], "{second}");
}

#[test]
fn second_capture_does_not_store_the_checkpoint_directory() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    fs::write(root.join("scratch.txt"), "untracked").unwrap();
    assert_eq!(s.capture("turn-1")["stored_files"], 1);
    let second = s.capture("turn-2");
    assert_eq!(second["stored_files"], 1, "{second}");
}

#[test]
fn session_store_state_is_not_attributed_but_user_harn_files_are() {
    let dir = repo();
    let root = dir.path();
    let skills = root.join(".harn/skills");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join("custom.md"), "before").unwrap();
    fs::create_dir_all(root.join(".harn/personas")).unwrap();
    fs::write(root.join(".harn/personas/custom.md"), "before").unwrap();
    fs::write(root.join(".harn/session-store.sqlite"), "initial runtime").unwrap();
    let session = Session::new(root);
    let capture = session.capture("turn-1");
    assert_eq!(capture["available"], true);
    assert_eq!(capture["stored_files"], 2, "{capture}");
    let attributed = session.call_with("tool-1", Some("workspace_write"), || {
        fs::create_dir_all(root.join(".harn/agent-run-writers")).unwrap();
        fs::create_dir_all(root.join(".harn/fs-cas-locks")).unwrap();
        fs::create_dir_all(root.join(".harn/hostlib")).unwrap();
        for path in [
            ".harn/session-store.sqlite",
            ".harn/session-store.sqlite-wal",
            ".harn/session-store.sqlite-shm",
            ".harn/session-store.sqlite-journal",
            ".harn/session-store.sqlite.harn-init.lock",
            ".harn/agent-run-writers/project.lock",
            ".harn/fs-cas-locks/file.lock",
            ".harn/hostlib/scanner-snapshot.json",
            ".harn/workers/worker.json",
            ".harn/receipts/receipt.json",
            ".harn/hostlib-tests/output",
            ".harn/orchestrator/state.json",
            ".harn/skills-cache/pkg/file",
            ".harn/flow.sqlite-wal",
            ".harn/personas/activations.json",
            ".harn/personas/activations.lock",
        ] {
            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            fs::write(root.join(path), "runtime").unwrap();
        }
        fs::write(skills.join("custom.md"), "agent edit").unwrap();
        fs::write(root.join(".harn/personas/custom.md"), "agent edit").unwrap();
    });
    assert_eq!(
        paths(&attributed, "changes"),
        [".harn/personas/custom.md", ".harn/skills/custom.md"]
    );
    let receipt = session.restore("turn-1");
    assert_eq!(
        paths(&receipt, "restored"),
        [".harn/personas/custom.md", ".harn/skills/custom.md"]
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/personas/custom.md")).unwrap(),
        "before"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/flow.sqlite-wal")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/personas/activations.lock")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(skills.join("custom.md")).unwrap(),
        "before"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/session-store.sqlite")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/agent-run-writers/project.lock")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/fs-cas-locks/file.lock")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(root.join(".harn/hostlib/scanner-snapshot.json")).unwrap(),
        "runtime"
    );
}

#[test]
fn nested_source_relative_runtime_state_is_not_an_edit() {
    let dir = repo();
    let root = dir.path();
    let nested = root.join("src/.harn");
    fs::create_dir_all(nested.join("skills")).unwrap();
    fs::write(nested.join("skills/custom.md"), "before").unwrap();
    let session = Session::new(root);
    assert_eq!(session.capture("turn-1")["available"], true);
    let attributed = session.call_with("tool-1", None, || {
        fs::create_dir_all(nested.join("memory")).unwrap();
        fs::write(nested.join("memory/events.jsonl"), "runtime").unwrap();
        fs::write(nested.join("skills/custom.md"), "agent edit").unwrap();
    });
    assert_eq!(
        paths(&attributed, "changes"),
        ["src/.harn/skills/custom.md"]
    );
    assert_eq!(
        paths(&session.restore("turn-1"), "restored"),
        ["src/.harn/skills/custom.md"]
    );
    assert_eq!(
        fs::read_to_string(nested.join("memory/events.jsonl")).unwrap(),
        "runtime"
    );
    assert_eq!(
        fs::read_to_string(nested.join("skills/custom.md")).unwrap(),
        "before"
    );
}

#[test]
fn capture_with_a_different_root_is_rejected() {
    let dir = repo();
    let other = repo();
    let s = Session::new(dir.path());
    s.capture("turn-1");
    let reg = registry();
    let err = try_call(
        &reg,
        "hostlib_fs_checkpoint_capture",
        dict(&[
            ("session_id", vm_string(&s.id)),
            ("boundary_id", vm_string("turn-2")),
            ("root", vm_string(&path_str(other.path()))),
        ]),
    );
    assert!(err.is_err());
}

#[test]
fn root_below_the_git_toplevel_is_checkpointed() {
    let dir = repo();
    let sub = dir.path().join("pkg");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("lib.txt"), "lib original\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "pkg"]);

    let s = Session::new(&sub);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::write(sub.join("lib.txt"), "lib edited by the agent\n").unwrap();
        fs::write(dir.path().join("a.txt"), "outside the root\n").unwrap();
    });
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "restored"), ["lib.txt"]);
    assert_eq!(read(&sub, "lib.txt"), "lib original\n");
    assert_eq!(read(dir.path(), "a.txt"), "outside the root\n");
}

#[cfg(unix)]
mod unix_permissions {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn running_as_root() -> bool {
        let probe = TempDir::new().unwrap();
        let locked = probe.path().join("locked");
        fs::create_dir(&locked).unwrap();
        chmod(&locked, 0o000);
        let readable = fs::read_dir(&locked).is_ok();
        chmod(&locked, 0o755);
        readable
    }

    #[test]
    fn directory_unreadable_at_call_begin_makes_restore_unavailable() {
        if running_as_root() {
            return;
        }
        let dir = repo();
        let root = dir.path();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/person.txt"), "person work\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "locked"]);
        let s = Session::new(root);
        s.capture("turn-1");

        chmod(&root.join("locked"), 0o000);
        let attributed = s.call_with("tool-1", None, || {
            chmod(&root.join("locked"), 0o755);
            fs::write(root.join("locked/person.txt"), "edited later\n").unwrap();
        });
        let change = &attributed["changes"][0];
        assert_eq!(change["path"], "locked/person.txt", "{attributed}");
        assert_eq!(change["incomplete"], true);

        let receipt = s.restore("turn-1");
        assert_eq!(receipt["available"], false, "{receipt}");
        assert_eq!(receipt["reason"], "incomplete_attribution");
        assert_eq!(read(root, "locked/person.txt"), "edited later\n");
    }

    #[test]
    fn unreadable_directory_in_both_scans_cannot_look_like_an_empty_restore() {
        if running_as_root() {
            return;
        }
        let dir = repo();
        let root = dir.path();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/person.txt"), "original\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "locked"]);
        let s = Session::new(root);
        assert_eq!(s.capture("turn-1")["available"], true);

        chmod(&root.join("locked"), 0o000);
        let attributed = s.call_with("tool-1", None, || {
            chmod(&root.join("locked"), 0o755);
            fs::write(root.join("locked/person.txt"), "edited while unseen\n").unwrap();
            chmod(&root.join("locked"), 0o000);
        });
        chmod(&root.join("locked"), 0o755);
        assert!(
            attributed["scan_errors"].as_u64().unwrap() > 0,
            "{attributed}"
        );
        assert!(
            attributed["changes"].as_array().unwrap().is_empty(),
            "{attributed}"
        );
        for receipt in [s.plan("turn-1"), s.restore("turn-1")] {
            assert_eq!(receipt["available"], false, "{receipt}");
            assert_eq!(receipt["reason"], "incomplete_attribution");
            assert!(receipt["detail"].as_str().unwrap().contains("tool-1"));
        }
        assert_eq!(read(root, "locked/person.txt"), "edited while unseen\n");
    }

    #[test]
    fn incomplete_initial_scan_cannot_create_a_checkpoint() {
        if running_as_root() {
            return;
        }
        let dir = repo();
        let root = dir.path();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/person.txt"), "original\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "locked"]);
        let s = Session::new(root);
        chmod(&root.join("locked"), 0o000);
        let capture = s.capture("turn-1");
        chmod(&root.join("locked"), 0o755);
        assert_eq!(capture["available"], false, "{capture}");
        assert_eq!(capture["reason"], "incomplete_capture");
        assert_eq!(s.plan("turn-1")["reason"], "no_checkpoint");
    }

    #[test]
    fn a_write_failure_is_reported_and_the_rest_of_the_restore_still_runs() {
        if running_as_root() {
            return;
        }
        let dir = repo();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/f.txt"), "f original\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "sub"]);
        let s = Session::new(root);
        s.capture("turn-1");
        s.call_with("tool-1", None, || {
            fs::remove_file(root.join("sub/f.txt")).unwrap();
            fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
        });
        chmod(&root.join("sub"), 0o555);
        let receipt = s.restore("turn-1");
        chmod(&root.join("sub"), 0o755);
        assert_eq!(paths(&receipt, "failed"), ["sub/f.txt"], "{receipt}");
        assert_eq!(paths(&receipt, "restored"), ["a.txt"]);
        assert_eq!(read(root, "a.txt"), "a original\n");
    }
}

/// The documented blind spot: a person's edit inside a call's window is
/// attributed to that call, so restore reverts it. The guard still refuses an
/// edit that lands after the call.
#[test]
fn writer_inside_a_call_window_is_attributed_to_the_call_but_later_edits_are_refused() {
    let dir = repo();
    let root = dir.path();
    let s = Session::new(root);
    s.capture("turn-1");
    let attributed = s.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by the agent\n").unwrap();
        fs::write(root.join("a.txt"), "a edited by a person, mid-call\n").unwrap();
    });
    assert_eq!(attributed["changes"][0]["path"], "a.txt");
    assert_eq!(attributed["changes"][0]["kind"], "modified");

    fs::write(root.join("a.txt"), "a edited after the call\n").unwrap();
    let receipt = s.restore("turn-1");
    assert_eq!(paths(&receipt, "refused"), ["a.txt"]);
    assert_eq!(read(root, "a.txt"), "a edited after the call\n");
}

#[test]
fn unknown_writer_inside_a_call_window_is_not_overwritten() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["available"], true, "{capture}");
    session.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "a edited by a person, mid-call\n").unwrap();
    });

    let plan = session.plan("turn-1");
    let receipt = session.restore("turn-1");
    assert_eq!(paths(&receipt, "refused"), ["a.txt"], "{receipt}");
    assert_eq!(read(root, "a.txt"), "a edited by a person, mid-call\n");
    assert_eq!(
        plan["entries"][0]["writers"][0]["kind"], "unknown",
        "{plan}"
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
}

#[test]
fn strict_writer_policy_cannot_be_downgraded_and_survives_fork() {
    use harn_hostlib::workspace_checkpoint::{
        capture_with_writer_policy, fork_session, WriterPolicy,
    };
    let dir = repo();
    let root = dir.path();
    let source = Session::new(root);
    let target = Session::new(root);
    let capture = source.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&source.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    source.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "unknown writer\n").unwrap();
    });
    assert!(capture_with_writer_policy(
        &source.id,
        "turn-2",
        Some(root),
        Some(WriterPolicy::LegacyScan),
    )
    .is_err());
    assert_eq!(source.capture("turn-2")["writer_policy"], "require_known");
    let fork = serde_json::to_value(fork_session(&source.id, &target.id, root).unwrap()).unwrap();
    assert_eq!(fork["available"], true, "{fork}");
    let receipt = target.restore("turn-1");
    assert_eq!(paths(&receipt, "refused"), ["a.txt"], "{receipt}");
    assert_eq!(read(root, "a.txt"), "unknown writer\n");
    let legacy = Session::new(root);
    assert_eq!(legacy.capture("legacy")["writer_policy"], "legacy_scan");
    assert!(capture_with_writer_policy(
        &legacy.id,
        "later",
        Some(root),
        Some(WriterPolicy::RequireKnown),
    )
    .is_err());
}

#[test]
fn strict_plan_keeps_writer_evidence_across_multiple_calls() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    session.call_with("first", None, || {
        fs::write(root.join("a.txt"), "first unknown edit\n").unwrap();
    });
    session.call_with("second", None, || {
        fs::write(root.join("a.txt"), "second unknown edit\n").unwrap();
    });
    let plan = session.plan("turn-1");
    assert_eq!(
        plan["entries"][0]["writers"],
        json!([{ "kind": "unknown" }])
    );
    assert_eq!(plan["entries"][0]["action"], "refuse", "{plan}");
    assert_eq!(paths(&session.restore("turn-1"), "refused"), ["a.txt"]);
    assert_eq!(read(root, "a.txt"), "second unknown edit\n");
}

#[test]
fn writer_confirmation_is_bound_to_the_observed_file_state() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    session.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "first unknown edit\n").unwrap();
    });
    let first = session.plan("turn-1");
    let observed = first["entries"][0]["observed"].clone();
    assert_eq!(observed["kind"], "present", "{first}");
    fs::write(root.join("a.txt"), "newer unknown edit\n").unwrap();
    let stale = session.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            (
                "confirmed_paths",
                harn_vm::json_to_vm_value(&json!([{
                    "session_id": session.id.as_str(),
                    "boundary_id": "turn-1",
                    "path": "a.txt",
                    "observed": observed,
                }])),
            ),
        ],
    );
    assert_eq!(paths(&stale, "refused"), ["a.txt"], "{stale}");
    assert_eq!(read(root, "a.txt"), "newer unknown edit\n");
    let latest = session.plan("turn-1");
    let confirmed = session.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            (
                "confirmed_paths",
                harn_vm::json_to_vm_value(&json!([{
                    "session_id": session.id.as_str(),
                    "boundary_id": "turn-1",
                    "path": "a.txt",
                    "observed": latest["entries"][0]["observed"],
                }])),
            ),
        ],
    );
    assert_eq!(paths(&confirmed, "refused"), ["a.txt"], "{confirmed}");
    assert_eq!(
        confirmed["refused"][0]["reason"],
        "concurrent-safe replacement is unavailable"
    );
    assert_eq!(read(root, "a.txt"), "newer unknown edit\n");
}

#[cfg(unix)]
#[test]
fn confirmed_recreation_only_publishes_into_an_absent_path() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    session.call_with("tool-1", None, || {
        fs::remove_file(root.join("a.txt")).unwrap();
    });
    let plan = session.plan("turn-1");
    assert_eq!(plan["entries"][0]["observed"]["kind"], "absent", "{plan}");
    let receipt = session.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            (
                "confirmed_paths",
                harn_vm::json_to_vm_value(&json!([{
                    "session_id": session.id.as_str(),
                    "boundary_id": "turn-1",
                    "path": "a.txt",
                    "observed": { "kind": "absent" },
                }])),
            ),
        ],
    );
    assert_eq!(paths(&receipt, "restored"), ["a.txt"], "{receipt}");
    assert_eq!(read(root, "a.txt"), "a original\n");
}

#[test]
fn confirmation_of_an_absent_file_does_not_authorize_a_new_file() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    session.call_with("tool-1", None, || {
        fs::remove_file(root.join("a.txt")).unwrap();
    });
    let plan = session.plan("turn-1");
    assert_eq!(plan["entries"][0]["observed"]["kind"], "absent", "{plan}");
    fs::write(root.join("a.txt"), "new file after confirmation\n").unwrap();
    let receipt = session.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            (
                "confirmed_paths",
                harn_vm::json_to_vm_value(&json!([{
                    "session_id": session.id.as_str(),
                    "boundary_id": "turn-1",
                    "path": "a.txt",
                    "observed": { "kind": "absent" },
                }])),
            ),
        ],
    );
    assert_eq!(paths(&receipt, "refused"), ["a.txt"], "{receipt}");
    assert_eq!(read(root, "a.txt"), "new file after confirmation\n");
}

#[test]
fn strict_restore_does_not_delete_an_existing_file_even_after_confirmation() {
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    let capture = session.call(
        "capture",
        vec![
            ("boundary_id", vm_string("turn-1")),
            ("root", vm_string(&session.root)),
            ("writer_policy", vm_string("require_known")),
        ],
    );
    assert_eq!(capture["writer_policy"], "require_known", "{capture}");
    session.call_with("tool-1", None, || {
        fs::write(root.join("new.txt"), "new content\n").unwrap();
    });
    let plan = session.plan("turn-1");
    let receipt = session.call(
        "restore",
        vec![
            ("boundary_id", vm_string("turn-1")),
            (
                "confirmed_paths",
                harn_vm::json_to_vm_value(&json!([{
                    "session_id": session.id.as_str(),
                    "boundary_id": "turn-1",
                    "path": "new.txt",
                    "observed": plan["entries"][0]["observed"],
                }])),
            ),
        ],
    );
    assert_eq!(paths(&receipt, "refused"), ["new.txt"], "{receipt}");
    assert_eq!(
        receipt["refused"][0]["reason"],
        "concurrent-safe replacement is unavailable"
    );
    assert_eq!(read(root, "new.txt"), "new content\n");
}

#[test]
fn malformed_or_duplicate_confirmations_do_not_restore() {
    use harn_hostlib::workspace_checkpoint::{
        restore_with_confirmations, ConfirmedPath, ObservedFileState,
    };
    let dir = repo();
    let root = dir.path();
    let session = Session::new(root);
    session.capture("turn-1");
    session.call_with("tool-1", None, || {
        fs::write(root.join("a.txt"), "do not overwrite\n").unwrap();
    });
    let malformed = ConfirmedPath {
        session_id: session.id.clone(),
        boundary_id: "turn-1".into(),
        path: "a.txt".into(),
        observed: ObservedFileState::Present {
            sha256: "not-a-hash".into(),
        },
    };
    assert!(restore_with_confirmations(&session.id, "turn-1", None, &[malformed]).is_err());
    let duplicate = ConfirmedPath {
        session_id: session.id.clone(),
        boundary_id: "turn-1".into(),
        path: "a.txt".into(),
        observed: ObservedFileState::Absent,
    };
    assert!(restore_with_confirmations(
        &session.id,
        "turn-1",
        None,
        &[duplicate.clone(), duplicate.clone()],
    )
    .is_err());
    let wrong_boundary = ConfirmedPath {
        boundary_id: "another-turn".into(),
        ..duplicate
    };
    assert!(restore_with_confirmations(&session.id, "turn-1", None, &[wrong_boundary]).is_err());
    assert_eq!(read(root, "a.txt"), "do not overwrite\n");
}

#[cfg(unix)]
#[test]
fn restore_never_writes_through_a_symlinked_ancestor() {
    let dir = repo();
    let root = dir.path();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/f.txt"), "f original\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "sub"]);
    let s = Session::new(root);
    s.capture("turn-1");
    s.call_with("tool-1", None, || {
        fs::remove_file(root.join("sub/f.txt")).unwrap();
    });

    // The directory is swapped for a symlink to somewhere outside the worktree.
    let outside = TempDir::new().unwrap();
    fs::remove_dir(root.join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("sub")).unwrap();

    let receipt = s.restore("turn-1");
    assert!(paths(&receipt, "restored").is_empty(), "{receipt}");
    assert_eq!(paths(&receipt, "failed"), ["sub/f.txt"], "{receipt}");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
