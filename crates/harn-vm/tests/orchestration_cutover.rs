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
const WORKFLOW_PROMPTS_HARN: &str =
    include_str!("../../harn-stdlib/src/stdlib/workflow/prompts.harn");
const CONTRACT_PROMPT_RS: &str = include_str!("../src/llm/tools/contract_prompt.rs");
const WORKFLOW_ARTIFACTS_RS: &str = include_str!("../src/orchestration/artifacts.rs");
const WORKFLOW_RS: &str = include_str!("../src/orchestration/workflow.rs");

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
