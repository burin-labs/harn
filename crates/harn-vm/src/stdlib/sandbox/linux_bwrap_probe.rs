//! Functional Bubblewrap availability, bounded through the runtime's existing
//! interrupt and process cleanup owner. A failed namespace setup must not hold
//! the command preparation step open through a descendant's inherited pipes.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::{executable, sealed_filter, DescriptorTransfer};

pub(super) fn probe() -> bool {
    if !std::fs::read("/etc/passwd").is_ok_and(|bytes| !bytes.is_empty()) {
        return false;
    }
    let Some(executable) = executable() else {
        return false;
    };
    let mut command = Command::new(executable);
    // This setup-only probe uses absolute programs and no payload grants.
    // Ambient loader controls must not run before namespace setup.
    command.env_clear();
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-net",
    ]);
    let mut descriptors = Vec::new();
    for path in ["/usr", "/lib", "/lib64", "/bin"] {
        if Path::new(path).exists() {
            let Ok(file) = std::fs::File::open(path) else {
                return false;
            };
            command.args(["--ro-bind-fd", &file.as_raw_fd().to_string(), path]);
            descriptors.push(file.into());
        }
    }
    // Probe the same fd-based mounts and filter installation the launch needs.
    let Ok(filter) = sealed_filter(&[0x06, 0, 0, 0, 0, 0, 0xff, 0x7f]) else {
        return false;
    };
    command.args(["--seccomp", &filter.as_raw_fd().to_string()]);
    descriptors.push(filter);
    DescriptorTransfer::new(descriptors).attach(&mut command);
    command.args([
        "--",
        "/usr/bin/sh",
        "-c",
        "test ! -e /etc/passwd && printf harn-bwrap-boundary",
    ]);
    // A failed wrapper may leave a descendant retaining stdout. Preparation
    // precedes the command timeout, so observe cancellation and bound this
    // setup operation without replacing an earlier caller deadline.
    let _deadline = crate::op_interrupt::with_deadline(Instant::now() + Duration::from_secs(1));
    crate::op_interrupt::capture_output_interruptible(&mut command)
        .is_ok_and(|output| probe_output_is_available(&output))
}

pub(super) fn probe_output_is_available(output: &std::process::Output) -> bool {
    output.status.success() && output.stdout == b"harn-bwrap-boundary"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};

    const CHILD_ROOT: &str = "HARN_NESTED_BWRAP_PROBE_ROOT";

    fn policy(root: &Path) -> CapabilityPolicy {
        CapabilityPolicy {
            workspace_roots: vec![root.display().to_string()],
            sandbox_profile: SandboxProfile::OsHardened,
            process_sandbox: Box::new(ProcessSandboxPolicy {
                allow_child_workspace_write: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn nested_availability_refuses_before_command_deadline() {
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let error = crate::stdlib::sandbox::build_std_command::<super::super::super::Backend>(
                "/bin/sh",
                &["-c".into(), "printf known-child".into()],
                &policy(Path::new(&root)),
                SandboxProfile::OsHardened,
            )
            .expect_err("inherited syscall ceiling must refuse new confinement");
            assert!(error.sandbox_mechanism_unavailable().is_some(), "{error}");
            println!("nested probe returned typed sandbox refusal");
            return;
        }

        // Missing Bubblewrap must not bypass the setup path this test protects.
        if executable().is_none() || !super::super::super::landlock_available() {
            eprintln!("[nested-bwrap] exercised=0: Bubblewrap executable and functional Landlock required");
            assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut policy = policy(root.path());
        policy.process_sandbox.read_roots =
            vec![executable.parent().unwrap().display().to_string()];
        let mut control =
            crate::stdlib::sandbox::build_std_command::<super::super::super::Backend>(
                "/bin/sh",
                &["-c".into(), "printf known-child".into()],
                &policy,
                policy.sandbox_profile,
            )
            .unwrap();
        let control = crate::op_interrupt::capture_output_interruptible(&mut control).unwrap();
        assert!(control.status.success(), "{control:?}");
        assert_eq!(control.stdout, b"known-child");
        let args = vec![
            "--exact".into(),
            "stdlib::sandbox::linux::bwrap::probe::tests::nested_availability_refuses_before_command_deadline".into(),
            "--nocapture".into(),
        ];
        let mut command =
            crate::stdlib::sandbox::build_std_command::<super::super::super::Backend>(
                executable.to_str().unwrap(),
                &args,
                &policy,
                policy.sandbox_profile,
            )
            .unwrap();
        command
            .env_clear()
            .env(CHILD_ROOT, root.path())
            .env("TMPDIR", root.path());
        let _deadline = crate::op_interrupt::with_deadline(Instant::now() + Duration::from_secs(5));
        let output = crate::op_interrupt::capture_output_interruptible(&mut command).unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("nested probe returned typed sandbox refusal"),
            "{output:?}"
        );
    }
}
