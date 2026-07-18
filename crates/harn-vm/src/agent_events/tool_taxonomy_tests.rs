use super::*;

#[test]
fn tool_call_error_category_serializes_as_snake_case() {
    let pairs = [
        (ToolCallErrorCategory::SchemaValidation, "schema_validation"),
        (ToolCallErrorCategory::ToolError, "tool_error"),
        (ToolCallErrorCategory::McpServerError, "mcp_server_error"),
        (ToolCallErrorCategory::HostBridgeError, "host_bridge_error"),
        (ToolCallErrorCategory::PermissionDenied, "permission_denied"),
        (ToolCallErrorCategory::RejectedLoop, "rejected_loop"),
        (ToolCallErrorCategory::ParseAborted, "parse_aborted"),
        (ToolCallErrorCategory::Timeout, "timeout"),
        (ToolCallErrorCategory::Network, "network"),
        (ToolCallErrorCategory::ResourceBusy, "resource_busy"),
        (ToolCallErrorCategory::Cancelled, "cancelled"),
        (
            ToolCallErrorCategory::AbandonedAtLoopExit,
            "abandoned_at_loop_exit",
        ),
        (ToolCallErrorCategory::Unknown, "unknown"),
    ];
    for (variant, wire) in pairs {
        let encoded = serde_json::to_string(&variant).unwrap();
        assert_eq!(encoded, format!("\"{wire}\""));
        assert_eq!(variant.as_str(), wire);
        let decoded: ToolCallErrorCategory = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, variant);
    }
}

#[test]
fn denial_gate_serializes_as_snake_case() {
    let pairs = [
        (DenialGate::ToolCeiling, "tool_ceiling"),
        (DenialGate::MalformedToolWrapper, "malformed_tool_wrapper"),
        (DenialGate::CapabilityCeiling, "capability_ceiling"),
        (DenialGate::SideEffectCeiling, "side_effect_ceiling"),
        (DenialGate::ArgConstraint, "arg_constraint"),
        (DenialGate::DynamicPermission, "dynamic_permission"),
        (DenialGate::ApprovalPolicy, "approval_policy"),
        (DenialGate::ApprovalUnavailable, "approval_unavailable"),
        (DenialGate::HostRejected, "host_rejected"),
        (DenialGate::HookDeny, "hook_deny"),
        (DenialGate::Unknown, "unknown"),
    ];
    assert_eq!(pairs.len(), DenialGate::ALL.len());
    for (variant, wire) in pairs {
        let encoded = serde_json::to_string(&variant).unwrap();
        assert_eq!(encoded, format!("\"{wire}\""));
        assert_eq!(variant.as_str(), wire);
        let decoded: DenialGate = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, variant);
    }
}

#[test]
fn tool_denial_serializes_terminal_record_and_skips_empty_fields() {
    let denial = ToolDenial::terminal(
        DenialGate::CapabilityCeiling,
        Some("workspace.write_text".to_string()),
        "tool 'edit' exceeds capability ceiling: workspace.write_text",
    );
    let json = denial.to_json();
    assert_eq!(json["gate"], "capability_ceiling");
    assert_eq!(json["capability"], "workspace.write_text");
    assert_eq!(json["retryable"], false);
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .contains("capability ceiling"));
    let bare = ToolDenial::terminal(DenialGate::ToolCeiling, None, "exceeds tool ceiling");
    let bare_json = bare.to_json();
    assert!(bare_json.get("denied_paths").is_none());
    assert!(bare_json.get("capability").is_none());
    let recovered: ToolDenial = serde_json::from_value(denial.to_json()).unwrap();
    assert_eq!(recovered, denial);
}

#[test]
fn tool_executor_round_trips_with_adjacent_tag() {
    for executor in [
        ToolExecutor::HarnBuiltin,
        ToolExecutor::HostBridge,
        ToolExecutor::McpServer {
            server_name: "linear".to_string(),
        },
        ToolExecutor::ProviderNative,
    ] {
        let json = serde_json::to_value(&executor).unwrap();
        let kind = json.get("kind").and_then(|v| v.as_str()).unwrap();
        match &executor {
            ToolExecutor::HarnBuiltin => assert_eq!(kind, "harn_builtin"),
            ToolExecutor::HostBridge => assert_eq!(kind, "host_bridge"),
            ToolExecutor::McpServer { server_name } => {
                assert_eq!(kind, "mcp_server");
                assert_eq!(json["server_name"], *server_name);
            }
            ToolExecutor::ProviderNative => assert_eq!(kind, "provider_native"),
        }
        let recovered: ToolExecutor = serde_json::from_value(json).unwrap();
        assert_eq!(recovered, executor);
    }
}

#[test]
fn tool_call_error_category_from_internal_collapses_transient_family() {
    use crate::value::ErrorCategory as Internal;
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::Timeout),
        ToolCallErrorCategory::Timeout
    );
    for net in [
        Internal::RateLimit,
        Internal::Overloaded,
        Internal::ServerError,
        Internal::TransientNetwork,
    ] {
        assert_eq!(
            ToolCallErrorCategory::from_internal(&net),
            ToolCallErrorCategory::Network,
            "{net:?} should map to Network",
        );
    }
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::ResourceBusy),
        ToolCallErrorCategory::ResourceBusy
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::SchemaValidation),
        ToolCallErrorCategory::SchemaValidation
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::ToolError),
        ToolCallErrorCategory::ToolError
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::ToolRejected),
        ToolCallErrorCategory::PermissionDenied
    );
    assert_eq!(
        ToolCallErrorCategory::from_internal(&Internal::Cancelled),
        ToolCallErrorCategory::Cancelled
    );
    for bridge in [
        Internal::Auth,
        Internal::EgressBlocked,
        Internal::NotFound,
        Internal::CircuitOpen,
        Internal::Generic,
    ] {
        assert_eq!(
            ToolCallErrorCategory::from_internal(&bridge),
            ToolCallErrorCategory::HostBridgeError,
            "{bridge:?} should map to HostBridgeError",
        );
    }
}

#[test]
fn only_schema_validation_is_recoverable() {
    assert!(ToolCallErrorCategory::SchemaValidation.is_recoverable());
    for category in ToolCallErrorCategory::ALL {
        if matches!(category, ToolCallErrorCategory::SchemaValidation) {
            continue;
        }
        assert!(
            !category.is_recoverable(),
            "{category:?} must not be treated as recoverable"
        );
    }
}
