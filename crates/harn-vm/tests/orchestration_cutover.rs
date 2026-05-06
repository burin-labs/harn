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
        "spawn_agent",
        "sub_agent_run",
        "send_input",
        "worker_trigger",
        "wait_agent",
        "close_agent",
        "resume_agent",
        "list_agents",
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
        "__host_agent_session_init",
        "__host_agent_session_finalize",
        "__host_agent_session_messages",
        "__host_agent_session_record_assistant",
        "__host_agent_session_record_tool_results",
        "__host_agent_session_record_usage",
        "__host_agent_session_drain_feedback",
        "__host_agent_session_totals",
        "__host_agent_session_inject_feedback",
        "__host_agent_session_set_active_skills",
        "__host_agent_session_active_skills",
        "__host_agent_session_compact_if_needed",
        "__host_agent_session_replace_messages",
        "__host_agent_session_claim_tool_format",
        "__host_agent_budget_pre_call_blocked",
        "__host_agent_emit_event",
        "__host_skill_score",
        "__host_agent_capture_events",
        "__host_agent_parse_tool_calls",
        "__host_agent_dispatch_tool_call",
        "__host_agent_dispatch_tool_batch",
        "__host_sub_agent_run",
        "__host_worker_spawn",
        "__host_worker_send_input",
        "__host_worker_trigger",
        "__host_worker_wait",
        "__host_worker_close",
        "__host_worker_resume",
        "__host_worker_list",
        "__host_workflow_prepare_run",
        "__host_workflow_stage_prepare",
        "__host_workflow_stage_complete",
        "__host_workflow_map_branch_artifact",
        "__host_workflow_map_execute_branch",
        "__host_workflow_map_finalize",
        "__host_workflow_map_plan",
        "__host_workflow_record_transitions",
        "__host_workflow_finalize_run",
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
