//! A read denial inside a writable root holds against renames (harn#9788).
//!
//! A `file-read*` deny is path-based, so a child that can write the root could
//! rename the denied file, or a directory above it, and read it under the new
//! name. The profile also denies unlink, which covers rename, on the denied
//! subtree and on each directory above it. The control strips those rules from
//! the same rendered profile and shows each rename then leaks.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

const SECRET: &str = "synthetic-denied-secret";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    denied: PathBuf,
}

/// `<workspace>/a/dir/secret.json`, so the file, its directory, and the
/// directory above that can each be renamed.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("fixture root");
    let workspace = std::fs::canonicalize(root.path())
        .expect("canonical root")
        .join("workspace");
    let denied = workspace.join("a/dir/secret.json");
    std::fs::create_dir_all(denied.parent().unwrap()).expect("denied parent");
    std::fs::write(&denied, SECRET).expect("denied file");
    std::fs::write(workspace.join("a.txt"), "a").expect("source file");
    Fixture {
        _root: root,
        workspace,
        denied,
    }
}

fn profile(fixture: &Fixture) -> String {
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![fixture.workspace.display().to_string()],
        process_sandbox: Box::new(crate::orchestration::ProcessSandboxPolicy {
            read_deny_roots: vec![fixture.denied.display().to_string()],
            ..Default::default()
        }),
        ..CapabilityPolicy::default()
    };
    render_profile(&policy)
}

fn run(profile: &str, workspace: &Path, script: &str) -> String {
    let output = Command::new(SANDBOX_EXEC_PATH)
        .args(["-p", profile, "--", "/bin/sh", "-c", script])
        .current_dir(workspace)
        .output()
        .expect("run sandbox-exec");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Each script moves something on the path to the secret, then reads it under
/// the new name.
const RENAMES: &[(&str, &str)] = &[
    ("the file", "mv a/dir/secret.json moved; cat moved"),
    ("its directory", "mv a/dir moved; cat moved/secret.json"),
    (
        "the directory above",
        "mv a moved; cat moved/dir/secret.json",
    ),
];

#[test]
fn a_denied_file_inside_a_writable_root_cannot_be_renamed_out_of_its_denial() {
    let fixture = fixture();
    let profile = profile(&fixture);
    for (moved, script) in RENAMES {
        let stdout = run(&profile, &fixture.workspace, script);
        assert!(
            !stdout.contains(SECRET),
            "renaming {moved} exposed the denied file"
        );
        assert!(
            fixture.denied.exists(),
            "renaming {moved} moved the denied file"
        );
    }

    // The writable root stays writable around the denial.
    let stdout = run(
        &profile,
        &fixture.workspace,
        "cp a.txt b.txt && touch a/dir/created && rm a/dir/created && echo created",
    );
    assert_eq!(stdout.trim(), "created");
}

/// The control: the same profile without its unlink rules, which is the read
/// deny alone, lets every rename above reach the secret.
#[test]
fn without_the_unlink_rules_each_rename_reaches_the_denied_file() {
    for (moved, script) in RENAMES {
        let fixture = fixture();
        let read_deny_only = profile(&fixture)
            .lines()
            .filter(|line| !line.contains("file-write-unlink"))
            .collect::<Vec<_>>()
            .join("\n");
        let stdout = run(&read_deny_only, &fixture.workspace, script);
        assert!(
            stdout.contains(SECRET),
            "renaming {moved} did not reach the secret even without the unlink rules, so \
             the test above proves nothing"
        );
    }
}
