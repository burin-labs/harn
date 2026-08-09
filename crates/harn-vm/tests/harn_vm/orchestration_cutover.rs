#[test]
fn stdlib_facades_are_imported_and_host_primitives_are_discoverable() {
    let metadata = harn_vm::stdlib::stdlib_builtin_metadata()
        .into_iter()
        .map(|entry| (entry.name().to_string(), entry))
        .collect::<std::collections::BTreeMap<_, _>>();

    // These are Harn stdlib functions, not ambient runtime builtins. Keeping
    // them out of the builtin registry makes imports pure and forces their
    // effects to flow through the nominal handles in their signatures.
    for name in [
        "agent_loop",
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
        "workflow_execute",
    ] {
        assert!(
            !metadata.contains_key(name),
            "{name} must remain an imported stdlib facade, not ambient authority"
        );
    }

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
        "__host_agent_session_post_event",
        "__host_agent_session_pending_injections",
        "__host_agent_session_revoke_reminder",
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
        "__host_worker_stop",
        "__host_worker_suspend",
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
        "__host_stage_select_artifacts",
        "__host_stage_execute_once",
        "__host_stage_record_attempt",
        "__host_llm_usage_snapshot",
        "__host_llm_usage_delta",
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
