const LLM_MOD_RS: &str = include_str!("../src/llm/mod.rs");
const AGENT_CONFIG_RS: &str = include_str!("../src/llm/agent_config.rs");
const WORKFLOW_REGISTER_RS: &str = include_str!("../src/stdlib/workflow/register.rs");
const AGENT_LOOP_HARN: &str = include_str!("../../harn-stdlib/src/stdlib/agent/loop.harn");
const AGENT_OPTIONS_HARN: &str = include_str!("../../harn-stdlib/src/stdlib/agent/options.harn");
const AGENT_TURN_HARN: &str = include_str!("../../harn-stdlib/src/stdlib/agent/turn.harn");
const WORKFLOW_EXECUTE_HARN: &str =
    include_str!("../../harn-stdlib/src/stdlib/workflow/execute.harn");
const WORKFLOW_CONTEXT_HARN: &str =
    include_str!("../../harn-stdlib/src/stdlib/workflow/context.harn");
const WORKFLOW_OPTIONS_HARN: &str =
    include_str!("../../harn-stdlib/src/stdlib/workflow/options.harn");
const WORKFLOW_PROMPTS_HARN: &str =
    include_str!("../../harn-stdlib/src/stdlib/workflow/prompts.harn");
const CONTRACT_PROMPT_RS: &str = include_str!("../src/llm/tools/contract_prompt.rs");
const WORKFLOW_ARTIFACTS_RS: &str = include_str!("../src/orchestration/artifacts.rs");
const WORKFLOW_RS: &str = include_str!("../src/orchestration/workflow.rs");

#[test]
fn registration_dsl_keeps_stdlib_facades_and_host_primitives_discoverable() {
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

#[test]
fn public_orchestration_entrypoints_dispatch_through_harn_stdlib() {
    assert!(
        !LLM_MOD_RS.contains("register_async_builtin(\"agent_loop\""),
        "`agent_loop` must remain a Harn stdlib export, not a direct Rust builtin"
    );
    assert!(
        !LLM_MOD_RS.contains("register_async_builtin(\"agent_turn\""),
        "`agent_turn` must remain a Harn stdlib export, not a direct Rust builtin"
    );
    assert!(
        !WORKFLOW_REGISTER_RS.contains("register_async_builtin(\"workflow_execute\""),
        "`workflow_execute` must remain a Harn stdlib export, not a direct Rust builtin"
    );
    assert!(
        AGENT_CONFIG_RS.contains("__host_llm_session_run")
            && AGENT_LOOP_HARN.contains("__host_llm_session_run"),
        "the Harn agent loop facade should call only the host LLM session primitive"
    );
    assert!(
        !AGENT_CONFIG_RS.contains("agent_loop_profile_defaults(&options, \"agent_loop\")"),
        "public agent_loop profile/default policy belongs in std/agent/options.harn"
    );
    assert!(
        !AGENT_CONFIG_RS.contains("opts.get(\"root_task\")")
            && !AGENT_CONFIG_RS.contains("opts.get(\"deliverables\")")
            && AGENT_OPTIONS_HARN.contains("__with_task_ledger_shorthand"),
        "public task-ledger shorthand belongs in std/agent/options.harn"
    );
    assert!(
        LLM_MOD_RS.contains("__host_agent_capture_events")
            && AGENT_TURN_HARN.contains("agent_capture_events"),
        "the Harn agent turn facade should compose the host event-capture primitive"
    );
    assert!(
        WORKFLOW_REGISTER_RS.contains("__host_workflow_graph_run")
            && WORKFLOW_EXECUTE_HARN.contains("__host_workflow_graph_run"),
        "the Harn workflow facade should call only the host workflow graph primitive"
    );
    assert!(
        WORKFLOW_RS.contains("prepare_workflow_stage_prompt(")
            && WORKFLOW_PROMPTS_HARN.contains("workflow_prepare_stage_prompt"),
        "workflow stage prompt preparation belongs in std/workflow/prompts.harn"
    );
    assert!(
        !WORKFLOW_ARTIFACTS_RS.contains("pub fn render_workflow_prompt")
            && !WORKFLOW_ARTIFACTS_RS.contains("pub fn render_verification_context"),
        "workflow stage and verification prompt renderers must not move back into Rust"
    );
    assert!(
        WORKFLOW_RS.contains("select_workflow_stage_artifacts(")
            && WORKFLOW_CONTEXT_HARN.contains("workflow_select_stage_artifacts"),
        "workflow stage artifact selection policy belongs in std/workflow/context.harn"
    );
    assert!(
        !WORKFLOW_RS.contains("selection_policy.include_kinds")
            && !WORKFLOW_RS.contains("select_artifacts_adaptive(artifacts.to_vec()"),
        "workflow.rs should not own stage artifact-selection policy"
    );
    assert!(
        WORKFLOW_RS.contains("prepare_workflow_stage_agent_options(")
            && WORKFLOW_OPTIONS_HARN.contains("workflow_stage_agent_options"),
        "workflow stage agent option composition belongs in std/workflow/options.harn"
    );
    assert!(
        !WORKFLOW_RS.contains("HARN_AGENT_TOOL_FORMAT")
            && !WORKFLOW_RS.contains("default_tool_format(&model, &provider)")
            && !WORKFLOW_RS
                .contains("max_iterations: node.model_policy.max_iterations.unwrap_or(16)")
            && !WORKFLOW_RS.contains("max_nudges: node.model_policy.max_nudges.unwrap_or(3)"),
        "workflow.rs should not own stage tool-format/default loop policy"
    );
}

#[test]
fn prompt_prose_does_not_move_back_into_rust_renderers() {
    for forbidden in [
        "## Tool Calling Contract",
        "## Native tool protocol",
        "## Available tools",
        "## Task ledger",
    ] {
        assert!(
            !CONTRACT_PROMPT_RS.contains(forbidden),
            "tool-contract prose belongs in stdlib .harn.prompt assets, not Rust"
        );
    }

    for forbidden in [
        "You are running workflow stage",
        "Return exactly one JSON object",
        "Verification context:",
    ] {
        assert!(
            !WORKFLOW_ARTIFACTS_RS.contains(forbidden),
            "workflow prompt prose belongs in stdlib .harn.prompt assets, not Rust"
        );
    }
}
