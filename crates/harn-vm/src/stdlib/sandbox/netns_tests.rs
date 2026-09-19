//! Cases for the namespace handover.
//!
//! Kept beside the backend cases rather than inside the handover module
//! because both need the same Linux policy fixtures, and separate from them
//! because these two assert a different kind of claim: each names a step that
//! fails silently, and each carries the control that proves it can fail.

use super::tests::linux_policy_with_workspace_ops;
use super::*;
use crate::stdlib::sandbox::handler_sandbox_test_guard;

/// The ruleset descriptor actually reaches the far side of an `exec`, and the
/// hook that makes it do so is what carries it.
///
/// This is the one step of the namespace handover that fails silently. Every
/// descriptor this runtime opens is close-on-exec, so without the hook the
/// helper is handed a number naming nothing. It would enter no ruleset,
/// install the syscall filter, run the payload, and exit zero, while the
/// receipt above it still reported the filesystem boundary as enforced — the
/// same shape as the re-exec bug that created the transferable confinement in
/// the first place.
///
/// Asserted by asking the exec'd child whether the number is open, rather than
/// by inspecting the parent, because the parent's descriptor is open either
/// way and reading it there would pass with the hook removed. The negative
/// control is the same spawn without the hook, in the same test, so the two
/// cannot drift apart.
#[test]
fn the_ruleset_descriptor_survives_the_helper_exec_only_with_the_hook() {
    let _guard = handler_sandbox_test_guard();
    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    policy.side_effect_level = Some("process_exec".to_string());

    crate::orchestration::push_execution_policy(policy);
    let built = transferable_confinement("/bin/sh");
    crate::orchestration::pop_execution_policy();

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
    // Reads the descriptor from inside the exec'd process. `test -e` on the
    // process's own descriptor directory is true only if the number is still
    // open there, which is precisely the claim.
    let probe = format!("test -e /proc/self/fd/{fd}");

    let mut carried = Command::new("/bin/sh");
    carried.args(["-c", &probe]);
    keep_ruleset_across_exec(&mut carried, confinement);
    let carried = carried.status().expect("spawn the carrying probe");

    // NEGATIVE CONTROL: the same descriptor, the same probe, no hook. A build
    // that stopped clearing the flag would make the assertion above pass only
    // if this one also passed, and it must not.
    let mut dropped = Command::new("/bin/sh");
    dropped.args(["-c", &probe]);
    let dropped = dropped.status().expect("spawn the control probe");

    assert!(
        carried.success(),
        "the ruleset descriptor must be open in the exec'd helper; without it the helper \
         enters no ruleset and the payload runs under the syscall filter alone"
    );
    assert!(
        !dropped.success(),
        "the control must not see the descriptor: if it does, the flag was never set and this \
         test proves nothing about the hook"
    );
}

/// A loopback grant admits the socket calls, and cannot do so outside a
/// namespace.
///
/// Both halves are the claim. Without the first, the grant is worth nothing:
/// a filter with no `socket` term refuses loopback exactly as hard as it
/// refuses the internet, so the build tool the grant was opened for still
/// cannot start while every layer above reports a working sandbox — a
/// boundary that refuses everything reads as a boundary that works.
///
/// Without the second, the first is a hole. Those terms carry no address
/// condition, so on the host network they would admit an outbound connection
/// anywhere. What makes them safe is that the child has no route off the
/// host, and the only thing guaranteeing that is the namespace. So the test
/// asserts the widened filter is unreachable without one: the same grant with
/// no helper to build the namespace is refused, rather than rendered with the
/// wider filter and no boundary.
#[test]
fn a_loopback_grant_admits_sockets_only_where_a_namespace_bounds_them() {
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("process_exec".to_string());
    assert!(
        !allowed_syscalls(&policy).contains(&libc::SYS_socket),
        "the premise is that this policy admits no socket calls before the grant",
    );

    policy.process_sandbox.allow_tcp_loopback = true;
    policy.process_sandbox.netns_launcher_path = Some("/opt/example/launch".to_string());
    let granted = allowed_syscalls(&policy);
    for syscall in [
        libc::SYS_socket,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_connect,
        libc::SYS_accept,
    ] {
        assert!(
            granted.contains(&syscall),
            "a loopback grant must admit the calls a loopback client needs",
        );
    }

    // NEGATIVE CONTROL: the same grant with nothing to build the namespace.
    // It must be refused, because the filter it would otherwise be rendered
    // with places no limit on where a socket may reach.
    let mut unnamespaced = policy;
    unnamespaced.process_sandbox.netns_launcher_path = None;
    resolve_netns_launcher(&unnamespaced)
        .expect_err("a loopback grant with no namespace helper must be refused, never widened");
}
