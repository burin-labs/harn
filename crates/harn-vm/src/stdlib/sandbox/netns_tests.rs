//! Cases for the namespace handover that can be read in-process.
//!
//! Kept beside the backend cases because they need the same Linux policy
//! fixtures, and separate from them because this one asserts a different kind
//! of claim: it names a step that fails silently and carries the control that
//! proves it can fail.
//!
//! The other half of the handover cannot be read here at all. Whether the
//! ruleset descriptor survives the `exec` is only answerable from the far
//! side of one, so that case runs a real helper process and lives in
//! `tests/harn_vm/namespace_handover.rs`.

use super::tests::linux_policy_with_workspace_ops;
use super::*;

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
