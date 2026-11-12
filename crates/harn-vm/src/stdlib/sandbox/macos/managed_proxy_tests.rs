use super::*;
use std::net::{SocketAddr, TcpListener, TcpStream};

#[test]
fn loopback_grant_keeps_listeners_without_bypassing_a_managed_proxy() {
    const CHILD: &str = "HARN_TEST_MANAGED_LOOPBACK_TARGETS";
    const PREFIX: &str = "MANAGED_LOOPBACK_RESULT ";
    const NAME: &str = "stdlib::sandbox::macos::tests::managed_proxy::loopback_grant_keeps_listeners_without_bypassing_a_managed_proxy";
    if let Ok(raw) = std::env::var(CHILD) {
        let addresses: Vec<SocketAddr> = serde_json::from_str(&raw).unwrap();
        let _listener =
            TcpListener::bind("127.0.0.1:0").expect("loopback listener bind remains permitted");
        let reached: Vec<bool> = addresses
            .iter()
            .map(|address| TcpStream::connect(address).is_ok())
            .collect();
        println!("{PREFIX}{}", serde_json::to_string(&reached).unwrap());
        return;
    }

    let listeners: Vec<TcpListener> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let addresses: Vec<SocketAddr> = listeners
        .iter()
        .map(|listener| listener.local_addr().unwrap())
        .collect();
    for address in &addresses {
        TcpStream::connect(address)
            .expect("every measured endpoint is reachable without confinement");
    }
    let workspace = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("network".into());
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    policy.read_only_roots = vec![executable.parent().unwrap().display().to_string()];
    policy.process_sandbox.allow_tcp_loopback = true;
    let config = ProcessCommandConfig {
        cwd: Some(workspace.path().to_path_buf()),
        env: vec![(CHILD.into(), serde_json::to_string(&addresses).unwrap())],
        ..Default::default()
    };
    for (proxy, expected) in [
        (None, vec![true, true, true]),
        (
            Some(crate::orchestration::ProcessNetworkProxy {
                http_port: addresses[0].port(),
                socks_port: addresses[1].port(),
            }),
            vec![true, true, false],
        ),
    ] {
        policy.process_network_proxy = proxy;
        let output = Backend::run_to_output(
            &executable.display().to_string(),
            &strings([NAME, "--exact", "--nocapture", "--test-threads=1"]),
            &config,
            &policy,
            SandboxProfile::OsHardened,
        )
        .expect("the Mac backend must install the requested policy");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let measured: Vec<bool> = serde_json::from_str(
            stdout
                .lines()
                .find_map(|line| line.split_once(PREFIX).map(|(_, result)| result))
                .unwrap_or_else(|| panic!("child never reported its connections: {stdout}")),
        )
        .unwrap();
        assert_eq!(measured, expected, "proxy={proxy:?}");
    }
}
