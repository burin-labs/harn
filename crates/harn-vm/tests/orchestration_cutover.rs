#[test]
fn stdlib_facades_and_host_primitives_are_discoverable() {
    let metadata = harn_vm::stdlib::stdlib_builtin_metadata()
        .into_iter()
        .map(|entry| (entry.name().to_string(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();

    for name in [
        "agent_loop",
        "agent_turn",
        "agent_parse_tool_calls",
        "agent_dispatch_tool_call",
        "agent_dispatch_tool_batch",
    ] {
        let entry = metadata
            .get(name)
            .unwrap_or_else(|| panic!("{name} must be registered"));
        assert_eq!(entry.kind(), harn_vm::VmBuiltinKind::Async);
        assert_eq!(entry.category(), Some("agent.stdlib"));
        assert!(
            entry.signature().is_some(),
            "{name} should carry registration metadata"
        );
    }

    let workflow_execute = metadata
        .get("workflow_execute")
        .expect("workflow_execute must be registered");
    assert_eq!(workflow_execute.kind(), harn_vm::VmBuiltinKind::Async);
    assert_eq!(workflow_execute.category(), Some("workflow.stdlib"));

    for name in [
        "__host_llm_session_run",
        "__host_agent_capture_events",
        "__host_agent_parse_tool_calls",
        "__host_agent_dispatch_tool_call",
        "__host_agent_dispatch_tool_batch",
        "__host_workflow_graph_run",
        "host_call",
        "host_tool_call",
        "agent_session_compact",
        "daemon_spawn",
    ] {
        let entry = metadata
            .get(name)
            .unwrap_or_else(|| panic!("{name} must be registered"));
        assert!(
            entry.signature().is_some(),
            "{name} should carry registration metadata"
        );
        assert!(
            entry.category().is_some(),
            "{name} should carry registration category metadata"
        );
    }
}
