use harn_vm::orchestration::{CapabilityPolicy, ProcessSandboxPolicy};

#[test]
fn downstream_hosts_can_use_the_v010132_process_policy_shape() {
    let process_sandbox = ProcessSandboxPolicy {
        presets: None,
        read_roots: Vec::new(),
        write_roots: Vec::new(),
        read_deny_roots: Vec::new(),
        allow_tcp_loopback: false,
    };
    let policy = CapabilityPolicy {
        process_sandbox,
        ..CapabilityPolicy::default()
    };

    assert_eq!(policy.process_sandbox, ProcessSandboxPolicy::default());
}
