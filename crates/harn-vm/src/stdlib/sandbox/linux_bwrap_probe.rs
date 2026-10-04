//! Functional Bubblewrap availability, bounded through the runtime's existing
//! interrupt and process cleanup owner. A failed namespace setup must not hold
//! the command preparation step open through a descendant's inherited pipes.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::{executable, sealed_filter, DescriptorTransfer};

pub(super) enum ProbeOutcome {
    Completed(bool),
    Interrupted,
}

pub(super) fn probe() -> ProbeOutcome {
    if crate::op_interrupt::requested() {
        return ProbeOutcome::Interrupted;
    }
    if !std::fs::read("/etc/passwd").is_ok_and(|bytes| !bytes.is_empty()) {
        return ProbeOutcome::Completed(false);
    }
    let Some(executable) = executable() else {
        return ProbeOutcome::Completed(false);
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
                return ProbeOutcome::Completed(false);
            };
            command.args(["--ro-bind-fd", &file.as_raw_fd().to_string(), path]);
            descriptors.push(file.into());
        }
    }
    // Probe the same fd-based mounts and filter installation the launch needs.
    let Ok(filter) = sealed_filter(&[0x06, 0, 0, 0, 0, 0, 0xff, 0x7f]) else {
        return ProbeOutcome::Completed(false);
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
    capture_probe(&mut command)
}

fn capture_probe(command: &mut Command) -> ProbeOutcome {
    // A failed wrapper may leave a descendant retaining stdout. Preparation
    // precedes the command timeout, so observe cancellation and bound this
    // setup operation without replacing an earlier caller deadline.
    let _deadline = crate::op_interrupt::with_deadline(Instant::now() + Duration::from_secs(1));
    let output = crate::op_interrupt::capture_output_interruptible(command);
    if crate::op_interrupt::requested() {
        ProbeOutcome::Interrupted
    } else {
        ProbeOutcome::Completed(output.is_ok_and(|output| probe_output_is_available(&output)))
    }
}

pub(super) fn probe_output_is_available(output: &std::process::Output) -> bool {
    output.status.success() && output.stdout == b"harn-bwrap-boundary"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};

    const CHILD_ROOT: &str = "HARN_TEST_NESTED_BWRAP_PROBE_ROOT";

    #[test]
    fn cancelled_setup_does_not_cache_unavailability() {
        let cache = std::sync::Mutex::new(None);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let interrupted = crate::op_interrupt::install(Some(cancel), None);
        let mut first = Command::new("/usr/bin/printf");
        first.env_clear().arg("harn-bwrap-boundary");
        assert!(!super::super::cached_availability(
            &cache,
            || capture_probe(&mut first)
        ));
        assert!(crate::op_interrupt::requested());
        assert!(
            cache.lock().unwrap().is_none(),
            "caller cancellation is not a host fact"
        );
        drop(interrupted);

        // A real nonempty successful process on the same capture and cache
        // path proves that cancellation did not poison this process's memo.
        // It does not claim this host can create Bubblewrap namespaces.
        let mut next = Command::new("/usr/bin/printf");
        next.env_clear().arg("harn-bwrap-boundary");
        assert!(super::super::cached_availability(&cache, || capture_probe(
            &mut next
        )));
        assert_eq!(*cache.lock().unwrap(), Some(true));
        assert!(super::super::cached_availability(&cache, || panic!(
            "completed host fact must be reused"
        )));
    }

    #[test]
    fn preparation_preserves_caller_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let _cancel = crate::op_interrupt::install(
            Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                true,
            ))),
            None,
        );
        let error = super::super::prepare(
            "/usr/bin/printf",
            &["known-child".into()],
            &policy(root.path()),
            SandboxProfile::OsHardened,
        )
        .err()
        .expect("cancelled preparation must not launch the payload");
        assert!(crate::cancellation::is_cancellation(&error), "{error}");
        assert!(error.sandbox_mechanism_unavailable().is_none());
    }

    #[test]
    fn preparation_preserves_caller_deadline() {
        let root = tempfile::tempdir().unwrap();
        let _deadline = crate::op_interrupt::install(None, Some(Instant::now()));
        let error = super::super::prepare(
            "/usr/bin/printf",
            &["known-child".into()],
            &policy(root.path()),
            SandboxProfile::OsHardened,
        )
        .err()
        .expect("expired preparation must not launch the payload");
        assert!(matches!(error, crate::VmError::Thrown(_)), "{error}");
        assert_eq!(
            error.to_string(),
            crate::Vm::deadline_exceeded_error().to_string()
        );
        assert!(error.sandbox_mechanism_unavailable().is_none());
    }

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
