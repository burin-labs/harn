use super::*;
use crate::stdlib::sandbox;

struct Restore {
    socket: Option<std::ffi::OsString>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        match self.socket.take() {
            Some(value) => std::env::set_var("SSH_AUTH_SOCK", value),
            None => std::env::remove_var("SSH_AUTH_SOCK"),
        }
        crate::reset_thread_local_state();
    }
}

fn read_socket(command: &mut std::process::Command) -> String {
    let output = command
        .args(["-c", "printf '%s' \"$SSH_AUTH_SOCK\""])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn selected_agent_reaches_every_inherited_spawn_without_a_session_environment() {
    let _lock = crate::runtime_paths::test_env_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::reset_thread_local_state();
    let _restore = Restore {
        socket: std::env::var_os("SSH_AUTH_SOCK"),
    };
    std::env::set_var("SSH_AUTH_SOCK", "/ambient/other-agent.sock");
    assert!(current_session_environment().is_none());
    let mut policy = crate::orchestration::CapabilityPolicy {
        sandbox_profile: crate::orchestration::SandboxProfile::Unrestricted,
        ..Default::default()
    };
    policy.process_sandbox.ssh_agent_socket = Some("/host/selected-agent.sock".into());
    crate::orchestration::push_execution_policy(policy);
    let args = vec!["-c".into(), "printf '%s' \"$SSH_AUTH_SOCK\"".into()];
    let expected = "/host/selected-agent.sock";
    assert_eq!(
        read_socket(&mut sandbox::session_std_command("/bin/sh").unwrap()),
        expected
    );
    let output = sandbox::std_command_for("/bin/sh", &args)
        .unwrap()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    let output = sandbox::command_output("/bin/sh", &args, &Default::default()).unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    let captured = run_captured_spawn(CapturedSpawn {
        label: "agent environment proof",
        cmd: "/bin/sh",
        args: &args,
        cwd: None,
        env: &[],
        env_clear: false,
        stdin: None,
        timeout: None,
    })
    .unwrap();
    assert!(captured.output.status.success());
    assert_eq!(String::from_utf8(captured.output.stdout).unwrap(), expected);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let output = sandbox::session_tokio_command("/bin/sh")
                .unwrap()
                .args(&args)
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            let output = sandbox::tokio_command_for("/bin/sh", &args)
                .unwrap()
                .output()
                .await
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        });
    let mut caller = sandbox::session_std_command("/bin/sh").unwrap();
    caller.env("SSH_AUTH_SOCK", "/caller/alternate.sock");
    assert_eq!(read_socket(&mut caller), "/caller/alternate.sock");
    crate::orchestration::pop_execution_policy();
    assert_eq!(
        read_socket(&mut sandbox::session_std_command("/bin/sh").unwrap()),
        "/ambient/other-agent.sock"
    );
}
