use super::*;

#[test]
fn network_statuses_match_command_response_schemas() {
    use harn_vm::process_sandbox::ProcessSandboxNetwork;
    for name in ["run_command", "wait_command"] {
        let source = harn_hostlib::schemas::lookup(
            "tools",
            name,
            harn_hostlib::schemas::SchemaKind::Response,
        )
        .unwrap();
        let schema: serde_json::Value = serde_json::from_str(source).unwrap();
        let network = &schema["properties"]["sandbox"]["properties"]["network"];
        assert!(
            network.is_object(),
            "the command schema must define network metadata"
        );
        let validator = jsonschema::validator_for(network).unwrap();
        for status in [
            ProcessSandboxNetwork::Policy {
                direct_network: false,
                tcp_loopback: false,
                managed_proxy: false,
            },
            ProcessSandboxNetwork::Inherited,
            ProcessSandboxNetwork::Unavailable,
            ProcessSandboxNetwork::Unknown,
        ] {
            assert!(validator.is_valid(&serde_json::to_value(status).unwrap()));
        }
        assert!(!validator.is_valid(&serde_json::json!({"status": "policy"})));
        assert!(
            !validator.is_valid(&serde_json::json!({"status": "unknown", "direct_network": false}))
        );
    }
}

fn install_network_policy(
    root: &std::path::Path,
    network_ceiling: bool,
    proxy: bool,
) -> ExecutionPolicyGuard {
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![root.to_string_lossy().into_owned()],
        side_effect_level: Some(
            if network_ceiling {
                "network"
            } else {
                "process_exec"
            }
            .into(),
        ),
        process_network_proxy: proxy.then_some(harn_vm::orchestration::ProcessNetworkProxy {
            http_port: 3128,
            socks_port: 1080,
        }),
        ..CapabilityPolicy::default()
    });
    ExecutionPolicyGuard
}

fn command_request(root: &std::path::Path) -> harn_vm::value::DictMap {
    let mut request = dict_map();
    request.insert("argv".into(), vlist_str(&["fixture", "network-receipt"]));
    request.insert("cwd".into(), vm_string(root.to_string_lossy().as_ref()));
    request
}

#[test]
fn command_receipts_distinguish_denied_public_and_proxy_grants() {
    let workspace = tempdir().unwrap();
    for (network_ceiling, proxy, [direct_network, tcp_loopback, managed_proxy]) in [
        (false, false, [false, false, false]),
        (true, false, [true, true, false]),
        (true, true, [false, false, true]),
    ] {
        let _policy = install_network_policy(workspace.path(), network_ceiling, proxy);
        let (spawner, _controller, _guard) =
            install_mock_with(MockProcessConfig::with_stdout(0, "network-receipt-control"));
        let result = call_tool(
            "hostlib_tools_run_command",
            command_request(workspace.path()),
        )
        .unwrap();
        assert_response_matches_schema("run_command", &result);
        let result = require_dict(result);
        assert_eq!(require_str(&result, "stdout"), "network-receipt-control");
        assert_eq!(require_int(&result, "exit_code"), 0);
        assert_eq!(spawner.captured().len(), 1);
        require_nil(&result, "denial");
        let sandbox = require_nested_dict(&result, "sandbox");
        assert!(require_bool(&sandbox, "enforced"));
        assert_eq!(require_str(&sandbox, "denial_reporting"), "inferred_only");
        let network = require_nested_dict(&sandbox, "network");
        assert_eq!(require_str(&network, "status"), "policy");
        assert_eq!(require_bool(&network, "direct_network"), direct_network);
        assert_eq!(require_bool(&network, "tcp_loopback"), tcp_loopback);
        assert_eq!(require_bool(&network, "managed_proxy"), managed_proxy);
    }
}

#[test]
fn background_completion_keeps_network_grants_after_policy_changes() {
    let workspace = tempdir().unwrap();
    let session_id = unique_session_id("network-receipt-policy-change");
    let _session = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let original_policy = install_network_policy(workspace.path(), true, true);
    let (spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());
    let mut request = command_request(workspace.path());
    request.insert("background".into(), VmValue::Bool(true));
    let started = call_tool("hostlib_tools_run_command", request).unwrap();
    assert_response_matches_schema("run_command", &started);
    let started = require_dict(started);
    let handle_id = require_str(&started, "handle_id");
    let completion = register_completion_notifier(&handle_id).expect("live command");
    let original_sandbox = require_nested_dict(&started, "sandbox");
    let original_network = require_nested_dict(&original_sandbox, "network");
    assert!(!require_bool(&original_network, "direct_network"));
    assert!(!require_bool(&original_network, "tcp_loopback"));
    assert!(require_bool(&original_network, "managed_proxy"));
    assert_eq!(spawner.captured().len(), 1);

    drop(original_policy);
    let _later_policy = install_network_policy(workspace.path(), false, false);
    controller.append_stdout(b"completed-after-policy-change");
    controller.complete_with(ExitStatus::from_code(0));
    completion.recv().expect("waiter completed");

    let mut request = dict_map();
    request.insert("handle_id".into(), vm_string(&handle_id));
    let waited = call_tool("hostlib_tools_wait_command", request).unwrap();
    assert_response_matches_schema("wait_command", &waited);
    let waited = require_dict(waited);
    assert_eq!(
        require_str(&waited, "stdout"),
        "completed-after-policy-change"
    );
    assert_eq!(require_int(&waited, "exit_code"), 0);
    assert_eq!(
        harn_vm::value::json::vm_value_dict_to_json(&require_nested_dict(&waited, "sandbox")),
        harn_vm::value::json::vm_value_dict_to_json(&original_sandbox)
    );
}
