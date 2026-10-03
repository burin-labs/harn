//! The alternate renderer for the same opened Linux filesystem grants.
//! Bubblewrap creates the namespaces and mounts, then installs Harn's existing
//! seccomp filter. Neither mount sources nor that filter can change after
//! preparation: the launch carries owned descriptors through every exec.

use std::io::{Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{
    compile_seccomp_program, filesystem_profile, policy_allows_network, read_only_access,
    sandbox_rejection, workspace_access, DescriptorTransfer, FilesystemProfile,
    ProcessFilesystemScope, TransferableConfinement, LANDLOCK_ACCESS_FS_EXECUTE,
    LANDLOCK_ACCESS_FS_READ_DIR, LANDLOCK_ACCESS_FS_READ_FILE, LANDLOCK_ACCESS_FS_WRITE_FILE,
};
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::stdlib::sandbox::{PrepareOutcome, SandboxMechanism, SandboxMechanismAvailability};
use crate::VmError;

fn executable() -> Option<PathBuf> {
    ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// A known nonempty host file must disappear through the same real wrapper
/// path. A binary that merely exists, or a probe that observes nothing, fails.
pub(super) fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(probe)
}

fn probe() -> bool {
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
    // Probe the same fd-based mounts and filter installation the launch
    // needs. An older wrapper with only path-based mounts isn't adequate.
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
    command
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| probe_output_is_available(&output))
}

fn probe_output_is_available(output: &std::process::Output) -> bool {
    output.status.success() && output.stdout == b"harn-bwrap-boundary"
}

pub(in crate::stdlib::sandbox) fn prepare(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    if !available() {
        let mut refusal = super::super::SandboxMechanismUnavailable::new(
            SandboxMechanism::LinuxBubblewrap,
            SandboxMechanismAvailability::AbsentOnHost,
            profile,
        );
        refusal.unconfined = super::super::enforcement::ConfinementDimension::ALL
            .into_iter()
            .filter(|dimension| dimension.required_by(policy))
            .collect();
        return Err(refusal.into_error());
    }
    if policy.process_network_proxy.is_some() {
        return Err(sandbox_rejection(
            "bubblewrap cannot express the managed proxy-only egress grant".into(),
        ));
    }
    let filesystem = filesystem_profile(
        program,
        policy,
        u64::MAX,
        ProcessFilesystemScope::PrivatePidNamespace,
    )?;
    let mut helper_grant = FilesystemProfile {
        rules: Vec::new(),
        symlinks: Default::default(),
        handled_access_fs: filesystem.handled_access_fs,
        read_deny_roots: filesystem.read_deny_roots.clone(),
    };
    let seccomp = TransferableConfinement {
        ruleset: None,
        seccomp: compile_seccomp_program(policy)?,
    };
    let filter = sealed_filter(&seccomp.seccomp_bytes()).map_err(|error| {
        sandbox_rejection(format!(
            "could not pin the bubblewrap syscall filter: {error}"
        ))
    })?;
    let MountPlan {
        mut argv,
        mut descriptors,
        device_descriptors,
    } = mounts(filesystem)?;
    argv.splice(
        0..0,
        [
            "--die-with-parent".into(),
            "--unshare-user".into(),
            "--unshare-pid".into(),
            "--unshare-ipc".into(),
        ],
    );
    // An abstract Unix socket belongs to its network namespace too. The
    // existing filter still withholds connect for a serve-only socket grant.
    if !policy_allows_network(policy) {
        argv.push("--unshare-net".into());
    }
    argv.extend(["--seccomp".into(), filter.as_raw_fd().to_string()]);
    descriptors.push(filter);
    // Allocator tuning the launch environment carries is kept out of
    // bubblewrap's own environment and re-applied to the payload through
    // these arguments, written once the final environment is known
    // (`launch_environment`). Empty until then, which adds nothing.
    let payload_env = payload_env_args().map_err(|error| {
        sandbox_rejection(format!(
            "could not create the bubblewrap payload environment descriptor: {error}"
        ))
    })?;
    argv.extend([
        PAYLOAD_ENV_ARGS_FLAG.into(),
        payload_env.as_raw_fd().to_string(),
    ]);
    descriptors.push(payload_env);
    let mut finalizer_args = Vec::new();
    if !device_descriptors.is_empty() {
        let launcher = policy.process_sandbox.netns_launcher_path.as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_file())
            .map(|path| super::super::normalize_for_policy(&path))
            .ok_or_else(|| sandbox_rejection(
                "bubblewrap needs the admitted Harn namespace helper to close device setup descriptors before the payload".into()
            ))?;
        super::push_rule(
            &mut helper_grant,
            launcher.clone(),
            read_only_access(),
            false,
        )?;
        if !helper_grant.symlinks.is_empty() {
            return Err(sandbox_rejection(
                "bubblewrap finalizer path changed after normalization".into(),
            ));
        }
        let helper_mount = mounts(helper_grant)?;
        if !helper_mount.device_descriptors.is_empty() {
            return Err(sandbox_rejection(
                "bubblewrap finalizer must be a regular executable".into(),
            ));
        }
        argv.extend(helper_mount.argv);
        descriptors.extend(helper_mount.descriptors);
        finalizer_args.push(launcher.display().to_string());
        finalizer_args.push(crate::process_sandbox::NETNS_LAUNCH_SUBCOMMAND.into());
        for device in device_descriptors {
            finalizer_args.extend([
                crate::process_sandbox::NETNS_CLOSE_FD_FLAG.into(),
                device.to_string(),
            ]);
        }
        finalizer_args.push("--".into());
    }
    // Seal namespace scaffolding after every payload and helper mount. This
    // is nonrecursive, preserving only the explicitly writable child mounts.
    argv.extend(["--remount-ro".into(), "/".into(), "--".into()]);
    argv.extend(finalizer_args);
    argv.push(program.into());
    argv.extend_from_slice(args);
    Ok(PrepareOutcome::BubblewrapExec {
        wrapper: executable()
            .expect("the functional probe found bubblewrap")
            .to_string_lossy()
            .into_owned(),
        args: argv,
        descriptors: DescriptorTransfer::new(descriptors),
    })
}

struct MountPlan {
    argv: Vec<String>,
    descriptors: Vec<OwnedFd>,
    device_descriptors: Vec<super::DeviceMountFinalization>,
}

fn mounts(filesystem: FilesystemProfile) -> Result<MountPlan, VmError> {
    let mut argv = Vec::new();
    let mut descriptors = Vec::new();
    let mut device_descriptors = Vec::new();
    // Create aliases before any host mount so this never modifies a writable
    // host grant. Rebuilding an alias grants no target the handles omit.
    for (alias, target) in filesystem.symlinks {
        argv.extend([
            "--symlink".into(),
            target.to_string_lossy().into_owned(),
            alias.to_string_lossy().into_owned(),
        ]);
    }
    for rule in filesystem.rules {
        let metadata = rule.file.metadata().map_err(|error| {
            sandbox_rejection(format!(
                "cannot inspect pinned grant {}: {error}",
                rule.path.display()
            ))
        })?;
        if rule.path == Path::new("/proc") {
            // A fresh procfs in the PID namespace cannot expose host siblings.
            // A files-only grant cannot be widened to directory enumeration.
            if rule.allowed_access & LANDLOCK_ACCESS_FS_READ_DIR == 0 {
                return Err(sandbox_rejection(
                    "bubblewrap cannot express the files-only process introspection grant".into(),
                ));
            }
            argv.extend([
                "--proc".into(),
                "/proc".into(),
                "--remount-ro".into(),
                "/proc".into(),
            ]);
            continue;
        }
        let reads = LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;
        let writable = rule.allowed_access & LANDLOCK_ACCESS_FS_WRITE_FILE != 0;
        if metadata.is_dir()
            && rule.allowed_access != read_only_access()
            && rule.allowed_access != workspace_access(&CapabilityPolicy::default())
        {
            return Err(sandbox_rejection(format!(
                "bubblewrap cannot express selective filesystem rights for {}",
                rule.path.display(),
            )));
        }
        if rule.allowed_access & reads == 0
            || (rule.allowed_access & LANDLOCK_ACCESS_FS_EXECUTE == 0
                && metadata.permissions().mode() & 0o111 != 0
                && !metadata.file_type().is_char_device())
        {
            return Err(sandbox_rejection(format!(
                "bubblewrap cannot express selective read/execute rights for {}",
                rule.path.display(),
            )));
        }
        let destination = rule.path.to_string_lossy().into_owned();
        let source = rule.file.as_raw_fd();
        if metadata.file_type().is_char_device() {
            // Unlike --bind-fd, --dev-bind does not consume its source fd.
            // Finalization closes it so procfs cannot reopen the host device
            // outside the mounted grant's permissions.
            device_descriptors.push(super::DeviceMountFinalization {
                descriptor: source,
                destination: rule.path.clone(),
            });
            // --dev-bind has no fd spelling. Naming our still-owned descriptor
            // through procfs keeps the same pinned source, not a mutable path.
            argv.extend([
                "--dev-bind".into(),
                format!("/proc/self/fd/{source}"),
                destination.clone(),
            ]);
            if !writable {
                argv.extend(["--remount-ro".into(), destination]);
            }
        } else {
            argv.extend([
                if writable {
                    "--bind-fd"
                } else {
                    "--ro-bind-fd"
                }
                .into(),
                source.to_string(),
                destination,
            ]);
        }
        descriptors.push(rule.file.into());
    }
    // Workspace scratch is a named existing writable grant, not a broad host
    // /tmp bind or an unreported extra writable tmpfs.
    Ok(MountPlan {
        argv,
        descriptors,
        device_descriptors,
    })
}

/// Bubblewrap reads NUL-separated options from the descriptor given here, at
/// the point in its argv where the flag appears.
pub(in crate::stdlib::sandbox) const PAYLOAD_ENV_ARGS_FLAG: &str = "--args";

pub(in crate::stdlib::sandbox) fn payload_env_args() -> std::io::Result<OwnedFd> {
    let raw = unsafe { libc::memfd_create(c"harn-bwrap-env".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn sealed_filter(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    let raw = unsafe {
        libc::memfd_create(
            c"harn-seccomp".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = std::fs::File::from(descriptor);
    file.write_all(bytes)?;
    file.rewind()?;
    if unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ProcessSandboxPolicy;

    fn live() -> bool {
        let available = available()
            && std::env::var("BWRAP_LAUNCHER").is_ok_and(|path| Path::new(&path).is_file());
        if !available {
            eprintln!("[linux-bwrap] exercised=0: functional namespace/mount probe or BWRAP_LAUNCHER test helper unavailable");
            assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        }
        available
    }

    #[test]
    fn functional_probe_requires_the_confinement_marker_not_loader_exit_zero() {
        if !probe() {
            eprintln!("[linux-bwrap] exercised=0: functional namespace/mount probe unavailable");
            assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
            return;
        }
        let diagnostic = Command::new(executable().unwrap())
            .env("LD_TRACE_LOADED_OBJECTS", "1")
            .output()
            .unwrap();
        assert!(diagnostic.status.success());
        assert!(!diagnostic.stdout.is_empty());
        assert!(
            !probe_output_is_available(&diagnostic),
            "loader exit zero is not a confinement marker"
        );
    }

    fn policy(root: &Path) -> CapabilityPolicy {
        CapabilityPolicy {
            workspace_roots: vec![root.display().to_string()],
            sandbox_profile: SandboxProfile::Worktree,
            process_sandbox: Box::new(ProcessSandboxPolicy {
                presets: Some(vec![]),
                allow_child_workspace_write: true,
                allow_process_self_introspection: true,
                netns_launcher_path: std::env::var("BWRAP_LAUNCHER").ok(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn launch(prepared: PrepareOutcome, cwd: &Path) -> std::process::Output {
        let PrepareOutcome::BubblewrapExec {
            wrapper,
            args,
            descriptors,
        } = prepared
        else {
            panic!("the bubblewrap owner did not prepare a pinned launch")
        };
        let mut command = Command::new(wrapper);
        command.args(args).current_dir(cwd);
        descriptors.attach(&mut command);
        command.output().unwrap()
    }

    /// Proven on a real bubblewrap launch: inherited allocator tuning reaches
    /// the confined payload, bubblewrap itself never carries it, and a
    /// behavior-changing control is still refused.
    #[test]
    fn allocator_tuning_reaches_the_payload_and_not_bubblewrap() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let script = vec![
            "-c".into(),
            "printf '%s|%s' \"$MALLOC_ARENA_MAX\" \"${MALLOC_CHECK_-unset}\"".into(),
        ];
        let PrepareOutcome::BubblewrapExec {
            wrapper,
            args,
            descriptors,
        } = prepare("/usr/bin/sh", &script, &policy, policy.sandbox_profile).unwrap()
        else {
            panic!("the bubblewrap owner did not prepare a pinned launch")
        };
        let mut command = Command::new(wrapper);
        command
            .args(args)
            .current_dir(root.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("MALLOC_ARENA_MAX", "2");
        descriptors.attach(&mut command);
        crate::stdlib::sandbox::launch_environment::reapply_allocator_tuning(&mut command, true)
            .unwrap();
        assert!(!command
            .get_envs()
            .any(|(name, value)| name == "MALLOC_ARENA_MAX" && value.is_some()));
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "2|unset");

        // Negative control: the validator still refuses a loader control.
        assert!(crate::security::validate_process_environment(
            crate::security::ProcessEnvironmentBoundary::TrustedSetup,
            std::iter::empty(),
            [("GLIBC_TUNABLES".into(), Some("glibc.malloc.check=3".into()))],
        )
        .is_err());
    }

    #[test]
    fn workspace_write_requires_the_existing_child_write_grant() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ordinary"), "readable").unwrap();
        let mut policy = policy(root.path());
        policy.capabilities =
            std::collections::BTreeMap::from([("workspace".into(), vec!["read_text".into()])]);
        policy.process_sandbox.allow_child_workspace_write = false;
        let read_only = vec!["-c".into(),
            "test \"$(cat ordinary)\" = readable; if printf escaped > marker; then exit 31; fi; printf denied-write".into()];
        let refused = launch(
            prepare("/usr/bin/sh", &read_only, &policy, policy.sandbox_profile).unwrap(),
            root.path(),
        );
        assert!(refused.status.success(), "{refused:?}");
        assert_eq!(refused.stdout, b"denied-write");
        assert!(!root.path().join("marker").exists());
        policy.process_sandbox.allow_child_workspace_write = true;
        let writable = vec!["-c".into(), "printf granted > marker".into()];
        let allowed = launch(
            prepare("/usr/bin/sh", &writable, &policy, policy.sandbox_profile).unwrap(),
            root.path(),
        );
        assert!(allowed.status.success(), "{allowed:?}");
        assert_eq!(
            std::fs::read(root.path().join("marker")).unwrap(),
            b"granted"
        );
    }

    #[test]
    fn opened_grant_survives_path_replacement_and_path_mount_control_does_not() {
        if !live() {
            return;
        }
        let tree = tempfile::tempdir().unwrap();
        let granted = tree.path().join("granted");
        std::fs::create_dir(&granted).unwrap();
        std::fs::write(granted.join("marker"), "original").unwrap();
        let policy = policy(&granted);
        let args = vec![
            "-c".into(),
            "cat \"$1/marker\"; printf reached > \"$1/new\"".into(),
            "probe".into(),
            granted.display().to_string(),
        ];
        let pinned = prepare("/usr/bin/sh", &args, &policy, policy.sandbox_profile).unwrap();
        let mut path_control =
            prepare("/usr/bin/sh", &args, &policy, policy.sandbox_profile).unwrap();
        let PrepareOutcome::BubblewrapExec { args, .. } = &mut path_control else {
            unreachable!()
        };
        let index = args
            .windows(3)
            .position(|args| args[0] == "--bind-fd" && args[2] == granted.display().to_string())
            .expect("actual workspace grant was rendered");
        args[index] = "--bind".into();
        args[index + 1] = granted.display().to_string();
        let admitted_inode = tree.path().join("admitted-inode");
        std::fs::rename(&granted, &admitted_inode).unwrap();
        std::fs::create_dir(&granted).unwrap();
        std::fs::write(granted.join("marker"), "replacement").unwrap();

        let pinned = launch(pinned, tree.path());
        assert!(pinned.status.success(), "{pinned:?}");
        assert_eq!(pinned.stdout, b"original");
        assert_eq!(
            std::fs::read(admitted_inode.join("new")).unwrap(),
            b"reached"
        );
        assert!(!granted.join("new").exists());
        let control = launch(path_control, tree.path());
        assert!(control.status.success(), "{control:?}");
        assert_eq!(control.stdout, b"replacement");
        assert_eq!(std::fs::read(granted.join("new")).unwrap(), b"reached");
    }

    #[test]
    fn outside_read_readonly_write_and_ungranted_scaffolding_are_denied() {
        if !live() {
            return;
        }
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let readonly = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel"), "outside").unwrap();
        std::fs::write(readonly.path().join("marker"), "readonly").unwrap();
        let mut policy = policy(root.path());
        policy.process_sandbox.read_roots = vec![readonly.path().display().to_string()];
        let args = vec![
            "-c".into(),
            concat!(
                "set -eu; test ! -e \"$1/sentinel\"; ",
                "test \"$(cat \"$2/marker\")\" = readonly; ",
                "if printf changed > \"$2/marker\" 2>/dev/null; then exit 31; fi; ",
                "if mkdir /ungranted 2>/dev/null; then exit 32; fi; ",
                "if printf 1 > /proc/self/oom_score_adj 2>/dev/null; then exit 33; fi; ",
                "printf inside > marker; printf boundary-reached"
            )
            .into(),
            "probe".into(),
            outside.path().display().to_string(),
            readonly.path().display().to_string(),
        ];
        let result = launch(
            prepare("/usr/bin/sh", &args, &policy, policy.sandbox_profile).unwrap(),
            root.path(),
        );
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"boundary-reached");
        assert_eq!(
            std::fs::read(root.path().join("marker")).unwrap(),
            b"inside"
        );
        assert_eq!(
            std::fs::read(readonly.path().join("marker")).unwrap(),
            b"readonly"
        );
        assert_eq!(
            std::fs::read(outside.path().join("sentinel")).unwrap(),
            b"outside"
        );
    }

    #[test]
    fn mount_failure_precedes_payload_and_pinned_handles_do_not_escape() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let args = vec![
            "-c".into(),
            concat!(
                "set -eu; for fd in \"$@\"; do ",
                "test ! -e /proc/self/fd/$fd; done; ",
                "printf reached > marker; printf closed-pinned-handles"
            )
            .into(),
            "probe".into(),
        ];
        let mut valid = prepare("/usr/bin/sh", &args, &policy, policy.sandbox_profile).unwrap();
        let PrepareOutcome::BubblewrapExec {
            args, descriptors, ..
        } = &mut valid
        else {
            unreachable!()
        };
        args.extend(descriptors.numbers().into_iter().map(|fd| fd.to_string()));
        let mut invalid = prepare("/usr/bin/sh", &[], &policy, policy.sandbox_profile).unwrap();
        let PrepareOutcome::BubblewrapExec { args, .. } = &mut invalid else {
            unreachable!()
        };
        let index = args.iter().position(|arg| arg == "--bind-fd").unwrap();
        args[index + 1] = "-1".into();
        let payload = args.iter().position(|arg| arg == "--").unwrap();
        args.truncate(payload + 1);
        args.extend([
            "/usr/bin/sh".into(),
            "-c".into(),
            "printf escaped > marker".into(),
        ]);
        let failed = launch(invalid, root.path());
        assert!(
            !failed.status.success(),
            "invalid mount reached the payload: {failed:?}"
        );
        assert!(!root.path().join("marker").exists());
        let reached = launch(valid, root.path());
        assert!(reached.status.success(), "{reached:?}");
        assert_eq!(reached.stdout, b"closed-pinned-handles");
        assert_eq!(
            std::fs::read(root.path().join("marker")).unwrap(),
            b"reached"
        );
    }

    #[test]
    fn a_profile_without_device_grants_executes_without_the_finalizer() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let mut policy = policy(root.path());
        policy.process_sandbox.read_deny_roots = vec!["/dev".into()];
        policy.process_sandbox.netns_launcher_path = None;
        let payload = vec!["-c".into(), "printf reached > marker".into()];
        let prepared = prepare("/usr/bin/sh", &payload, &policy, policy.sandbox_profile).unwrap();
        let PrepareOutcome::BubblewrapExec { args, .. } = &prepared else {
            unreachable!()
        };
        let payload = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[payload + 1], "/usr/bin/sh");
        let reached = launch(prepared, root.path());
        assert!(reached.status.success(), "{reached:?}");
        assert_eq!(
            std::fs::read(root.path().join("marker")).unwrap(),
            b"reached"
        );
    }

    #[test]
    fn device_grants_without_a_finalizer_refuse_before_payload() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let mut policy = policy(root.path());
        policy.process_sandbox.netns_launcher_path = None;
        let filesystem = filesystem_profile(
            "/usr/bin/sh",
            &policy,
            u64::MAX,
            ProcessFilesystemScope::PrivatePidNamespace,
        )
        .unwrap();
        assert!(!mounts(filesystem).unwrap().device_descriptors.is_empty());
        let payload = vec!["-c".into(), "printf reached > marker".into()];
        let refused = match prepare("/usr/bin/sh", &payload, &policy, policy.sandbox_profile) {
            Err(error) => error,
            Ok(_) => panic!("device setup was admitted without its finalizer"),
        };
        assert!(refused.to_string().contains("namespace helper"));
        assert!(!root.path().join("marker").exists());
    }

    #[test]
    fn replaced_device_mount_is_refused_before_payload() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let payload_args = vec!["-c".into(), "printf reached > marker".into()];
        let mut changed = prepare(
            "/usr/bin/sh",
            &payload_args,
            &policy,
            policy.sandbox_profile,
        )
        .unwrap();
        let PrepareOutcome::BubblewrapExec { args, .. } = &mut changed else {
            unreachable!()
        };
        let index = args
            .windows(3)
            .position(|args| args[0] == "--dev-bind" && args[2] == "/dev/null")
            .expect("the null device was admitted by its pinned descriptor");
        args[index + 1] = "/dev/zero".into();
        let refused = launch(changed, root.path());
        assert!(!refused.status.success(), "{refused:?}");
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("mounted device differs from its pinned source"),
            "{refused:?}"
        );
        assert!(!root.path().join("marker").exists());
        let reached = launch(
            prepare(
                "/usr/bin/sh",
                &payload_args,
                &policy,
                policy.sandbox_profile,
            )
            .unwrap(),
            root.path(),
        );
        assert!(reached.status.success(), "{reached:?}");
        assert_eq!(
            std::fs::read(root.path().join("marker")).unwrap(),
            b"reached"
        );
    }

    #[test]
    fn both_renderers_subtract_credential_aliases_and_terminate_alias_cycles() {
        if !live() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let read = tempfile::tempdir().unwrap();
        let secret = read.path().join("credential");
        std::fs::write(&secret, "synthetic-denied").unwrap();
        std::fs::write(read.path().join("ordinary"), "ordinary").unwrap();
        let alias = read.path().join("alias");
        std::os::unix::fs::symlink(&secret, &alias).unwrap();
        std::os::unix::fs::symlink(read.path(), read.path().join("cycle")).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"synthetic-denied");
        let mut policy = policy(root.path());
        policy.process_sandbox.read_roots = vec![read.path().display().to_string()];
        policy.process_sandbox.read_deny_roots = vec![secret.display().to_string()];
        let args = vec![
            "-c".into(),
            concat!(
                "set -eu; test \"$(cat \"$1/ordinary\")\" = ordinary; ",
                "for name in credential alias cycle/credential; do ",
                "if cat \"$1/$name\" >/dev/null 2>&1; then exit 34; fi; done; ",
                "printf credential-boundary-reached"
            )
            .into(),
            "probe".into(),
            read.path().display().to_string(),
        ];
        let result = launch(
            prepare("/usr/bin/sh", &args, &policy, policy.sandbox_profile).unwrap(),
            root.path(),
        );
        assert!(result.status.success(), "bubblewrap: {result:?}");
        assert_eq!(result.stdout, b"credential-boundary-reached");
        if super::super::landlock_available() {
            let mut command = super::super::super::build_std_command::<super::super::Backend>(
                "/usr/bin/sh",
                &args,
                &policy,
                policy.sandbox_profile,
            )
            .unwrap();
            let result = command.current_dir(root.path()).output().unwrap();
            assert!(result.status.success(), "landlock: {result:?}");
            assert_eq!(result.stdout, b"credential-boundary-reached");
        } else {
            eprintln!("[linux-landlock] exercised=0: Landlock unavailable on this host");
            assert_ne!(
                std::env::var("HARN_REQUIRE_LANDLOCK_TESTS").as_deref(),
                Ok("1")
            );
        }
    }
}
