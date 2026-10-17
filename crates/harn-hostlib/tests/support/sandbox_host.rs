use harn_vm as vm;
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../harn-vm/tests/support/sandbox_host.rs"));

pub fn assert_unavailable_tool<T>(result: &Result<T, host::HostlibError>) -> bool {
    let Some(expected) = unavailable_worktree_sandbox() else { return false };
    match result {
        Err(host::HostlibError::SandboxMechanismUnavailable { refusal, .. }) => assert_eq!(refusal.as_ref(), &expected),
        Err(error) => panic!("expected measured mechanism refusal {expected}, got {error}"),
        Ok(_) => panic!("unsupported host executed a tool instead of refusing {expected}"),
    }
    true
}

pub fn assert_unavailable_local<T>(result: &Result<T, host::sandbox::SandboxError>) -> bool {
    let Some(mut expected) = unavailable_worktree_sandbox() else { return false };
    expected.profile = vm::orchestration::SandboxProfile::OsHardened;
    expected.requirement = vm::process_sandbox::SandboxRequirement::Profile;
    let expected = expected.into_error();
    match result {
        Err(host::sandbox::SandboxError::Exec(message)) => assert_eq!(message, &format!("harn-vm process sandbox rejected exec: {expected}")),
        Err(error) => panic!("expected measured mechanism refusal {expected}, got {error}"),
        Ok(_) => panic!("unsupported host executed a local command instead of refusing {expected}"),
    }
    true
}
