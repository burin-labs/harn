//! Every consumer of credential-path knowledge must recognize every credential
//! the `known_paths` registry names, and none may flag an ordinary path.
//!
//! Each consumer is reached through the entry point its callers use.

use std::path::Path;

use crate::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    ToolApprovalPolicy,
};
use crate::stdlib::sandbox::{check_fs_path_scope, FsAccess};

struct ReadPolicyScope;

impl ReadPolicyScope {
    fn new(home: &Path, candidate: &Path) -> Self {
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            // Grant the candidate too: a credential symlink may resolve outside home.
            read_only_roots: vec![home.display().to_string(), candidate.display().to_string()],
            ..CapabilityPolicy::default()
        });
        Self
    }
}

impl Drop for ReadPolicyScope {
    fn drop(&mut self) {
        pop_execution_policy();
    }
}

/// Which consumers flag `path`; the sandbox only judges home paths.
fn consumer_misses(path: &Path, home: Option<&Path>) -> Vec<&'static str> {
    let text = path.display().to_string();
    let mut misses = Vec::new();
    if let Some(home) = home {
        let _scope = ReadPolicyScope::new(home, path);
        if check_fs_path_scope(path, FsAccess::Read).is_ok() {
            misses.push("sandbox denylist");
        }
    }
    if !crate::security::args_reference_secret(&serde_json::json!({"path": text})) {
        misses.push("taint secret-read gate");
    }
    let scan = crate::orchestration::command_risk_scan_json(
        &serde_json::json!({"request": {"command": format!("cat '{text}'")}}),
        None,
    );
    if !scan["risk_labels"]
        .as_array()
        .is_some_and(|labels| labels.iter().any(|label| label == "credential_file_read"))
    {
        misses.push("command risk scan");
    }
    if crate::orchestration::first_sensitive_path(&ToolApprovalPolicy::default(), &[text]).is_none()
    {
        misses.push("approval sensitive paths");
    }
    misses
}

/// A file name that each basename glob accepts.
fn example_name(glob: &str) -> String {
    glob.replace('*', "sample")
}

#[test]
fn every_registry_credential_trips_every_consumer() {
    let home = crate::stdlib::sandbox::sandbox_user_home_dir().expect("an absolute home");
    let mut drift = Vec::new();
    for entry in crate::known_paths::credential_home_paths() {
        let misses = consumer_misses(&home.join(entry), Some(&home));
        if !misses.is_empty() {
            drift.push(format!("~/{entry}: missed by {}", misses.join(", ")));
        }
    }
    for glob in crate::known_paths::credential_basename_globs() {
        let path = Path::new("/work/project/config").join(example_name(glob));
        let misses = consumer_misses(&path, None);
        if !misses.is_empty() {
            drift.push(format!(
                "{}: missed by {}",
                path.display(),
                misses.join(", ")
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "credential consumers disagree:\n{}",
        drift.join("\n")
    );
}

#[test]
fn ordinary_paths_trip_no_consumer() {
    let home = crate::stdlib::sandbox::sandbox_user_home_dir().expect("an absolute home");
    for relative in [
        ".config/git/config",
        ".docker/daemon.json",
        ".cargo/config.toml",
    ] {
        let misses = consumer_misses(&home.join(relative), Some(&home));
        assert_eq!(
            misses.len(),
            4,
            "~/{relative} was flagged by a consumer: {misses:?}"
        );
    }
    for path in ["/work/project/src/main.rs", "/work/project/.environment.ts"] {
        let misses = consumer_misses(Path::new(path), None);
        assert_eq!(
            misses.len(),
            3,
            "{path} was flagged by a consumer: {misses:?}"
        );
    }
}
