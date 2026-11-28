//! Live Landlock falsifier for the host facts runtimes size themselves from.
//!
//! Thread pools and allocators read CPU topology and cgroup limits at
//! startup; some abort without them. A confined child must be able to read
//! them, and must still be refused a file outside every grant.

use super::tests::live_landlock_available;
use super::*;

/// Whether a confined read of `path`'s first byte under the default worktree
/// profile succeeds. One byte, because `/dev/full` never ends.
fn confined_read(workspace: &Path, path: &str) -> bool {
    crate::orchestration::push_execution_policy(CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });
    let output = crate::stdlib::sandbox::command_output(
        "/bin/sh",
        &["-c".to_string(), format!("head -c 1 '{path}' > /dev/null")],
        &crate::stdlib::sandbox::ProcessCommandConfig {
            cwd: Some(workspace.to_path_buf()),
            ..Default::default()
        },
    );
    crate::orchestration::pop_execution_policy();
    output.is_ok_and(|output| output.status.success())
}

fn host_reads(path: &str) -> bool {
    use std::io::Read;
    std::fs::File::open(path).is_ok_and(|mut file| file.read(&mut [0u8; 1]).is_ok())
}

#[test]
fn a_confined_child_reads_cpu_topology_and_cgroup_limits() {
    const TEST: &str = "a_confined_child_reads_cpu_topology_and_cgroup_limits";
    if !live_landlock_available(TEST) {
        return;
    }
    let workspace = tempfile::TempDir::new().expect("workspace");
    let workspace = workspace.path().canonicalize().expect("canonical");
    // Control: a file outside every grant stays unreadable, so a pass below is
    // the grant's and not a sandbox that admits everything.
    let secret = tempfile::NamedTempFile::new_in(std::env::current_dir().expect("cwd"))
        .expect("a file outside the workspace");
    assert!(
        !confined_read(&workspace, &secret.path().display().to_string()),
        "a file outside every grant was readable, so nothing was measured"
    );
    // The rest of /sys stays closed: hardware identity is not a runtime limit.
    let identity = "/sys/class/dmi/id/sys_vendor";
    if host_reads(identity) {
        assert!(
            !confined_read(&workspace, identity),
            "{identity} was readable under confinement"
        );
    }
    let mut measured = 0;
    for path in [
        "/sys/devices/system/cpu/online",
        "/sys/devices/system/node/online",
        "/sys/fs/cgroup/cgroup.controllers",
        "/sys/kernel/mm/transparent_hugepage/enabled",
        "/proc/cpuinfo",
        "/proc/meminfo",
        "/dev/full",
    ] {
        if !host_reads(path) {
            eprintln!("[{TEST}] {path}: unreadable on the host, skipped");
            continue;
        }
        assert!(
            confined_read(&workspace, path),
            "a confined child could not read {path}"
        );
        measured += 1;
    }
    // Per-process files are reported, not asserted: Landlock resolves
    // `/proc/self` when the rule is built, to the parent's own directory.
    for path in [
        "/proc/self/cgroup",
        "/proc/self/mountinfo",
        "/proc/self/status",
    ] {
        eprintln!(
            "[{TEST}] {path}: confined read {}",
            if confined_read(&workspace, path) {
                "allowed"
            } else {
                "denied"
            }
        );
    }
    if measured == 0 {
        crate::sandbox_host::boundary_not_measured(TEST, "the host exposes none of the files");
        return;
    }
    eprintln!("[{TEST}] {measured} host files readable under confinement");
}
