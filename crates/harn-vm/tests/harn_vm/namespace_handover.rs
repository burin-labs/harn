//! The confinement handover into a helper process.
//!
//! An integration test rather than a unit test because the claim can only be
//! read from the far side of an `exec`, which means running a real second
//! program. That program is this crate's own process helper, so the case
//! measures the handover rather than whatever shell the host image ships.

#[cfg(target_os = "linux")]
#[test]
fn unavailable_landlock_fixture() {
    if std::env::var_os("BWRAP_LEGACY_HANDOVER_FIXTURE").is_none() {
        return;
    }
    use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    };
    harn_vm::orchestration::push_execution_policy(policy);
    let result = harn_vm::process_sandbox::transferable_confinement("/usr/bin/true");
    harn_vm::orchestration::pop_execution_policy();
    let Err(harn_vm::VmError::SandboxMechanismUnavailable(refusal)) = result else {
        panic!("a Landlock-only handover must not return seccomp-only confinement");
    };
    assert_eq!(
        refusal.mechanism,
        harn_vm::process_sandbox::SandboxMechanism::LinuxLandlock
    );
    assert_eq!(
        refusal.availability,
        harn_vm::process_sandbox::SandboxMechanismAvailability::AbsentOnHost
    );
}

#[cfg(target_os = "linux")]
fn landlock_unavailable_fixture_command(name: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", name, "--nocapture"]);
    let instructions = [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_landlock_create_ruleset as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    // SAFETY: the fork callback only reads owned instructions and calls prctl.
    unsafe {
        command.pre_exec(move || {
            let filter = libc::sock_fprog {
                len: instructions.len() as u16,
                filter: instructions.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &filter) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_landlock_never_returns_a_seccomp_only_handover() {
    let mut command =
        landlock_unavailable_fixture_command("namespace_handover::unavailable_landlock_fixture");
    command.env("BWRAP_LEGACY_HANDOVER_FIXTURE", "1");
    let result = command.output().unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("1 passed; 0 failed"),
        "the refusal fixture must actually execute: {result:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn setup_probe_environment_fixture() {
    let Some(mode) = std::env::var_os("BWRAP_PROBE_LOADER_FIXTURE") else {
        return;
    };
    // Only the exact selected test runs in this disposable child. Mutating
    // its environment cannot affect another in-process test or its launcher.
    if mode == "diagnostic" {
        std::env::set_var("LD_TRACE_LOADED_OBJECTS", "1");
    }
    if harn_vm::process_sandbox::active_backend_filesystem_available() {
        println!("setup-probe-confined-marker-reached");
    } else {
        println!("setup-probe-unavailable");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn setup_probe_ignores_ambient_payload_loader_controls_in_isolated_children() {
    let run = |mode: &str| {
        let result = landlock_unavailable_fixture_command(
            "namespace_handover::setup_probe_environment_fixture",
        )
        .env("BWRAP_PROBE_LOADER_FIXTURE", mode)
        .output()
        .unwrap();
        assert!(result.status.success(), "{result:?}");
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(
            output.contains("1 passed; 0 failed"),
            "fixture must actually run: {output}"
        );
        output
    };
    let baseline = run("baseline");
    if baseline.contains("setup-probe-unavailable") {
        eprintln!("NOT EXERCISED: functional bubblewrap namespaces unavailable");
        assert_ne!(std::env::var("BWRAP_REQUIRE_TESTS").as_deref(), Ok("1"));
        return;
    }
    assert!(
        baseline.contains("setup-probe-confined-marker-reached"),
        "{baseline}"
    );
    let diagnostic = run("diagnostic");
    assert!(
        diagnostic.contains("setup-probe-confined-marker-reached"),
        "{diagnostic}"
    );
}

/// The ruleset descriptor actually reaches the far side of an `exec`, and the
/// hook that makes it do so is what carries it.
///
/// This is the step of the namespace handover that fails silently. Every
/// descriptor this runtime opens is close-on-exec, so without the hook the
/// helper is handed a number naming nothing. It would enter no ruleset,
/// install the syscall filter, run the payload and exit zero, while the layer
/// above still reported the filesystem boundary as enforced.
///
/// Asserted by asking the exec'd process whether the number is open, rather
/// than by inspecting the parent, because the parent's descriptor is open
/// either way and reading it there would pass with the hook removed. The
/// negative control is the same spawn without the hook, in the same case, so
/// the two cannot drift apart.
#[cfg(target_os = "linux")]
#[test]
fn the_ruleset_descriptor_survives_the_helper_exec_only_with_the_hook() {
    use std::collections::BTreeMap;
    use std::process::Command;

    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };
    use harn_vm::process_sandbox::{keep_ruleset_across_exec, transferable_confinement};

    let workspace = tempfile::tempdir().expect("workspace");
    let helper = crate::support::process_helper();

    let policy = CapabilityPolicy {
        tools: Vec::new(),
        capabilities: BTreeMap::new(),
        workspace_roots: vec![workspace.path().display().to_string()],
        read_only_roots: Vec::new(),
        side_effect_level: Some("process_exec".to_string()),
        sandbox_profile: SandboxProfile::OsHardened,
        ..CapabilityPolicy::default()
    };

    push_execution_policy(policy);
    let built = transferable_confinement(&helper);
    pop_execution_policy();

    let confinement = match built {
        Ok(Some(confinement)) => confinement,
        Ok(None) => panic!("the policy asked for confinement, so one must have been built"),
        Err(error) => panic!("building the confinement must not fail here: {error:?}"),
    };
    let Some(fd) = confinement.ruleset_fd() else {
        eprintln!(
            "[ruleset-handover] SKIP: this host built no Landlock ruleset, so there is no \
             descriptor to carry"
        );
        return;
    };
    let fd = fd.to_string();

    let mut carried = Command::new(&helper);
    carried.args(["--fd-open", &fd]);
    keep_ruleset_across_exec(&mut carried, confinement);
    let carried = carried.status().expect("spawn the carrying probe");

    // NEGATIVE CONTROL: the same descriptor, the same probe, no hook. A build
    // that stopped clearing the flag would make the assertion above pass only
    // if this one also passed, and it must not.
    let mut dropped = Command::new(&helper);
    dropped.args(["--fd-open", &fd]);
    let dropped = dropped.status().expect("spawn the control probe");

    assert!(
        carried.success(),
        "the ruleset descriptor must be open in the exec'd helper; without it the helper \
         enters no ruleset and the payload runs under the syscall filter alone"
    );
    assert!(
        !dropped.success(),
        "the control must not see the descriptor: if it does, the flag was never set and this \
         case proves nothing about the hook"
    );
}
