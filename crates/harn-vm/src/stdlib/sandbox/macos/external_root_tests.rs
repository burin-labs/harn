//! A `read` external root reaching a confined child through the macOS
//! sandbox profile. Split out of `macos_tests.rs` to keep that file under the
//! source-length ratchet.

use std::path::Path;
use std::process::Command;

use super::tests::macos_policy_with_workspace_ops;
use super::{render_profile, SANDBOX_EXEC_PATH};

/// A host projects an external read root into capability scope and approval
/// metadata. The first spawn, without that capability grant, is refused; once
/// projected, a confined child can read the root but cannot write there.
#[test]
fn a_read_external_root_is_readable_but_not_writable_by_a_confined_child() {
    if !Path::new(SANDBOX_EXEC_PATH).exists() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let external = tempfile::tempdir().expect("external root");
    let reference = external.path().join("reference.txt");
    std::fs::write(&reference, "reference").expect("seed reference file");
    let written = external.path().join("written.txt");
    let mut policy = macos_policy_with_workspace_ops(&["read_text", "write_text"]);
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    let host_projected_policy = crate::orchestration::CapabilityPolicy {
        read_only_roots: vec![external.path().display().to_string()],
        ..policy.clone()
    };

    let run = |policy: &crate::orchestration::CapabilityPolicy, script: &str| {
        Command::new(SANDBOX_EXEC_PATH)
            .args(["-p", &render_profile(policy), "--", "/bin/sh", "-c", script])
            .current_dir(workspace.path())
            .output()
            .expect("spawn sandbox-exec")
    };
    let read_script = format!("cat '{}'", reference.display());
    let write_script = format!("echo x > '{}'", written.display());

    let unrooted = run(&policy, &read_script);
    assert!(
        !unrooted.status.success(),
        "control: without a capability read root the child must not read outside the workspace"
    );

    crate::orchestration::push_approval_policy(crate::orchestration::ToolApprovalPolicy {
        external_roots: vec![crate::orchestration::ExternalRoot::read(
            external.path().display().to_string(),
        )],
        ..Default::default()
    });
    let read = run(&host_projected_policy, &read_script);
    let write = run(&host_projected_policy, &write_script);
    crate::orchestration::pop_approval_policy();

    assert!(
        read.status.success() && String::from_utf8_lossy(&read.stdout) == "reference",
        "a read external root must be readable: stderr={}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(
        !write.status.success() && !written.exists(),
        "a read external root must not be writable by a confined child"
    );
}
