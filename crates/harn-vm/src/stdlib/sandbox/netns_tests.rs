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

    let helper = descriptor_probe_helper();

    crate::orchestration::push_execution_policy(policy);
    let built = transferable_confinement(&helper.display().to_string());
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
    // Reads the descriptor from inside the exec'd process, which is the only
    // side that can answer: the parent's own copy is open either way.
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

/// The hermetic helper this case execs to read a descriptor from the far side
/// of an `exec`.
///
/// A shell would do the same job in one line and is what this probe used to
/// run, but a test that depends on the host's `/bin/sh` is not hermetic and
/// the repository gate refuses it. The helper ships from this crate, so the
/// probe measures the handover rather than the image the test happens to run
/// on.
///
/// Resolution is layered because a unit test gets none of the `CARGO_BIN_EXE_*`
/// help an integration test does: the runner's variables first, then the
/// binary beside this test executable. It **panics** rather than skipping when
/// none of them lands. A skip here would be indistinguishable from a pass, and
/// the whole point of this case is that the failure it guards is silent.
#[cfg(test)]
fn descriptor_probe_helper() -> std::path::PathBuf {
    const HELPER: &str = "harn-test-echo-env";

    for key in [
        "NEXTEST_BIN_EXE_harn-test-echo-env",
        "CARGO_BIN_EXE_harn-test-echo-env",
    ] {
        if let Some(path) = std::env::var_os(key) {
            let path = std::path::PathBuf::from(path);
            if path.is_file() {
                return path;
            }
        }
    }
    // The test executable lives in `<target>/<profile>/deps/`, and the helper
    // is built beside it one level up.
    let test_binary = std::env::current_exe().expect("this test executable's own path");
    let beside = test_binary
        .parent()
        .and_then(|deps| deps.parent())
        .map(|profile| profile.join(HELPER));
    match beside {
        Some(path) if path.is_file() => path,
        other => panic!(
            "the {HELPER} helper is required to read the descriptor after exec, and no build of              it was found (looked beside the test executable at {other:?}); refusing to skip,              because a skipped case here is indistinguishable from a passing one"
        ),
    }
}
