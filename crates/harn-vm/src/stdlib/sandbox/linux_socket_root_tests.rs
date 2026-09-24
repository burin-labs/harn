//! A named Unix-socket root takes a socket file whatever else the policy
//! grants.

use super::tests::live_landlock_available;
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPolicy, SandboxProfile};
use crate::stdlib::sandbox::{command_output, ProcessCommandConfig};

/// Bind a Unix socket at `socket` under `policy`, returning whether it bound.
fn binds_under(policy: CapabilityPolicy, socket: &std::path::Path) -> bool {
    crate::orchestration::push_execution_policy(policy);
    let output = command_output(
        "/usr/bin/perl",
        &[
            "-MSocket".to_string(),
            "-e".to_string(),
            "socket(S, PF_UNIX, SOCK_STREAM, 0) or die \"socket: $!\"; \
             bind(S, sockaddr_un($ARGV[0])) or die \"bind: $!\";"
                .to_string(),
            socket.display().to_string(),
        ],
        &ProcessCommandConfig::default(),
    );
    crate::orchestration::pop_execution_policy();
    matches!(output, Ok(out) if out.status.success()) && socket.exists()
}

/// A policy permitting networking, with one socket root outside every
/// writable root, binds a socket file under that root.
///
/// # The defect
///
/// The rule granting socket-file creation on the named roots was installed
/// only on the serve-only path. A policy that also permitted networking got
/// no rule for them, so the root the operator named for exactly this purpose
/// refused the bind, while the receipt reported the grant as superseded.
///
/// # Reading it
///
/// The sibling bind is the negative control: the same policy must still
/// refuse a socket outside the root, so the pass cannot come from a blanket
/// grant.
#[test]
fn a_network_policy_still_admits_socket_files_under_its_socket_root() {
    if !live_landlock_available("socket-root-with-network") {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    let base = root.path().canonicalize().expect("canonical root");
    let workspace = base.join("workspace");
    let sockets = base.join("sockets");
    let elsewhere = base.join("elsewhere");
    for dir in [&workspace, &sockets, &elsewhere] {
        std::fs::create_dir_all(dir).expect("create dir");
    }
    let policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.display().to_string()],
        side_effect_level: Some("network".to_string()),
        process_sandbox: Box::new(ProcessSandboxPolicy {
            unix_socket_roots: vec![sockets.display().to_string()],
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    };

    assert!(
        binds_under(policy.clone(), &sockets.join("build.sock")),
        "a socket root outside every writable root must take a socket file on a \
         network-permitting policy"
    );
    assert!(
        !binds_under(policy, &elsewhere.join("build.sock")),
        "a socket outside the root must still be refused, or the bind above proves nothing"
    );
}
