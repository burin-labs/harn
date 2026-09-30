//! Every default denylist entry is refused to a live confined child, even
//! though `PackageManagerConfig` grants the directory that holds it.
//!
//! The substitution tests in `linux_tests.rs` check which paths the backend
//! decided to grant. This checks the claim a reader of
//! `read_deny_defaults.toml` relies on: a file planted at each entry cannot be
//! read. A new entry is covered the moment it lands in the data file, and an
//! entry the backend fails to subtract fails here by name.

use super::tests::live_landlock_available;
use crate::orchestration::{
    CapabilityPolicy, ProcessSandboxPolicy, ProcessSandboxPreset, SandboxProfile,
};
use crate::stdlib::sandbox::{command_output, ProcessCommandConfig};

/// Concrete credential files at CLI default locations under the preset-granted
/// directories. Each must be refused whatever shape its denylist entry takes,
/// a file or a directory above it, so a narrowed or dropped entry fails here
/// by name rather than by the entry disappearing from the loop below.
const KNOWN_CLI_CREDENTIALS: &[&str] = &[
    ".config/gh/hosts.yml",
    ".config/gcloud/credentials.db",
    ".config/glab-cli/config.yml",
    ".config/hub",
    ".config/github-copilot/apps.json",
    ".config/rclone/rclone.conf",
    ".config/doctl/config.yaml",
    ".config/hcloud/cli.toml",
    ".config/containers/auth.json",
    ".config/.wrangler/config/default.toml",
    ".config/stripe/config.toml",
    ".config/sops/age/keys.txt",
    ".config/configstore/firebase-tools.json",
    ".config/git/credentials",
    ".cache/huggingface/token",
];

/// Whether a confined child can `cat` `target` under `policy`.
fn reads_under(policy: &CapabilityPolicy, target: &std::path::Path) -> bool {
    crate::orchestration::push_execution_policy(policy.clone());
    let output = command_output(
        "/bin/cat",
        &[target.display().to_string()],
        &ProcessCommandConfig::default(),
    );
    crate::orchestration::pop_execution_policy();
    matches!(output, Ok(out) if out.status.success())
}

/// # The defect
///
/// The preset opens all of `~/.config` and `~/.cache`, and the denylist named
/// only three credential locations under them. The GitLab, DigitalOcean,
/// Hetzner and rclone CLIs, among others, keep tokens at their default paths
/// there, and a confined child read each one.
///
/// # The legs
///
/// * control: an ordinary file under `~/.config` and one under `~/.cache` are
///   readable, so a refusal cannot be explained by an absent grant;
/// * claim: a file planted at every default entry, and at every known CLI
///   credential location, is refused.
#[test]
fn a_confined_child_is_refused_every_default_denylist_entry() {
    if !live_landlock_available("credential-denylist") {
        return;
    }
    let _env_lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let home = tempfile::TempDir::new().expect("temp home");
    let home_path = home.path().canonicalize().expect("canonical home");
    let workspace = tempfile::TempDir::new().expect("workspace");
    let workspace_path = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");

    let mut targets: Vec<String> = KNOWN_CLI_CREDENTIALS
        .iter()
        .map(|relative| (*relative).to_string())
        .collect();
    for entry in crate::orchestration::default_read_deny_home_paths() {
        // An entry that is a directory above a known file is covered by the
        // file; planting a file at the directory's own path would collide.
        if !targets
            .iter()
            .any(|target| target == entry || target.starts_with(&format!("{entry}/")))
        {
            targets.push(entry.clone());
        }
    }
    for relative in &targets {
        let path = home_path.join(relative);
        std::fs::create_dir_all(path.parent().expect("entry parent")).expect("entry dir");
        std::fs::write(&path, "NOT-A-REAL-CREDENTIAL\n").expect("plant entry");
    }
    let controls = [
        home_path.join(".config/some-tool/settings.json"),
        home_path.join(".cache/some-tool/data.json"),
    ];
    for control in &controls {
        std::fs::create_dir_all(control.parent().expect("control parent")).expect("control dir");
        std::fs::write(control, "READABLE\n").expect("plant control");
    }

    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home_path);
    let policy = CapabilityPolicy {
        workspace_roots: vec![workspace_path.display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: Box::new(ProcessSandboxPolicy {
            presets: Some(vec![ProcessSandboxPreset::PackageManagerConfig]),
            ..Default::default()
        }),
        ..CapabilityPolicy::default()
    };
    let unreadable_controls: Vec<_> = controls
        .iter()
        .filter(|control| !reads_under(&policy, control))
        .collect();
    let read_entries: Vec<_> = targets
        .iter()
        .filter(|relative| reads_under(&policy, &home_path.join(relative)))
        .collect();
    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert!(
        unreadable_controls.is_empty(),
        "the preset must grant these, or every refusal below proves nothing: \
         {unreadable_controls:?}"
    );
    assert!(
        read_entries.is_empty(),
        "a confined child read these credential files: {read_entries:?}"
    );
    eprintln!(
        "[credential-denylist] {} credential files refused, {} controls read",
        targets.len(),
        controls.len()
    );
}
