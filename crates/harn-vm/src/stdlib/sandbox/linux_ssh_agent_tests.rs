use super::*;

fn socket_families(policy: &CapabilityPolicy) -> i32 {
    let filter = compile_seccomp_program(policy, true).unwrap();
    // The child uses only the prepared filter and raw socket/close/exit calls.
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        if seccompiler::apply_filter(&filter).is_err() {
            unsafe { libc::_exit(64) };
        }
        let mut mask = 0;
        for (index, family) in [libc::AF_INET, libc::AF_INET6, libc::AF_UNIX]
            .into_iter()
            .enumerate()
        {
            let descriptor =
                unsafe { libc::socket(family, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
            if descriptor >= 0 {
                unsafe { libc::close(descriptor) };
                mask |= 1 << index;
            }
        }
        unsafe { libc::_exit(mask) };
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
    assert!(
        libc::WIFEXITED(status),
        "socket probe did not exit: {status}"
    );
    let mask = libc::WEXITSTATUS(status);
    assert_ne!(mask, 64, "the actual seccomp filter must be installed");
    mask
}

#[test]
fn scoped_agent_socket_preserves_the_existing_namespaced_loopback_families() {
    let mut policy = linux_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("process_exec".into());
    assert_eq!(
        socket_families(&policy),
        0,
        "no grant must deny all addressable sockets"
    );
    policy.process_sandbox.ssh_agent_socket = Some("/host/agent.sock".into());
    assert_eq!(
        socket_families(&policy),
        4,
        "agent alone admits Unix sockets only"
    );
    policy.process_sandbox.allow_tcp_loopback = true;
    policy.process_sandbox.ssh_agent_socket = None;
    assert!(namespaced_loopback_grant(&policy));
    assert_eq!(
        socket_families(&policy),
        7,
        "loopback-only positive control"
    );
    policy.process_sandbox.ssh_agent_socket = Some("/host/agent.sock".into());
    assert_eq!(
        socket_families(&policy),
        7,
        "the agent grant must preserve IPv4 and IPv6 loopback sockets"
    );
}
