//! Every consumer of credential-path knowledge must recognize every credential
//! the `known_paths` registry names, and none may flag an ordinary path.
//!
//! Each consumer is reached through the entry point its callers use.

use std::path::{Path, PathBuf};

use crate::orchestration::ToolApprovalPolicy;

struct Consumers {
    denied: Vec<PathBuf>,
}

impl Consumers {
    fn new() -> Self {
        Self {
            denied: crate::stdlib::sandbox::process_sandbox_read_deny_roots(
                &crate::orchestration::CapabilityPolicy::default(),
            ),
        }
    }

    /// Which consumers flag `path`; the sandbox only judges home paths.
    fn misses(&self, path: &Path, home_path: bool) -> Vec<&'static str> {
        let text = path.display().to_string();
        let mut misses = Vec::new();
        if home_path && !crate::stdlib::sandbox::path_is_denied(path, &self.denied) {
            misses.push("sandbox denylist");
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
        if crate::orchestration::first_sensitive_path(&ToolApprovalPolicy::default(), &[text])
            .is_none()
        {
            misses.push("approval sensitive paths");
        }
        misses
    }
}

/// A file name that each basename glob accepts.
fn example_name(glob: &str) -> String {
    glob.replace('*', "sample")
}

#[test]
fn every_registry_credential_trips_every_consumer() {
    let home = crate::stdlib::sandbox::sandbox_user_home_dir().expect("an absolute home");
    let consumers = Consumers::new();
    let mut drift = Vec::new();
    for entry in crate::known_paths::credential_home_paths() {
        let misses = consumers.misses(&home.join(entry), true);
        if !misses.is_empty() {
            drift.push(format!("~/{entry}: missed by {}", misses.join(", ")));
        }
    }
    for glob in crate::known_paths::credential_basename_globs() {
        let path = Path::new("/work/project/config").join(example_name(glob));
        let misses = consumers.misses(&path, false);
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
    let consumers = Consumers::new();
    for relative in [
        ".config/git/config",
        ".docker/daemon.json",
        ".cargo/config.toml",
    ] {
        let misses = consumers.misses(&home.join(relative), true);
        assert_eq!(
            misses.len(),
            4,
            "~/{relative} was flagged by a consumer: {misses:?}"
        );
    }
    for path in ["/work/project/src/main.rs", "/work/project/.environment.ts"] {
        let misses = consumers.misses(Path::new(path), false);
        assert_eq!(
            misses.len(),
            3,
            "{path} was flagged by a consumer: {misses:?}"
        );
    }
}
