//! The confinement handover into a helper process.
//!
//! An integration test rather than a unit test because the claim can only be
//! read from the far side of an `exec`, which means running a real second
//! program. That program is this crate's own process helper, so the case
//! measures the handover rather than whatever shell the host image ships.

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
