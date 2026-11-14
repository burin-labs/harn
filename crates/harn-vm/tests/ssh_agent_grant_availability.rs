//! Exercise the same public surface a native embedding host imports.

use harn_vm::orchestration::CapabilityPolicy;
use harn_vm::process_sandbox::ssh_agent_grant_available;
use harn_vm::{ErrorCategory, VmError};

fn refusal(error: VmError) -> String {
    match error {
        VmError::CategorizedError {
            category: ErrorCategory::ToolRejected,
            message,
        } => message,
        other => panic!("expected a categorized grant refusal, got {other}"),
    }
}

#[test]
fn an_absent_grant_needs_no_host_support() {
    let policy = CapabilityPolicy::default();
    assert!(ssh_agent_grant_available(&policy).is_ok());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_embedder_checks_real_socket_metadata_without_connecting_or_mutating_policy() {
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut policy = CapabilityPolicy::default();
    policy.process_sandbox.ssh_agent_socket = Some(socket.to_str().unwrap().into());
    let before = policy.clone();
    ssh_agent_grant_available(&policy).unwrap();
    assert_eq!(policy, before);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    let file = directory.path().join("ordinary-file");
    std::fs::write(&file, b"not a socket").unwrap();
    for path in [
        std::path::PathBuf::from("relative-agent.sock"),
        directory.path().join("missing.sock"),
        file,
    ] {
        policy.process_sandbox.ssh_agent_socket = Some(path.to_str().unwrap().into());
        assert_eq!(
            refusal(ssh_agent_grant_available(&policy).unwrap_err()),
            "SSH agent grant requires an existing absolute Unix socket path"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_no_network_proposal_reports_the_measured_host_decision() {
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("agent.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let mut policy = CapabilityPolicy {
        side_effect_level: Some("process_exec".into()),
        ..CapabilityPolicy::default()
    };
    policy.process_sandbox.ssh_agent_socket = Some(socket.to_str().unwrap().into());
    let before = policy.clone();
    match ssh_agent_grant_available(&policy) {
        Ok(()) => eprintln!("SSH agent preflight: measured no-network grant held"),
        Err(error) => {
            assert!(cfg!(target_os = "linux"));
            let reason = refusal(error);
            assert_eq!(
                reason,
                "SSH agent socket connection requires Landlock ABI 9 with measured enforcement on this host"
            );
            eprintln!("SSH agent preflight: measured no-network grant refused: {reason}");
        }
    }
    assert_eq!(policy, before);
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn an_unsupported_backend_returns_the_existing_grant_refusal() {
    let mut policy = CapabilityPolicy::default();
    policy.process_sandbox.ssh_agent_socket = Some("/proposed/agent.sock".into());
    assert_eq!(
        refusal(ssh_agent_grant_available(&policy).unwrap_err()),
        "this process sandbox cannot enforce an SSH agent socket grant"
    );
}
