//! Canonical embedded Harn standard library source catalog.
//!
//! This crate intentionally contains only static source strings so runtime and
//! static tooling crates can share the same stdlib modules without depending on
//! each other.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibSource {
    pub module: &'static str,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibPromptAsset {
    pub path: &'static str,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibPublicFunction {
    pub name: String,
    pub signature: String,
    pub required_params: usize,
    pub total_params: usize,
    pub variadic: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibEntrypointModule {
    pub import_path: String,
    pub category: String,
}

pub const STDLIB_SOURCES: &[StdlibSource] = &[
    StdlibSource {
        module: "text",
        source: include_str!("stdlib/stdlib_text.harn"),
    },
    StdlibSource {
        module: "ansi",
        source: include_str!("stdlib/stdlib_ansi.harn"),
    },
    StdlibSource {
        module: "table",
        source: include_str!("stdlib/stdlib_table.harn"),
    },
    StdlibSource {
        module: "diff",
        source: include_str!("stdlib/stdlib_diff.harn"),
    },
    StdlibSource {
        module: "edit",
        source: include_str!("stdlib/stdlib_edit.harn"),
    },
    StdlibSource {
        module: "artifact/web",
        source: include_str!("stdlib/artifact/web.harn"),
    },
    StdlibSource {
        module: "collections",
        source: include_str!("stdlib/stdlib_collections.harn"),
    },
    StdlibSource {
        module: "math",
        source: include_str!("stdlib/stdlib_math.harn"),
    },
    StdlibSource {
        module: "path",
        source: include_str!("stdlib/stdlib_path.harn"),
    },
    StdlibSource {
        module: "fs",
        source: include_str!("stdlib/stdlib_fs.harn"),
    },
    StdlibSource {
        module: "run_artifacts",
        source: include_str!("stdlib/stdlib_run_artifacts.harn"),
    },
    StdlibSource {
        module: "os",
        source: include_str!("stdlib/stdlib_os.harn"),
    },
    StdlibSource {
        module: "json",
        source: include_str!("stdlib/stdlib_json.harn"),
    },
    StdlibSource {
        module: "json/stream",
        source: include_str!("stdlib/stdlib_json_stream.harn"),
    },
    StdlibSource {
        module: "cache",
        source: include_str!("stdlib/stdlib_cache.harn"),
    },
    StdlibSource {
        module: "observability",
        source: include_str!("stdlib/stdlib_observability.harn"),
    },
    StdlibSource {
        module: "timing",
        source: include_str!("stdlib/stdlib_timing.harn"),
    },
    StdlibSource {
        module: "tools",
        source: include_str!("stdlib/stdlib_tools.harn"),
    },
    StdlibSource {
        module: "composition",
        source: include_str!("stdlib/stdlib_composition.harn"),
    },
    StdlibSource {
        module: "web",
        source: include_str!("stdlib/stdlib_web.harn"),
    },
    StdlibSource {
        module: "graphql",
        source: include_str!("stdlib/stdlib_graphql.harn"),
    },
    StdlibSource {
        module: "schema",
        source: include_str!("stdlib/stdlib_schema.harn"),
    },
    StdlibSource {
        module: "testing",
        source: include_str!("stdlib/stdlib_testing.harn"),
    },
    StdlibSource {
        module: "files",
        source: include_str!("stdlib/stdlib_files.harn"),
    },
    StdlibSource {
        module: "mcp",
        source: include_str!("stdlib/stdlib_mcp.harn"),
    },
    StdlibSource {
        module: "vision",
        source: include_str!("stdlib/stdlib_vision.harn"),
    },
    StdlibSource {
        module: "context",
        source: include_str!("stdlib/stdlib_context.harn"),
    },
    StdlibSource {
        module: "context/maintenance",
        source: include_str!("stdlib/context/maintenance.harn"),
    },
    StdlibSource {
        module: "context/eval",
        source: include_str!("stdlib/context/eval.harn"),
    },
    StdlibSource {
        module: "runtime",
        source: include_str!("stdlib/stdlib_runtime.harn"),
    },
    StdlibSource {
        module: "io",
        source: include_str!("stdlib/stdlib_io.harn"),
    },
    StdlibSource {
        module: "command",
        source: include_str!("stdlib/stdlib_command.harn"),
    },
    StdlibSource {
        module: "signal",
        source: include_str!("stdlib/stdlib_signal.harn"),
    },
    StdlibSource {
        module: "net_policy",
        source: include_str!("stdlib/stdlib_net_policy.harn"),
    },
    StdlibSource {
        module: "review",
        source: include_str!("stdlib/stdlib_review.harn"),
    },
    StdlibSource {
        module: "experiments",
        source: include_str!("stdlib/stdlib_experiments.harn"),
    },
    StdlibSource {
        module: "project",
        source: include_str!("stdlib/stdlib_project.harn"),
    },
    StdlibSource {
        module: "prompt_library",
        source: include_str!("stdlib/stdlib_prompt_library.harn"),
    },
    StdlibSource {
        module: "async",
        source: include_str!("stdlib/stdlib_async.harn"),
    },
    StdlibSource {
        module: "poll",
        source: include_str!("stdlib/stdlib_poll.harn"),
    },
    StdlibSource {
        module: "coerce",
        source: include_str!("stdlib/stdlib_coerce.harn"),
    },
    StdlibSource {
        module: "settled",
        source: include_str!("stdlib/stdlib_settled.harn"),
    },
    StdlibSource {
        module: "cli",
        source: include_str!("stdlib/stdlib_cli.harn"),
    },
    StdlibSource {
        module: "gha",
        source: include_str!("stdlib/stdlib_gha.harn"),
    },
    StdlibSource {
        module: "tui",
        source: include_str!("stdlib/stdlib_tui.harn"),
    },
    StdlibSource {
        module: "jsonl",
        source: include_str!("stdlib/stdlib_jsonl.harn"),
    },
    StdlibSource {
        module: "config",
        source: include_str!("stdlib/stdlib_config.harn"),
    },
    StdlibSource {
        module: "calendar",
        source: include_str!("stdlib/stdlib_calendar.harn"),
    },
    StdlibSource {
        module: "agents",
        source: include_str!("stdlib/stdlib_agents.harn"),
    },
    StdlibSource {
        module: "lifecycle/pool",
        source: include_str!("stdlib/lifecycle/pool.harn"),
    },
    StdlibSource {
        module: "lifecycle/combinators",
        source: include_str!("stdlib/lifecycle/combinators.harn"),
    },
    StdlibSource {
        module: "lifecycle/on_budget",
        source: include_str!("stdlib/lifecycle/on_budget.harn"),
    },
    StdlibSource {
        module: "agent/prompts",
        source: include_str!("stdlib/agent/prompts.harn"),
    },
    StdlibSource {
        module: "llm/media",
        source: include_str!("stdlib/llm/media.harn"),
    },
    StdlibSource {
        module: "llm/options",
        source: include_str!("stdlib/llm/options.harn"),
    },
    StdlibSource {
        module: "llm/catalog",
        source: include_str!("stdlib/llm/catalog.harn"),
    },
    StdlibSource {
        module: "llm/safe",
        source: include_str!("stdlib/llm/safe.harn"),
    },
    StdlibSource {
        module: "llm/budget",
        source: include_str!("stdlib/llm/budget.harn"),
    },
    StdlibSource {
        module: "llm/economics",
        source: include_str!("stdlib/llm/economics.harn"),
    },
    StdlibSource {
        module: "llm/prompts",
        source: include_str!("stdlib/llm/prompts.harn"),
    },
    StdlibSource {
        module: "llm/defaults",
        source: include_str!("stdlib/llm/defaults.harn"),
    },
    StdlibSource {
        module: "llm/handlers",
        source: include_str!("stdlib/llm/handlers.harn"),
    },
    StdlibSource {
        module: "llm/tool_telemetry",
        source: include_str!("stdlib/llm/tool_telemetry.harn"),
    },
    StdlibSource {
        module: "llm/tool_middleware",
        source: include_str!("stdlib/llm/tool_middleware.harn"),
    },
    StdlibSource {
        module: "llm/tool_binder",
        source: include_str!("stdlib/llm/tool_binder.harn"),
    },
    StdlibSource {
        module: "llm/refine",
        source: include_str!("stdlib/llm/refine.harn"),
    },
    StdlibSource {
        module: "llm/ensemble",
        source: include_str!("stdlib/llm/ensemble.harn"),
    },
    StdlibSource {
        module: "llm/rerank",
        source: include_str!("stdlib/llm/rerank.harn"),
    },
    StdlibSource {
        module: "agent/reasoning",
        source: include_str!("stdlib/agent/reasoning.harn"),
    },
    StdlibSource {
        module: "agent/options",
        source: include_str!("stdlib/agent/options.harn"),
    },
    StdlibSource {
        module: "llm/judge",
        source: include_str!("stdlib/llm/judge.harn"),
    },
    StdlibSource {
        module: "llm/optimize",
        source: include_str!("stdlib/llm/optimize.harn"),
    },
    StdlibSource {
        module: "agent/events",
        source: include_str!("stdlib/agent/events.harn"),
    },
    StdlibSource {
        module: "agent/primitives",
        source: include_str!("stdlib/agent/primitives.harn"),
    },
    StdlibSource {
        module: "agent/progress",
        source: include_str!("stdlib/agent/progress.harn"),
    },
    StdlibSource {
        module: "agent/required_tools",
        source: include_str!("stdlib/agent/required_tools.harn"),
    },
    StdlibSource {
        module: "agent/stall",
        source: include_str!("stdlib/agent/stall.harn"),
    },
    StdlibSource {
        module: "agent/control",
        source: include_str!("stdlib/agent/control.harn"),
    },
    StdlibSource {
        module: "agent/loop",
        source: include_str!("stdlib/agent/loop.harn"),
    },
    StdlibSource {
        module: "agent/chat",
        source: include_str!("stdlib/agent/chat.harn"),
    },
    StdlibSource {
        module: "agent/user",
        source: include_str!("stdlib/agent/user.harn"),
    },
    StdlibSource {
        module: "agent/tool_search",
        source: include_str!("stdlib/agent/tool_search.harn"),
    },
    StdlibSource {
        module: "agent/turn",
        source: include_str!("stdlib/agent/turn.harn"),
    },
    StdlibSource {
        module: "agent/workers",
        source: include_str!("stdlib/agent/workers.harn"),
    },
    StdlibSource {
        module: "agent/introspection",
        source: include_str!("stdlib/agent/introspection.harn"),
    },
    StdlibSource {
        module: "agent/resume_by",
        source: include_str!("stdlib/agent/resume_by.harn"),
    },
    StdlibSource {
        module: "agent/state",
        source: include_str!("stdlib/agent/state.harn"),
    },
    StdlibSource {
        module: "agent/skills",
        source: include_str!("stdlib/agent/skills.harn"),
    },
    StdlibSource {
        module: "agent/autocompact",
        source: include_str!("stdlib/agent/autocompact.harn"),
    },
    StdlibSource {
        module: "agent/mcp",
        source: include_str!("stdlib/agent/mcp.harn"),
    },
    StdlibSource {
        module: "agent/host_tools",
        source: include_str!("stdlib/agent/host_tools.harn"),
    },
    StdlibSource {
        module: "agent/budget",
        source: include_str!("stdlib/agent/budget.harn"),
    },
    StdlibSource {
        module: "agent/daemon",
        source: include_str!("stdlib/agent/daemon.harn"),
    },
    StdlibSource {
        module: "agent/preflight",
        source: include_str!("stdlib/agent/preflight.harn"),
    },
    StdlibSource {
        module: "agent/postturn",
        source: include_str!("stdlib/agent/postturn.harn"),
    },
    StdlibSource {
        module: "agent/judge",
        source: include_str!("stdlib/agent/judge.harn"),
    },
    StdlibSource {
        module: "agent/fact",
        source: include_str!("stdlib/agent/fact.harn"),
    },
    StdlibSource {
        module: "agent/presets",
        source: include_str!("stdlib/agent/presets.harn"),
    },
    StdlibSource {
        module: "agent_state",
        source: include_str!("stdlib/stdlib_agent_state.harn"),
    },
    StdlibSource {
        module: "memory",
        source: include_str!("stdlib/stdlib_memory.harn"),
    },
    StdlibSource {
        module: "postgres",
        source: include_str!("stdlib/stdlib_postgres.harn"),
    },
    StdlibSource {
        module: "checkpoint",
        source: include_str!("stdlib/stdlib_checkpoint.harn"),
    },
    StdlibSource {
        module: "host",
        source: include_str!("stdlib/stdlib_host.harn"),
    },
    StdlibSource {
        module: "git",
        source: include_str!("stdlib/stdlib_git.harn"),
    },
    StdlibSource {
        module: "hitl",
        source: include_str!("stdlib/stdlib_hitl.harn"),
    },
    StdlibSource {
        module: "trust",
        source: include_str!("stdlib/stdlib_trust.harn"),
    },
    StdlibSource {
        module: "corrections",
        source: include_str!("stdlib/stdlib_corrections.harn"),
    },
    StdlibSource {
        module: "plan",
        source: include_str!("stdlib/stdlib_plan.harn"),
    },
    StdlibSource {
        module: "waitpoints",
        source: include_str!("stdlib/stdlib_waitpoints.harn"),
    },
    StdlibSource {
        module: "waitpoint",
        source: include_str!("stdlib/stdlib_waitpoint.harn"),
    },
    StdlibSource {
        module: "monitors",
        source: include_str!("stdlib/stdlib_monitors.harn"),
    },
    StdlibSource {
        module: "worktree",
        source: include_str!("stdlib/stdlib_worktree.harn"),
    },
    StdlibSource {
        module: "acp",
        source: include_str!("stdlib/stdlib_acp.harn"),
    },
    StdlibSource {
        module: "triggers",
        source: include_str!("stdlib/stdlib_triggers.harn"),
    },
    StdlibSource {
        module: "triage",
        source: include_str!("stdlib/stdlib_triage.harn"),
    },
    StdlibSource {
        module: "dashboard/jobs",
        source: include_str!("stdlib/dashboard/jobs.harn"),
    },
    StdlibSource {
        module: "ui_resource",
        source: include_str!("stdlib/stdlib_ui_resource.harn"),
    },
    StdlibSource {
        module: "handoffs",
        source: include_str!("stdlib/stdlib_handoffs.harn"),
    },
    StdlibSource {
        module: "lifecycle",
        source: include_str!("stdlib/stdlib_lifecycle.harn"),
    },
    StdlibSource {
        module: "tool_hooks_catalogues",
        source: include_str!("stdlib/stdlib_tool_hooks_catalogues.harn"),
    },
    StdlibSource {
        module: "tool_hooks",
        source: include_str!("stdlib/stdlib_tool_hooks.harn"),
    },
    StdlibSource {
        module: "channel_guardrails",
        source: include_str!("stdlib/stdlib_channel_guardrails.harn"),
    },
    StdlibSource {
        module: "personas/prelude",
        source: include_str!("stdlib/stdlib_personas_prelude.harn"),
    },
    StdlibSource {
        module: "personas/bulletins",
        source: include_str!("stdlib/stdlib_personas_bulletins.harn"),
    },
    StdlibSource {
        module: "connectors/shared",
        source: include_str!("stdlib/stdlib_connectors_shared.harn"),
    },
    StdlibSource {
        module: "oauth/providers",
        source: include_str!("stdlib/oauth/providers.harn"),
    },
    StdlibSource {
        module: "oauth/storage",
        source: include_str!("stdlib/oauth/storage.harn"),
    },
    StdlibSource {
        module: "oauth/client",
        source: include_str!("stdlib/oauth/client.harn"),
    },
    StdlibSource {
        module: "oauth/device_flow",
        source: include_str!("stdlib/oauth/device_flow.harn"),
    },
    StdlibSource {
        module: "oauth/redaction",
        source: include_str!("stdlib/oauth/redaction.harn"),
    },
    StdlibSource {
        module: "oauth/dynamic_registration",
        source: include_str!("stdlib/oauth/dynamic_registration.harn"),
    },
    StdlibSource {
        module: "connectors/github",
        source: include_str!("stdlib/stdlib_connectors_github.harn"),
    },
    StdlibSource {
        module: "connectors/linear",
        source: include_str!("stdlib/stdlib_connectors_linear.harn"),
    },
    StdlibSource {
        module: "connectors/notion",
        source: include_str!("stdlib/stdlib_connectors_notion.harn"),
    },
    StdlibSource {
        module: "connectors/slack",
        source: include_str!("stdlib/stdlib_connectors_slack.harn"),
    },
    StdlibSource {
        module: "workflow/prompts",
        source: include_str!("stdlib/workflow/prompts.harn"),
    },
    StdlibSource {
        module: "workflow/context",
        source: include_str!("stdlib/workflow/context.harn"),
    },
    StdlibSource {
        module: "workflow/options",
        source: include_str!("stdlib/workflow/options.harn"),
    },
    StdlibSource {
        module: "workflow/checkpoints",
        source: include_str!("stdlib/workflow/checkpoints.harn"),
    },
    StdlibSource {
        module: "workflow/stage",
        source: include_str!("stdlib/workflow/stage.harn"),
    },
    StdlibSource {
        module: "workflow/map",
        source: include_str!("stdlib/workflow/map.harn"),
    },
    StdlibSource {
        module: "workflow/schedule",
        source: include_str!("stdlib/workflow/schedule.harn"),
    },
    StdlibSource {
        module: "workflow/execute",
        source: include_str!("stdlib/workflow/execute.harn"),
    },
];

pub const STDLIB_PROMPT_ASSETS: &[StdlibPromptAsset] = &[
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_text.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_text.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_native.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_native.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_text_response_protocol.harn.prompt",
        source: include_str!(
            "stdlib/agent/prompts/tool_contract_text_response_protocol.harn.prompt"
        ),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_action_native.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_action_native.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_action_text.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_action_text.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_task_ledger.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_task_ledger.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/tool_contract_deferred_tools.harn.prompt",
        source: include_str!("stdlib/agent/prompts/tool_contract_deferred_tools.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/deferred_tool_listing.harn.prompt",
        source: include_str!("stdlib/agent/prompts/deferred_tool_listing.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/action_turn_nudge.harn.prompt",
        source: include_str!("stdlib/agent/prompts/action_turn_nudge.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/agent_turn_preamble.harn.prompt",
        source: include_str!("stdlib/agent/prompts/agent_turn_preamble.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/default_nudge.harn.prompt",
        source: include_str!("stdlib/agent/prompts/default_nudge.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/agentic_user_system.harn.prompt",
        source: include_str!("stdlib/agent/prompts/agentic_user_system.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/agentic_user_user.harn.prompt",
        source: include_str!("stdlib/agent/prompts/agentic_user_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/loop_until_done_system.harn.prompt",
        source: include_str!("stdlib/agent/prompts/loop_until_done_system.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/completion_judge_default.harn.prompt",
        source: include_str!("stdlib/agent/prompts/completion_judge_default.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/completion_judge_feedback_fallback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/completion_judge_feedback_fallback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/completion_judge_user.harn.prompt",
        source: include_str!("stdlib/agent/prompts/completion_judge_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/parse_guidance.harn.prompt",
        source: include_str!("stdlib/agent/prompts/parse_guidance.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/protocol_violation_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/protocol_violation_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/native_tool_contract_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/native_tool_contract_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/verification_gate_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/verification_gate_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/action_required_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/action_required_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/daemon_watch_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/daemon_watch_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "agent/prompts/daemon_timer_feedback.harn.prompt",
        source: include_str!("stdlib/agent/prompts/daemon_timer_feedback.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/completion_fallback_system.harn.prompt",
        source: include_str!("stdlib/llm/prompts/completion_fallback_system.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/completion_fallback_user.harn.prompt",
        source: include_str!("stdlib/llm/prompts/completion_fallback_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/transcript_summarize_user.harn.prompt",
        source: include_str!("stdlib/llm/prompts/transcript_summarize_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/structural_chain_of_draft.harn.prompt",
        source: include_str!("stdlib/llm/prompts/structural_chain_of_draft.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/schema_recover_repair.harn.prompt",
        source: include_str!("stdlib/llm/prompts/schema_recover_repair.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/structured_envelope_schema_contract.harn.prompt",
        source: include_str!("stdlib/llm/prompts/structured_envelope_schema_contract.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/structured_envelope_repair.harn.prompt",
        source: include_str!("stdlib/llm/prompts/structured_envelope_repair.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/pairwise_rerank_user.harn.prompt",
        source: include_str!("stdlib/llm/prompts/pairwise_rerank_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "llm/prompts/tool_binder_user.harn.prompt",
        source: include_str!("stdlib/llm/prompts/tool_binder_user.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "workflow/prompts/stage.harn.prompt",
        source: include_str!("stdlib/workflow/prompts/stage.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "workflow/prompts/verification_context_intro.harn.prompt",
        source: include_str!("stdlib/workflow/prompts/verification_context_intro.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "orchestration/prompts/compaction_summary.harn.prompt",
        source: include_str!("stdlib/orchestration/prompts/compaction_summary.harn.prompt"),
    },
    StdlibPromptAsset {
        path: "orchestration/prompts/compaction_policy_replacement.harn.prompt",
        source: include_str!(
            "stdlib/orchestration/prompts/compaction_policy_replacement.harn.prompt"
        ),
    },
];

pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    STDLIB_SOURCES
        .iter()
        .find_map(|entry| (entry.module == module).then_some(entry.source))
}

pub fn get_stdlib_prompt_asset(path: &str) -> Option<&'static str> {
    let path = path.strip_prefix("std/").unwrap_or(path);
    STDLIB_PROMPT_ASSETS
        .iter()
        .find_map(|entry| (entry.path == path).then_some(entry.source))
}

pub fn public_functions_for_module(module: &str) -> Vec<StdlibPublicFunction> {
    let Some(source) = get_stdlib_source(module) else {
        return Vec::new();
    };
    public_functions_from_source(source)
}

pub fn entrypoint_modules() -> Vec<StdlibEntrypointModule> {
    STDLIB_SOURCES
        .iter()
        .filter_map(|entry| {
            entrypoint_category_from_source(entry.source).map(|category| StdlibEntrypointModule {
                import_path: format!("std/{}", entry.module),
                category,
            })
        })
        .collect()
}

fn entrypoint_category_from_source(source: &str) -> Option<String> {
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(category) = line.strip_prefix("// @harn-entrypoint-category ") {
            let category = category.trim();
            return (!category.is_empty()).then(|| category.to_string());
        }
        if !line.starts_with("//") {
            return None;
        }
    }
    None
}

fn public_functions_from_source(source: &str) -> Vec<StdlibPublicFunction> {
    let mut out = Vec::new();
    let mut doc: Option<String> = None;
    let lines = source.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.starts_with("/**") {
            let (parsed, next) = parse_harndoc(&lines, index);
            doc = parsed;
            index = next;
            continue;
        }
        if line.starts_with("pub fn ") {
            let (signature_line, next) = collect_public_function_signature(&lines, index);
            if let Some(function) = parse_public_function_line(&signature_line, doc.take()) {
                out.push(function);
                index = next;
                continue;
            }
        }
        if let Some(function) = parse_public_function_line(line, doc.take()) {
            out.push(function);
        } else if !line.is_empty() && !line.starts_with("//") {
            doc = None;
        }
        index += 1;
    }
    out
}

fn collect_public_function_signature(lines: &[&str], start: usize) -> (String, usize) {
    let mut parts = Vec::new();
    let mut index = start;
    while index < lines.len() {
        parts.push(lines[index].trim().to_string());
        let candidate = parts.join(" ");
        if public_function_signature_complete(&candidate) {
            return (candidate, index + 1);
        }
        index += 1;
    }
    (parts.join(" "), index)
}

fn public_function_signature_complete(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("pub fn ") else {
        return false;
    };
    let Some(name_end) = rest.find('(') else {
        return false;
    };
    matching_paren_len(&rest[name_end + 1..]).is_some()
}

fn parse_harndoc(lines: &[&str], start: usize) -> (Option<String>, usize) {
    let mut parts = Vec::new();
    let mut index = start;
    while index < lines.len() {
        let mut line = lines[index].trim();
        if index == start {
            line = line.trim_start_matches("/**").trim();
        }
        let done = line.ends_with("*/");
        line = line.trim_end_matches("*/").trim();
        line = line.trim_start_matches('*').trim();
        if !line.is_empty() {
            parts.push(line.to_string());
        }
        index += 1;
        if done {
            break;
        }
    }
    let text = parts.join("\n").trim().to_string();
    ((!text.is_empty()).then_some(text), index)
}

fn parse_public_function_line(line: &str, doc: Option<String>) -> Option<StdlibPublicFunction> {
    let rest = line.strip_prefix("pub fn ")?.trim();
    let name_end = rest.find('(')?;
    let name = rest[..name_end].trim();
    if name.is_empty() {
        return None;
    }
    let params_start = name_end + 1;
    let params_len = matching_paren_len(&rest[params_start..])?;
    let params = &rest[params_start..params_start + params_len];
    let after = rest[params_start + params_len + 1..].trim();
    let return_type = after
        .strip_prefix("->")
        .and_then(|tail| tail.split('{').next())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let signature = match return_type {
        Some(ret) => format!("{name}({params}) -> {ret}"),
        None => format!("{name}({params})"),
    };
    let param_parts = split_top_level_params(params);
    let total_params = param_parts
        .iter()
        .filter(|param| !param.trim().is_empty())
        .count();
    let variadic = param_parts
        .iter()
        .any(|param| param.trim_start().starts_with("..."));
    let required_params = param_parts
        .iter()
        .filter(|param| {
            let param = param.trim();
            !param.is_empty() && !param.contains('=') && !param.starts_with("...")
        })
        .count();
    Some(StdlibPublicFunction {
        name: name.to_string(),
        signature,
        required_params,
        total_params,
        variadic,
        doc,
    })
}

fn matching_paren_len(input: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (offset, ch) in input.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(offset);
                }
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn split_top_level_params(params: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0isize;
    let mut start = 0usize;
    for (offset, ch) in params.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&params[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        entrypoint_modules, get_stdlib_prompt_asset, get_stdlib_source,
        public_functions_for_module, STDLIB_PROMPT_ASSETS, STDLIB_SOURCES,
    };

    #[test]
    fn stdlib_sources_are_non_empty() {
        for entry in STDLIB_SOURCES {
            assert!(
                !entry.source.trim().is_empty(),
                "{} should have non-empty source",
                entry.module
            );
        }
    }

    #[test]
    fn stdlib_source_names_are_unique() {
        let mut names = BTreeSet::new();
        for entry in STDLIB_SOURCES {
            assert!(names.insert(entry.module), "duplicate {}", entry.module);
        }
    }

    #[test]
    fn stdlib_prompt_assets_are_non_empty() {
        for entry in STDLIB_PROMPT_ASSETS {
            assert!(
                !entry.source.trim().is_empty(),
                "{} should have non-empty prompt asset source",
                entry.path
            );
        }
    }

    #[test]
    fn stdlib_prompt_asset_paths_are_unique() {
        let mut paths = BTreeSet::new();
        for entry in STDLIB_PROMPT_ASSETS {
            assert!(paths.insert(entry.path), "duplicate {}", entry.path);
        }
    }

    #[test]
    fn key_stdlib_modules_resolve() {
        for module in [
            "context",
            "context/maintenance",
            "context/eval",
            "edit",
            "artifact/web",
            "command",
            "waitpoint",
            "llm/handlers",
            "llm/tool_middleware",
            "llm/tool_binder",
            "llm/ensemble",
            "llm/rerank",
            "personas/prelude",
            "personas/bulletins",
            "agent/host_tools",
            "agent/user",
            "llm/optimize",
            "llm/judge",
            "llm/refine",
            "connectors/shared",
            "connectors/github",
            "connectors/linear",
            "connectors/notion",
            "connectors/slack",
            "triage",
            "dashboard/jobs",
            "ui_resource",
        ] {
            assert!(
                get_stdlib_source(module).is_some(),
                "std/{module} should resolve"
            );
        }
    }

    #[test]
    fn key_stdlib_prompt_assets_resolve() {
        for path in [
            "std/agent/prompts/tool_contract_text.harn.prompt",
            "std/agent/prompts/action_turn_nudge.harn.prompt",
            "std/agent/prompts/completion_judge_default.harn.prompt",
            "std/workflow/prompts/stage.harn.prompt",
            "std/orchestration/prompts/compaction_summary.harn.prompt",
            "std/orchestration/prompts/compaction_policy_replacement.harn.prompt",
        ] {
            assert!(
                get_stdlib_prompt_asset(path).is_some(),
                "{path} should resolve"
            );
        }
    }

    #[test]
    fn public_function_catalog_derives_signatures_from_harn_source() {
        let exports = public_functions_for_module("workflow/execute");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].name, "workflow_execute");
        assert_eq!(
            exports[0].signature,
            "workflow_execute(task, graph, artifacts = nil, options = nil)"
        );
        assert_eq!(exports[0].required_params, 2);
        assert_eq!(exports[0].total_params, 4);
    }

    #[test]
    fn command_stdlib_module_exports_step_helpers() {
        let exports = public_functions_for_module("command")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "command_run",
            "command_output_tail",
            "command_json",
            "command_json_step",
            "command_try",
            "command_step",
            "command_steps_append",
            "command_last_failed_step",
            "command_step_ref",
        ] {
            assert!(exports.contains(name), "std/command should export {name}");
        }
    }

    #[test]
    fn async_stdlib_exports_predicate_backoff_name_only() {
        let exports = public_functions_for_module("async")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        assert!(
            exports.contains("retry_predicate_with_backoff"),
            "std/async should export retry_predicate_with_backoff"
        );
        assert!(
            !exports.contains("retry_with_backoff"),
            "std/async should not retain the old retry_with_backoff export"
        );
    }

    #[test]
    fn signal_stdlib_module_exports_interrupt_helpers() {
        let exports = public_functions_for_module("signal")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "on_interrupt",
            "off_interrupt",
            "interrupted",
            "with_interrupt",
        ] {
            assert!(exports.contains(name), "std/signal should export {name}");
        }
    }

    #[test]
    fn git_stdlib_module_exports_local_wrappers() {
        let exports = public_functions_for_module("git")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "git_run",
            "git_status",
            "git_current_branch",
            "git_log",
            "git_switch",
            "git_pull_ff_only",
            "git_find_tool",
            "git_run_tool",
            "git_tools",
            "git_toolbox_tools",
        ] {
            assert!(exports.contains(name), "std/git should export {name}");
        }
    }

    #[test]
    fn agent_workers_exports_suspend_resume_wrappers() {
        let exports = public_functions_for_module("agent/workers");
        let suspend = exports
            .iter()
            .find(|function| function.name == "suspend_agent")
            .expect("std/agent/workers should export suspend_agent");
        assert_eq!(
            suspend.signature,
            "suspend_agent(worker, reason = \"\", options = nil)"
        );
        assert_eq!(suspend.required_params, 1);
        assert_eq!(suspend.total_params, 3);

        let resume = exports
            .iter()
            .find(|function| function.name == "resume_agent")
            .expect("std/agent/workers should export resume_agent");
        assert_eq!(
            resume.signature,
            "resume_agent(worker_or_snapshot, resume_input = nil, continue_transcript = true)"
        );
        assert_eq!(resume.required_params, 1);
        assert_eq!(resume.total_params, 3);

        let parse_resume = exports
            .iter()
            .find(|function| function.name == "parse_resume_conditions")
            .expect("std/agent/workers should export parse_resume_conditions");
        assert_eq!(
            parse_resume.signature,
            "parse_resume_conditions(conditions = nil) -> ResumeConditions?"
        );
        assert_eq!(parse_resume.required_params, 0);
        assert_eq!(parse_resume.total_params, 1);

        let lifecycle = exports
            .iter()
            .find(|function| function.name == "agent_lifecycle_tools")
            .expect("std/agent/workers should export agent_lifecycle_tools");
        assert_eq!(
            lifecycle.signature,
            "agent_lifecycle_tools(registry = nil, options = nil)"
        );
        assert_eq!(lifecycle.required_params, 0);
        assert_eq!(lifecycle.total_params, 2);
    }

    #[test]
    fn tui_stdlib_module_exports_terminal_helpers() {
        let exports = public_functions_for_module("tui")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in ["page", "terminal_width", "rule", "clear", "select_from"] {
            assert!(exports.contains(name), "std/tui should export {name}");
        }
    }

    #[test]
    fn harn_entrypoint_catalog_is_declared_by_stdlib_sources() {
        let modules = entrypoint_modules();
        let entries = modules
            .iter()
            .map(|module| (module.import_path.as_str(), module.category.as_str()))
            .collect::<BTreeSet<_>>();
        for entry in [
            ("std/agent/loop", "agent.stdlib"),
            ("std/agent/turn", "agent.stdlib"),
            ("std/agent/primitives", "agent.stdlib"),
            ("std/workflow/execute", "workflow.stdlib"),
        ] {
            assert!(entries.contains(&entry), "{entry:?} should be declared");
        }
    }
}
