use super::*;
use crate::stdlib::sandbox::{ProcessSandboxDenialReporting, ProcessSandboxNetwork};

#[test]
fn externally_confined_child_preserves_network_uncertainty_and_denial_coverage() {
    use crate::process_sandbox::current_process_confinement;
    use crate::stdlib::sandbox::ProcessSandboxReportingContext;

    const CHILD_WORKSPACE: &str = "NETWORK_RECEIPT_EXTERNAL_CHILD_WORKSPACE";
    const RESULT_PREFIX: &str = "EXTERNAL_NETWORK_RECEIPT ";
    if let Some(workspace) = std::env::var_os(CHILD_WORKSPACE) {
        assert!(current_process_confinement().is_none());
        assert!(Backend::inherits_network_grants());
        assert!(super::super::nested::outer_denies("network-outbound", None));
        let no_policy = ProcessSandboxReportingContext::current();
        assert!(no_policy.policy.is_none());
        assert_eq!(
            no_policy.reporting,
            ProcessSandboxDenialReporting::NotEnforced
        );
        assert_eq!(no_policy.network, ProcessSandboxNetwork::Unknown);
        let mut requested = macos_policy_with_workspace_ops(&[]);
        requested.workspace_roots = vec![std::path::PathBuf::from(workspace).display().to_string()];
        requested.side_effect_level = Some("network".into());
        crate::orchestration::push_execution_policy(requested.clone());
        let context = ProcessSandboxReportingContext::current();
        assert!(context.policy.is_some());
        assert_eq!(
            context.reporting,
            ProcessSandboxDenialReporting::InferredOnly
        );
        assert_eq!(context.network, ProcessSandboxNetwork::Inherited);
        let output = Backend::run_to_output(
            "/bin/echo",
            &strings(["external-inherited-command-reached"]),
            &ProcessCommandConfig::default(),
            &requested,
            SandboxProfile::Worktree,
        )
        .expect("spawn under the external profile");
        crate::orchestration::pop_execution_policy();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"external-inherited-command-reached\n");
        println!(
            "{RESULT_PREFIX}{}",
            serde_json::to_value(context.report()).unwrap()
        );
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let exe = std::env::current_exe().expect("test binary");
    let mut outer = macos_policy_with_workspace_ops(&[]);
    outer.workspace_roots = vec![workspace.path().display().to_string()];
    outer.read_only_roots = vec![exe
        .ancestors()
        .nth(3)
        .expect("build root")
        .display()
        .to_string()];
    outer.side_effect_level = Some("process_exec".into());
    let availability = Backend::run_to_output(
        "/usr/bin/true",
        &[],
        &ProcessCommandConfig::default(),
        &outer,
        SandboxProfile::Worktree,
    );
    if crate::sandbox_host::assert_unavailable_result(&availability) {
        eprintln!("external network receipt not_measured: host process confinement unavailable");
        return;
    }
    assert!(availability
        .expect("sandboxed host control")
        .status
        .success());
    let output = Command::new(SANDBOX_EXEC_PATH)
        .arg("-p")
        .arg(render_profile_for_program(&outer, &exe.display().to_string()))
        .arg(&exe)
        .args([
            "stdlib::sandbox::macos::tests::network_receipt::externally_confined_child_preserves_network_uncertainty_and_denial_coverage",
            "--exact", "--nocapture", "--test-threads=1",
        ])
        .env(CHILD_WORKSPACE, workspace.path())
        .current_dir(workspace.path())
        .output().expect("run the externally confined case");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("child stdout");
    let report: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.split_once(RESULT_PREFIX).map(|(_, json)| json))
            .unwrap_or_else(|| panic!("actual child receipt missing from stdout: {stdout}")),
    )
    .expect("child receipt JSON");
    assert_eq!(
        report["network"],
        serde_json::json!({"status": "inherited"})
    );
    assert_eq!(report["enforced"], true);
}

#[test]
fn self_confined_child_reports_inherited_network_with_an_active_policy() {
    use crate::process_sandbox::confine_current_process;
    use crate::stdlib::sandbox::ProcessSandboxReportingContext;

    const CHILD_WORKSPACE: &str = "NETWORK_RECEIPT_CONFINED_CHILD_WORKSPACE";
    const RESULT_PREFIX: &str = "INHERITED_NETWORK_RECEIPT ";
    if let Some(workspace) = std::env::var_os(CHILD_WORKSPACE) {
        let mut outer = macos_policy_with_workspace_ops(&[]);
        outer.workspace_roots = vec![std::path::PathBuf::from(workspace).display().to_string()];
        outer.side_effect_level = Some("process_exec".into());
        confine_current_process(&outer).expect("confine the test child");
        assert!(super::super::nested::outer_denies("network-outbound", None));
        let mut requested = outer;
        requested.side_effect_level = Some("network".into());
        crate::orchestration::push_execution_policy(requested.clone());
        let context = ProcessSandboxReportingContext::current();
        assert!(context.policy.is_some());
        assert_eq!(context.network, ProcessSandboxNetwork::Inherited);
        let output = Backend::run_to_output(
            "/bin/echo",
            &strings(["inherited-command-reached"]),
            &ProcessCommandConfig::default(),
            &requested,
            SandboxProfile::Worktree,
        )
        .expect("spawn under the inherited profile");
        crate::orchestration::pop_execution_policy();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"inherited-command-reached\n");
        println!(
            "{RESULT_PREFIX}{}",
            serde_json::to_value(context.report()).unwrap()
        );
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let mut policy = macos_policy_with_workspace_ops(&[]);
    policy.workspace_roots = vec![workspace.path().display().to_string()];
    let availability = Backend::run_to_output(
        "/usr/bin/true",
        &[],
        &ProcessCommandConfig::default(),
        &policy,
        SandboxProfile::Worktree,
    );
    if crate::sandbox_host::assert_unavailable_result(&availability) {
        eprintln!("inherited network receipt not_measured: host process confinement unavailable");
        return;
    }
    assert!(availability
        .expect("sandboxed host control")
        .status
        .success());
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "stdlib::sandbox::macos::tests::network_receipt::self_confined_child_reports_inherited_network_with_an_active_policy",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_WORKSPACE, workspace.path())
        .current_dir(workspace.path())
        .output()
        .expect("run the isolated confinement case");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("child stdout");
    let report: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.split_once(RESULT_PREFIX).map(|(_, json)| json))
            .unwrap_or_else(|| panic!("actual child receipt missing from stdout: {stdout}")),
    )
    .expect("child receipt JSON");
    assert_eq!(
        report["network"],
        serde_json::json!({"status": "inherited"})
    );
    assert_eq!(report["enforced"], true);
}

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
