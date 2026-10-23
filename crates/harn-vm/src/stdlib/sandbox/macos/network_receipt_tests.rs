use super::*;
use crate::stdlib::sandbox::{ProcessSandboxDenialReporting, ProcessSandboxNetwork};

#[test]
fn network_receipt_matches_the_rendered_process_grant() {
    let proxy = crate::orchestration::ProcessNetworkProxy {
        http_port: 3128,
        socks_port: 1080,
    };
    for (ceiling, loopback, proxy, [direct_network, tcp_loopback, managed_proxy], proxy_port) in [
        ("process_exec", false, None, [false, false, false], false),
        ("network", false, None, [true, true, false], false),
        ("network", true, None, [false, true, false], false),
        ("network", false, Some(proxy), [false, false, true], true),
        (
            "process_exec",
            false,
            Some(proxy),
            [false, false, false],
            false,
        ),
        ("network", true, Some(proxy), [false, true, true], false),
        (
            "process_exec",
            true,
            Some(proxy),
            [false, true, true],
            false,
        ),
    ] {
        let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
        policy.side_effect_level = Some(ceiling.into());
        policy.process_sandbox.allow_tcp_loopback = loopback;
        policy.process_network_proxy = proxy;
        let report = ProcessSandboxNetwork::capture(
            ProcessSandboxDenialReporting::InferredOnly,
            Some(&policy),
            false,
        );
        assert_eq!(
            report,
            ProcessSandboxNetwork::Policy {
                direct_network,
                tcp_loopback,
                managed_proxy
            }
        );
        let profile = render_profile(&policy);
        assert_eq!(
            profile.contains("(allow network*)"),
            direct_network,
            "{profile}"
        );
        assert_eq!(
            profile.contains("(remote ip \"localhost:*\")"),
            loopback,
            "{profile}"
        );
        assert_eq!(
            profile.contains("(remote ip \"localhost:3128\")"),
            proxy_port,
            "{profile}"
        );
    }
}

#[test]
fn loopback_grant_keeps_managed_proxy_routes_at_both_host_ceilings() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let mut probe_policy = macos_policy_with_workspace_ops(&[]);
    probe_policy.workspace_roots = vec![std::env::current_dir().unwrap().display().to_string()];
    let availability = Backend::run_to_output(
        "/usr/bin/true",
        &[],
        &ProcessCommandConfig::default(),
        &probe_policy,
        SandboxProfile::Worktree,
    );
    if crate::sandbox_host::assert_unavailable_result(&availability) {
        eprintln!("network proxy routes not_measured: host process confinement unavailable");
        return;
    }
    assert!(availability
        .expect("sandboxed host control")
        .status
        .success());

    for ceiling in ["network", "process_exec"] {
        let allowed = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let allowed_port = allowed.local_addr().unwrap().port();
        let denied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let denied_port = denied.local_addr().unwrap().port();
        denied.set_nonblocking(true).unwrap();
        let server = crate::runtime_stack::spawn(move || {
            let (mut stream, _) = allowed.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            assert_eq!(line, "GET /allowed HTTP/1.1\r\n");
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\nnetwork-route-control").unwrap();
        });
        let proxy = crate::egress::ProcessEgressProxy::start_allowlist(&[format!(
            "127.0.0.1:{allowed_port}"
        )])
        .unwrap();
        let mut policy = macos_policy_with_workspace_ops(&["read_text"]);
        policy.workspace_roots = vec![std::env::current_dir().unwrap().display().to_string()];
        policy.side_effect_level = Some(ceiling.into());
        policy.process_sandbox.allow_tcp_loopback = true;
        policy.process_network_proxy = Some(proxy.endpoints());
        let report = ProcessSandboxNetwork::capture(
            ProcessSandboxDenialReporting::InferredOnly,
            Some(&policy),
            false,
        );
        assert_eq!(
            report,
            ProcessSandboxNetwork::Policy {
                direct_network: false,
                tcp_loopback: true,
                managed_proxy: true
            }
        );
        let mut environment = Command::new("/usr/bin/curl");
        super::super::super::process_output::apply_process_config(
            &mut environment,
            &ProcessCommandConfig::default(),
            Some(&policy),
        );
        assert!(environment
            .get_envs()
            .any(|(key, value)| key == "http_proxy"
                && value
                    == Some(std::ffi::OsStr::new(&format!(
                        "http://127.0.0.1:{}",
                        proxy.endpoints().http_port
                    )))));

        let run = |port, path| {
            Backend::run_to_output(
                "/usr/bin/curl",
                &strings([
                    "--silent",
                    "--show-error",
                    "--fail",
                    "--max-time",
                    "10",
                    &format!("http://127.0.0.1:{port}/{path}"),
                ]),
                &ProcessCommandConfig::default(),
                &policy,
                SandboxProfile::OsHardened,
            )
            .expect("sandboxed curl")
        };
        let output = run(allowed_port, "allowed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"network-route-control");
        server.join().unwrap();
        let output = run(denied_port, "denied");
        assert_eq!(output.status.code(), Some(22));
        assert!(
            matches!(denied.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        let audit = proxy.audit_snapshot();
        assert_eq!(audit.len(), 2);
        assert!(audit[0].allowed);
        assert!(!audit[1].allowed);
    }
}
