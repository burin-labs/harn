//! The `CapabilityPolicy` shapes an embedding host writes by hand.
//!
//! This is an integration test, so it compiles against `harn-vm` the way a
//! downstream crate does. Compiling it IS the assertion; the runtime checks
//! below are incidental.
//!
//! # What it pins, and what it deliberately does not
//!
//! `process_sandbox` is `Box<ProcessSandboxPolicy>` today and this test does
//! NOT lock that in. It pins the weaker, more useful thing: a host that owns a
//! `ProcessSandboxPolicy` by value can put it into a `CapabilityPolicy` struct
//! literal with `.into()`, and read the fields back through the field. Both
//! lines compile whether the field is boxed or held by value, because `Box<T>`
//! has `From<T>` and derefs to `T`. So the representation stays free to change
//! for stack-frame reasons, while the shape a consumer writes does not.
//!
//! # Why the weaker contract is the right one
//!
//! v0.10.134 tried the stronger version. The field had been boxed so a policy
//! moved by value stayed one pointer smaller, a downstream pre-tag compile gate
//! failed on the by-value assignments it broke, and unboxing to restore them
//! then failed this repository's own stack-frame budget gate. Neither
//! representation is free, so neither belongs in a contract. `.into()` costs
//! the consumer four characters and survives both.
//!
//! # Blind spot, stated rather than implied
//!
//! Nothing here prevents the other half of that release's break. A public field
//! ADDED to `ProcessSandboxPolicy` fails a downstream EXHAUSTIVE struct literal
//! with E0063, and no `Default`, builder, or serde attribute prevents it. Only
//! the consumer writing `..Default::default()` in its own literal does. This
//! test cannot reproduce that from inside the repository, because the literal
//! it would need to break lives downstream.

use harn_vm::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};

#[test]
fn a_host_moves_an_owned_process_policy_into_the_capability_policy() {
    let process = ProcessSandboxPolicy {
        read_roots: vec!["/opt/toolchain".to_string()],
        allow_tcp_loopback: true,
        ..ProcessSandboxPolicy::default()
    };

    // The load-bearing line: an owned policy, by value, into the literal.
    let policy = CapabilityPolicy {
        workspace_roots: vec!["/work".to_string()],
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: process.clone().into(),
        ..CapabilityPolicy::default()
    };

    // The other half of the contract: a host reads the grants back off the
    // field without naming the representation.
    assert_eq!(policy.process_sandbox.read_roots, vec!["/opt/toolchain"]);
    assert!(policy.process_sandbox.allow_tcp_loopback);

    // A grant this host named no roots for is empty, not absent. That default
    // is the whole contract for a field a host built before it existed.
    assert!(policy.process_sandbox.unix_socket_roots.is_empty());
    assert!(policy.process_sandbox.read_deny_roots.is_empty());
}

#[test]
fn a_default_policy_names_no_process_grants() {
    let policy = CapabilityPolicy::default();

    assert!(policy.process_sandbox.unix_socket_roots.is_empty());
    assert!(policy.process_sandbox.read_roots.is_empty());
    assert!(!policy.process_sandbox.allow_tcp_loopback);
    assert!(policy.process_sandbox.presets.is_none());
}
