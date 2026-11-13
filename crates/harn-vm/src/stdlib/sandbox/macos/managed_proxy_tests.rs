//! A child behind the managed proxy reaches the proxy and nothing else, measured
//! with real sockets under the real Seatbelt profile. Every other endpoint is a
//! host listener the child could otherwise use to bypass the proxy: an
//! unrelated loopback port, and the proxy's own port number over IPv6, over
//! UDP, and at the host's other IPv4 addresses.

use super::*;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

const CHILD: &str = "HARN_TEST_MANAGED_PROXY_TARGETS";
const PREFIX: &str = "MANAGED_PROXY_RESULT ";
const NAME: &str =
    "stdlib::sandbox::macos::tests::managed_proxy::managed_proxy_is_the_only_destination";

fn reached(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok()
}

fn child(targets: BTreeMap<String, SocketAddr>) {
    let mut result: BTreeMap<String, bool> = BTreeMap::new();
    for (name, address) in &targets {
        if name == "udp_same_port" {
            let sent = UdpSocket::bind("0.0.0.0:0")
                .and_then(|socket| socket.send_to(b"bypass", address))
                .is_ok();
            result.insert(name.clone(), sent);
        } else {
            result.insert(name.clone(), reached(*address));
        }
    }
    let own = TcpListener::bind("127.0.0.1:0");
    result.insert("own_listener_bind".into(), own.is_ok());
    // The listener stays open across the connect.
    let own_reached = own
        .as_ref()
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .is_some_and(reached);
    result.insert("own_listener_connect".into(), own_reached);
    println!("{PREFIX}{}", serde_json::to_string(&result).unwrap());
}

fn other_local_ipv4() -> Option<std::net::Ipv4Addr> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find_map(|interface| match interface.ip() {
            IpAddr::V4(address) if !address.is_loopback() => Some(address),
            _ => None,
        })
}

#[test]
fn managed_proxy_is_the_only_destination() {
    if let Ok(raw) = std::env::var(CHILD) {
        child(serde_json::from_str(&raw).unwrap());
        return;
    }
    let proxy = crate::egress::ProcessEgressProxy::start_allowlist(&["example.invalid:443".into()])
        .expect("proxy starts");
    let endpoints = proxy.endpoints();
    let port = endpoints.http_port;
    let unrelated = TcpListener::bind("127.0.0.1:0").unwrap();
    let ipv6 = TcpListener::bind(("::1", port)).expect("IPv6 has its own port space");
    let udp = UdpSocket::bind(("127.0.0.1", port)).expect("UDP has its own port space");
    udp.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut targets = BTreeMap::from([
        (
            "proxy_http".to_string(),
            SocketAddr::from(([127, 0, 0, 1], port)),
        ),
        (
            "proxy_socks".to_string(),
            SocketAddr::from(([127, 0, 0, 1], endpoints.socks_port)),
        ),
        (
            "unrelated_loopback".to_string(),
            unrelated.local_addr().unwrap(),
        ),
        ("ipv6_same_port".to_string(), ipv6.local_addr().unwrap()),
        ("udp_same_port".to_string(), udp.local_addr().unwrap()),
    ]);
    // Seatbelt's `localhost` includes this host's other addresses. The proxy
    // claims its port there, so the exact address is taken and a wildcard
    // listener on the port is shadowed.
    let lan = other_local_ipv4();
    let mut lan_exact_free = None;
    let _lan_listener = lan.map(|address| {
        targets.insert("lan_same_port".into(), SocketAddr::from((address, port)));
        let exact = TcpListener::bind((address, port));
        lan_exact_free = Some(exact.is_ok());
        exact.or_else(|_| TcpListener::bind(("0.0.0.0", port)))
    });
    if lan.is_none() {
        eprintln!("lan_same_port not_measured: this host has no non-loopback IPv4 address");
    }

    let workspace = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let config = ProcessCommandConfig {
        cwd: Some(workspace.path().to_path_buf()),
        env: vec![(CHILD.into(), serde_json::to_string(&targets).unwrap())],
        ..Default::default()
    };
    let mut measured = BTreeMap::new();
    for loopback in [true, false] {
        let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
        policy.side_effect_level = Some("network".into());
        policy.workspace_roots = vec![workspace.path().display().to_string()];
        policy.read_only_roots = vec![executable.parent().unwrap().display().to_string()];
        policy.process_sandbox.allow_tcp_loopback = loopback;
        policy.process_network_proxy = Some(endpoints);
        let output = Backend::run_to_output(
            &executable.display().to_string(),
            &strings([NAME, "--exact", "--nocapture", "--test-threads=1"]),
            &config,
            &policy,
            SandboxProfile::OsHardened,
        )
        .expect("the Mac backend installs the requested policy");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut result: BTreeMap<String, bool> = serde_json::from_str(
            stdout
                .lines()
                .find_map(|line| line.split_once(PREFIX).map(|(_, result)| result))
                .unwrap_or_else(|| panic!("child never reported its connections: {stdout}")),
        )
        .unwrap();
        // A datagram is reached only if it arrived.
        let mut buffer = [0u8; 16];
        result.insert("udp_same_port".into(), udp.recv_from(&mut buffer).is_ok());
        measured.insert(if loopback { "loopback" } else { "public" }, result);
    }
    drop(proxy);

    let expected = |loopback: bool| {
        let mut expected = BTreeMap::from([
            ("proxy_http".to_string(), true),
            ("proxy_socks".to_string(), true),
            ("unrelated_loopback".to_string(), false),
            ("ipv6_same_port".to_string(), false),
            ("udp_same_port".to_string(), false),
            // Listening is granted; connecting is the proxy's decision, and a
            // client that ignores the proxy cannot reach its own listener.
            ("own_listener_bind".to_string(), loopback),
            ("own_listener_connect".to_string(), false),
        ]);
        if lan.is_some() {
            expected.insert("lan_same_port".to_string(), false);
        }
        expected
    };
    let expected = BTreeMap::from([("loopback", expected(true)), ("public", expected(false))]);
    assert_eq!(
        (measured, lan_exact_free),
        (expected, lan.map(|_| false)),
        "lan={lan:?}"
    );
}

/// Ports with no live proxy behind them name whatever listens there, so the
/// profile that would grant them is never applied.
#[test]
fn managed_proxy_ports_without_a_live_proxy_are_refused_by_name() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let workspace = tempfile::tempdir().unwrap();
    let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
    policy.side_effect_level = Some("network".into());
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    policy.process_sandbox.allow_tcp_loopback = true;
    policy.process_network_proxy = Some(crate::orchestration::ProcessNetworkProxy {
        http_port: port,
        socks_port: port,
    });
    let result = Backend::run_to_output(
        "/usr/bin/true",
        &[],
        &ProcessCommandConfig::default(),
        &policy,
        SandboxProfile::OsHardened,
    );
    let error = result.expect_err("unheld proxy ports must not be granted");
    assert!(
        error.to_string().contains(&format!(
            "no managed network proxy in this process holds ports {port}"
        )),
        "{error}"
    );
}
