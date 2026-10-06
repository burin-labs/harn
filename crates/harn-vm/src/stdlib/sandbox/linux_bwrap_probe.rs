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
    /// A caller interrupt stopped the probe; the caller's error reports it.
    Interrupted,
    /// The probe's own setup budget expired first. Bubblewrap is refused for
    /// this command, but the slow setup is not cached as a host fact.
    Incomplete,
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
    capture_probe_within(command, PROBE_SETUP_BUDGET)
}

/// How long namespace setup may take before Bubblewrap is refused.
const PROBE_SETUP_BUDGET: Duration = Duration::from_secs(1);

fn capture_probe_within(command: &mut Command, budget: Duration) -> ProbeOutcome {
    // A failed wrapper may leave a descendant retaining stdout. Preparation
    // precedes the command timeout, so observe caller interrupts and bound
    // this setup with a budget of its own: its expiry is the probe's outcome,
    // never the caller's deadline error.
    let _budget = crate::op_interrupt::with_operation_budget(Instant::now() + budget);
    let output = crate::op_interrupt::capture_output_interruptible(command);
    if crate::op_interrupt::operation_budget_expired() {
        ProbeOutcome::Incomplete
    } else if crate::op_interrupt::requested() {
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
    fn an_expired_setup_budget_is_an_incomplete_probe_not_a_caller_deadline() {
        // The direct child exits at once, but a descendant keeps stdout open,
        // so only the probe's own budget can end the capture.
        let cache = std::sync::Mutex::new(None);
        let started = Instant::now();
        let outcome = super::super::cached_availability(&cache, || {
            let mut command = Command::new("/bin/sh");
            command
                .env_clear()
                .args(["-c", "sleep 5 & printf harn-bwrap-boundary"]);
            let outcome = capture_probe_within(&mut command, Duration::from_millis(200));
            assert!(
                crate::op_interrupt::requested_error().is_none(),
                "the restored caller context has no interrupt to report"
            );
            assert!(
                matches!(outcome, ProbeOutcome::Incomplete),
                "budget expiry must be the probe's own outcome"
            );
            outcome
        });
        assert!(!outcome, "an incomplete probe refuses Bubblewrap");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(
            cache.lock().unwrap().is_none(),
            "slow setup is not cached as a host fact"
        );
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
            let policy = policy(Path::new(&root));
            // harn#9455 lets a confined child stack its own Landlock domain,
            // so the default path no longer reaches Bubblewrap here.
            assert!(
                super::super::super::landlock_available(),
                "a Landlock-confined child must still see Landlock"
            );
            crate::stdlib::sandbox::build_std_command::<super::super::super::Backend>(
                "/bin/sh",
                &["-c".into(), "printf known-child".into()],
                &policy,
                SandboxProfile::OsHardened,
            )
            .expect("a nested command stacks Landlock");
            // Bubblewrap remains the fallback wherever Landlock is not
            // functional, and the inherited ceiling still withholds the user
            // namespace it needs. That fallback must settle as a typed refusal.
            let error = super::super::prepare(
                "/bin/sh",
                &["-c".into(), "printf known-child".into()],
                &policy,
                SandboxProfile::OsHardened,
            )
            .err()
            .expect("inherited syscall ceiling must refuse Bubblewrap");
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
