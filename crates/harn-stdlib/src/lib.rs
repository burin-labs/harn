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

macro_rules! embedded_catalog {
    ($entry:ident, $key:ident, [$($name:literal => $path:literal),* $(,)?]) => {
        &[
            $($entry {
                $key: $name,
                source: include_str!($path),
            },)*
        ]
    };
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

pub const STDLIB_SOURCES: &[StdlibSource] = embedded_catalog!(StdlibSource, module, [
    "text" => "stdlib/stdlib_text.harn",
    "semver" => "stdlib/stdlib_semver.harn",
    "changelog" => "stdlib/stdlib_changelog.harn",
    "ansi" => "stdlib/stdlib_ansi.harn",
    "table" => "stdlib/stdlib_table.harn",
    "diff" => "stdlib/stdlib_diff.harn",
    "edit" => "stdlib/stdlib_edit.harn",
    "edit/capabilities" => "stdlib/edit/capabilities.harn",
    "edit/internal" => "stdlib/edit/internal.harn",
    "edit/patch" => "stdlib/edit/patch.harn",
    "edit/safe_patch" => "stdlib/edit/safe_patch.harn",
    "edit/fast_apply" => "stdlib/edit/fast_apply.harn",
    "ast" => "stdlib/stdlib_ast.harn",
    "rules" => "stdlib/stdlib_rules.harn",
    "lint" => "stdlib/stdlib_lint.harn",
    "artifact/web" => "stdlib/artifact/web.harn",
    "collections" => "stdlib/stdlib_collections.harn",
    "math" => "stdlib/stdlib_math.harn",
    "slug" => "stdlib/stdlib_slug.harn",
    "path" => "stdlib/stdlib_path.harn",
    "fs" => "stdlib/stdlib_fs.harn",
    "run_artifacts" => "stdlib/stdlib_run_artifacts.harn",
    "os" => "stdlib/stdlib_os.harn",
    "json" => "stdlib/stdlib_json.harn",
    "json/stream" => "stdlib/stdlib_json_stream.harn",
    "xml" => "stdlib/stdlib_xml.harn",
    "cache" => "stdlib/stdlib_cache.harn",
    "observability" => "stdlib/stdlib_observability.harn",
    "timing" => "stdlib/stdlib_timing.harn",
    "verification" => "stdlib/stdlib_verification.harn",
    "tools" => "stdlib/stdlib_tools.harn",
    "composition" => "stdlib/stdlib_composition.harn",
    "web" => "stdlib/stdlib_web.harn",
    "graphql" => "stdlib/stdlib_graphql.harn",
    "code_librarian" => "stdlib/stdlib_code_librarian.harn",
    "schema" => "stdlib/stdlib_schema.harn",
    "schema/contracts" => "stdlib/schema/contracts.harn",
    "identity" => "stdlib/stdlib_identity.harn",
    "disclosure" => "stdlib/stdlib_disclosure.harn",
    "testing" => "stdlib/stdlib_testing.harn",
    "files" => "stdlib/stdlib_files.harn",
    "document" => "stdlib/stdlib_document.harn",
    "mcp" => "stdlib/stdlib_mcp.harn",
    "vision" => "stdlib/stdlib_vision.harn",
    "context" => "stdlib/stdlib_context.harn",
    "context/maintenance" => "stdlib/context/maintenance.harn",
    "context/eval" => "stdlib/context/eval.harn",
    "eval/stats" => "stdlib/stdlib_eval_stats.harn",
    "eval/agreement" => "stdlib/stdlib_eval_agreement.harn",
    "runtime" => "stdlib/stdlib_runtime.harn",
    "io" => "stdlib/stdlib_io.harn",
    "net" => "stdlib/stdlib_net.harn",
    "command" => "stdlib/stdlib_command.harn",
    "verification_types" => "stdlib/verification_types.harn",
    "verification_core" => "stdlib/verification_core.harn",
    "verification_targets" => "stdlib/verification_targets.harn",
    "verification_ladder" => "stdlib/verification_ladder.harn",
    "verification_public" => "stdlib/verification_public.harn",
    "signal" => "stdlib/stdlib_signal.harn",
    "net_policy" => "stdlib/stdlib_net_policy.harn",
    "review" => "stdlib/stdlib_review.harn",
    "experiments" => "stdlib/stdlib_experiments.harn",
    "project" => "stdlib/stdlib_project.harn",
    "prompt_library" => "stdlib/stdlib_prompt_library.harn",
    "async" => "stdlib/stdlib_async.harn",
    "poll" => "stdlib/stdlib_poll.harn",
    "coerce" => "stdlib/stdlib_coerce.harn",
    "settled" => "stdlib/stdlib_settled.harn",
    "cli" => "stdlib/stdlib_cli.harn",
    "cli/argparse" => "stdlib/cli/argparse.harn",
    "cli/render" => "stdlib/cli/render.harn",
    "cli/models/batch_artifacts" => "stdlib/cli/models/batch_artifacts.harn",
    "cli/models/batch_cancel" => "stdlib/cli/models/batch_cancel.harn",
    "cli/models/batch_download" => "stdlib/cli/models/batch_download.harn",
    "cli/models/batch_lifecycle" => "stdlib/cli/models/batch_lifecycle.harn",
    "cli/models/batch_status" => "stdlib/cli/models/batch_status.harn",
    "cli/models/batch_submit" => "stdlib/cli/models/batch_submit.harn",
    "cli/models/batch_transport" => "stdlib/cli/models/batch_transport.harn",
    "cli/models/lora_render" => "stdlib/cli/models/lora_render.harn",
    "cli/paths" => "stdlib/cli/paths.harn",
    "gha" => "stdlib/stdlib_gha.harn",
    "tui" => "stdlib/stdlib_tui.harn",
    "jsonl" => "stdlib/stdlib_jsonl.harn",
    "config" => "stdlib/stdlib_config.harn",
    "calendar" => "stdlib/stdlib_calendar.harn",
    "agents" => "stdlib/stdlib_agents.harn",
    "lifecycle/pool" => "stdlib/lifecycle/pool.harn",
    "lifecycle/combinators" => "stdlib/lifecycle/combinators.harn",
    "lifecycle/on_budget" => "stdlib/lifecycle/on_budget.harn",
    "agent/prompts" => "stdlib/agent/prompts.harn",
    "llm/media" => "stdlib/llm/media.harn",
    "llm/options" => "stdlib/llm/options.harn",
    "llm/catalog" => "stdlib/llm/catalog.harn",
    "llm/safe" => "stdlib/llm/safe.harn",
    "llm/envelope" => "stdlib/llm/envelope.harn",
    "llm/caller" => "stdlib/llm/caller.harn",
    "harness/policy" => "stdlib/harness/policy.harn",
    "llm/budget" => "stdlib/llm/budget.harn",
    "llm/economics" => "stdlib/llm/economics.harn",
    "llm/prompts" => "stdlib/llm/prompts.harn",
    "llm/defaults" => "stdlib/llm/defaults.harn",
    "llm/handlers" => "stdlib/llm/handlers.harn",
    "llm/tool_telemetry" => "stdlib/llm/tool_telemetry.harn",
    "llm/tool_middleware" => "stdlib/llm/tool_middleware.harn",
    "llm/tool_binder" => "stdlib/llm/tool_binder.harn",
    "llm/structural_validator" => "stdlib/llm/structural_validator.harn",
    "llm/missing_tool_call" => "stdlib/llm/missing_tool_call.harn",
    "llm/scope_classifier" => "stdlib/llm/scope_classifier.harn",
    "llm/refine" => "stdlib/llm/refine.harn",
    "llm/ensemble" => "stdlib/llm/ensemble.harn",
    "llm/rerank" => "stdlib/llm/rerank.harn",
    "agent/reasoning" => "stdlib/agent/reasoning.harn",
    "agent/caller_transport" => "stdlib/agent/caller_transport.harn",
    "agent/options" => "stdlib/agent/options.harn",
    "agent/options_types" => "stdlib/agent/options_types.harn",
    "agent/options_formats" => "stdlib/agent/options_formats.harn",
    "agent/options_validation" => "stdlib/agent/options_validation.harn",
    "agent/options_public" => "stdlib/agent/options_public.harn",
    "agent/llm_dispatch" => "stdlib/agent/llm_dispatch.harn",
    "agent/prefill" => "stdlib/agent/prefill.harn",
    "agent/retry" => "stdlib/agent/retry.harn",
    "llm/judge" => "stdlib/llm/judge.harn",
    "llm/faithfulness" => "stdlib/llm/faithfulness.harn",
    "llm/optimize" => "stdlib/llm/optimize.harn",
    "agent/events" => "stdlib/agent/events.harn",
    "agent/completions" => "stdlib/agent/completions.harn",
    "agent/transcript" => "stdlib/agent/transcript.harn",
    "agent/primitives" => "stdlib/agent/primitives.harn",
    "agent/progress" => "stdlib/agent/progress.harn",
    "agent/required_tools" => "stdlib/agent/required_tools.harn",
    "agent/monologue_actuation" => "stdlib/agent/monologue_actuation.harn",
    "agent/monologue_actuation_types" => "stdlib/agent/monologue_actuation_types.harn",
    "agent/stall_types" => "stdlib/agent/stall_types.harn",
    "agent/stall" => "stdlib/agent/stall.harn",
    "agent/stall_config" => "stdlib/agent/stall_config.harn",
    "agent/stall_observation" => "stdlib/agent/stall_observation.harn",
    "agent/stall_verification" => "stdlib/agent/stall_verification.harn",
    "agent/stall_detectors" => "stdlib/agent/stall_detectors.harn",
    "agent/governors" => "stdlib/agent/governors.harn",
    "agent/control" => "stdlib/agent/control.harn",
    "agent/action_graph" => "stdlib/agent/action_graph.harn",
    "agent/result_text" => "stdlib/agent/result_text.harn",
    "agent/best_of_n" => "stdlib/agent/best_of_n.harn",
    "agent/loop" => "stdlib/agent/loop.harn",
    "agent/loop_support" => "stdlib/agent/loop_support.harn",
    "agent/loop_call_resolution" => "stdlib/agent/loop_call_resolution.harn",
    "agent/loop_result_status" => "stdlib/agent/loop_result_status.harn",
    "agent/loop_foundation" => "stdlib/agent/loop_foundation.harn",
    "agent/loop_tool_calls" => "stdlib/agent/loop_tool_calls.harn",
    "agent/loop_resource_dispatch" => "stdlib/agent/loop_resource_dispatch.harn",
    "agent/loop_turn_options" => "stdlib/agent/loop_turn_options.harn",
    "agent/loop_turn_scope" => "stdlib/agent/loop_turn_scope.harn",
    "agent/loop_internal" => "stdlib/agent/loop_internal.harn",
    "agent/loop_run" => "stdlib/agent/loop_run.harn",
    "agent/loop_finalize" => "stdlib/agent/loop_finalize.harn",
    "agent/loop_terminal" => "stdlib/agent/loop_terminal.harn",
    "agent/chat" => "stdlib/agent/chat.harn",
    "agent/user" => "stdlib/agent/user.harn",
    "agent/tool_search" => "stdlib/agent/tool_search.harn",
    "agent/tool_annotations" => "stdlib/agent/tool_annotations.harn",
    "agent/tool_lifecycle" => "stdlib/agent/tool_lifecycle.harn",
    "agent/turn" => "stdlib/agent/turn.harn",
    "agent/workers" => "stdlib/agent/workers.harn",
    "agent/introspection" => "stdlib/agent/introspection.harn",
    "agent/resume_by" => "stdlib/agent/resume_by.harn",
    "agent/state" => "stdlib/agent/state.harn",
    "agent/canon" => "stdlib/agent/canon.harn",
    "agent/skills" => "stdlib/agent/skills.harn",
    "agent/autocompact" => "stdlib/agent/autocompact.harn",
    "agent/mcp" => "stdlib/agent/mcp.harn",
    "agent/command_capture" => "stdlib/agent/command_capture.harn",
    "agent/host_tools" => "stdlib/agent/host_tools.harn",
    "agent/host_injection" => "stdlib/agent/host_injection.harn",
    "agent/budget" => "stdlib/agent/budget.harn",
    "agent/daemon" => "stdlib/agent/daemon.harn",
    "agent/preflight" => "stdlib/agent/preflight.harn",
    "agent/postturn" => "stdlib/agent/postturn.harn",
    "agent/stance" => "stdlib/agent/stance.harn",
    "agent/lanes" => "stdlib/agent/lanes.harn",
    "agent/overlays" => "stdlib/agent/overlays.harn",
    "agent/sitrep" => "stdlib/agent/sitrep.harn",
    "agent/verdict" => "stdlib/agent/verdict.harn",
    "agent/judge_internals" => "stdlib/agent/judge_internals.harn",
    "agent/judge" => "stdlib/agent/judge.harn",
    "agent/guardrails" => "stdlib/agent/guardrails.harn",
    "agent/step_judge" => "stdlib/agent/step_judge.harn",
    "agent/scratchpad" => "stdlib/agent/scratchpad.harn",
    "agent/fact" => "stdlib/agent/fact.harn",
    "agent/hypothesis" => "stdlib/agent/hypothesis.harn",
    "agent/pattern_knowledge" => "stdlib/agent/pattern_knowledge.harn",
    "agent/probe" => "stdlib/agent/probe.harn",
    "agent/stream" => "stdlib/agent/stream.harn",
    "agent/presets" => "stdlib/agent/presets.harn",
    "agent/pins" => "stdlib/agent/pins.harn",
    "agent/goal" => "stdlib/agent/goal.harn",
    "agent/task_plan" => "stdlib/agent/task_plan.harn",
    "personas/compiler" => "stdlib/personas/compiler.harn",
    "personas/prompt_compiler" => "stdlib/personas/prompt_compiler.harn",
    "agent_state" => "stdlib/stdlib_agent_state.harn",
    "memory" => "stdlib/stdlib_memory.harn",
    "session-store" => "stdlib/stdlib_session_store.harn",
    "coordination" => "stdlib/stdlib_coordination.harn",
    "fleet/coordination" => "stdlib/fleet/coordination.harn",
    "postgres" => "stdlib/stdlib_postgres.harn",
    "postgres/query" => "stdlib/postgres/query.harn",
    "sqlite" => "stdlib/stdlib_sqlite.harn",
    "checkpoint" => "stdlib/stdlib_checkpoint.harn",
    "host" => "stdlib/stdlib_host.harn",
    "host_lease" => "stdlib/stdlib_host_lease.harn",
    "git" => "stdlib/stdlib_git.harn",
    "git/checkout" => "stdlib/git/checkout.harn",
    "git/contracts" => "stdlib/git/contracts.harn",
    "hitl" => "stdlib/stdlib_hitl.harn",
    "trust" => "stdlib/stdlib_trust.harn",
    "corrections" => "stdlib/stdlib_corrections.harn",
    "plan" => "stdlib/stdlib_plan.harn",
    "waitpoints" => "stdlib/stdlib_waitpoints.harn",
    "waitpoint" => "stdlib/stdlib_waitpoint.harn",
    "monitors" => "stdlib/stdlib_monitors.harn",
    "worktree" => "stdlib/stdlib_worktree.harn",
    "acp" => "stdlib/stdlib_acp.harn",
    "external_agent" => "stdlib/stdlib_external_agent.harn",
    "triggers" => "stdlib/stdlib_triggers.harn",
    "triage" => "stdlib/stdlib_triage.harn",
    "dashboard/jobs" => "stdlib/dashboard/jobs.harn",
    "ui_resource" => "stdlib/stdlib_ui_resource.harn",
    "handoffs" => "stdlib/stdlib_handoffs.harn",
    "lifecycle" => "stdlib/stdlib_lifecycle.harn",
    "tool_hooks_catalogues" => "stdlib/stdlib_tool_hooks_catalogues.harn",
    "tool_hooks" => "stdlib/stdlib_tool_hooks.harn",
    "channel_guardrails" => "stdlib/stdlib_channel_guardrails.harn",
    "personas/prelude" => "stdlib/stdlib_personas_prelude.harn",
    "personas/bulletins" => "stdlib/stdlib_personas_bulletins.harn",
    "connectors/shared" => "stdlib/stdlib_connectors_shared.harn",
    "oauth/providers" => "stdlib/oauth/providers.harn",
    "oauth/token_exchange_catalog" => "stdlib/oauth/token_exchange_catalog.harn",
    "oauth/token_exchange" => "stdlib/oauth/token_exchange.harn",
    "oauth/storage" => "stdlib/oauth/storage.harn",
    "oauth/client" => "stdlib/oauth/client.harn",
    "oauth/device_flow" => "stdlib/oauth/device_flow.harn",
    "oauth/redaction" => "stdlib/oauth/redaction.harn",
    "oauth/dynamic_registration" => "stdlib/oauth/dynamic_registration.harn",
    "connectors/github" => "stdlib/stdlib_connectors_github.harn",
    "connectors/linear" => "stdlib/stdlib_connectors_linear.harn",
    "connectors/notion" => "stdlib/stdlib_connectors_notion.harn",
    "connectors/slack" => "stdlib/stdlib_connectors_slack.harn",
    "workflow/prompts" => "stdlib/workflow/prompts.harn",
    "workflow/context" => "stdlib/workflow/context.harn",
    "workflow/options" => "stdlib/workflow/options.harn",
    "workflow/checkpoints" => "stdlib/workflow/checkpoints.harn",
    "workflow/patterns" => "stdlib/workflow/patterns.harn",
    "workflow/stage" => "stdlib/workflow/stage.harn",
    "workflow/map" => "stdlib/workflow/map.harn",
    "workflow/schedule" => "stdlib/workflow/schedule.harn",
    "workflow/execute" => "stdlib/workflow/execute.harn",
    "workflow/repair" => "stdlib/workflow/repair.harn",
    "security" => "stdlib/stdlib_security.harn",
    "pii" => "stdlib/stdlib_pii.harn",
]);

/// Canonical normalized connector event schemas, authored as Harn `type`
/// declarations. This is the SOURCE OF TRUTH for the Rust event-payload
/// structs in `crates/harn-vm/src/triggers/event/schemas_generated.rs`, which
/// are generated from these declarations by `harn connector-schema-codegen`.
///
/// It is intentionally NOT registered in [`STDLIB_SOURCES`]: it is a codegen
/// input (like `spec/openapi.yaml`), not a module loaded into every program.
/// Exposing it as an embedded string keeps the generator independent of the
/// current working directory.
pub const CONNECTOR_EVENT_SCHEMAS_SOURCE: &str = include_str!("stdlib/stdlib_event_schemas.harn");

pub const STDLIB_PROMPT_ASSETS: &[StdlibPromptAsset] = embedded_catalog!(StdlibPromptAsset, path, [
    "agent/prompts/tool_contract_text.harn.prompt" => "stdlib/agent/prompts/tool_contract_text.harn.prompt",
    "agent/prompts/tool_contract_json.harn.prompt" => "stdlib/agent/prompts/tool_contract_json.harn.prompt",
    "agent/prompts/tool_contract_native.harn.prompt" => "stdlib/agent/prompts/tool_contract_native.harn.prompt",
    "agent/prompts/tool_contract_text_response_protocol.harn.prompt" => "stdlib/agent/prompts/tool_contract_text_response_protocol.harn.prompt",
    "agent/prompts/tool_contract_action_native.harn.prompt" => "stdlib/agent/prompts/tool_contract_action_native.harn.prompt",
    "agent/prompts/tool_contract_action_text.harn.prompt" => "stdlib/agent/prompts/tool_contract_action_text.harn.prompt",
    "agent/prompts/tool_contract_task_ledger.harn.prompt" => "stdlib/agent/prompts/tool_contract_task_ledger.harn.prompt",
    "agent/prompts/tool_contract_deferred_tools.harn.prompt" => "stdlib/agent/prompts/tool_contract_deferred_tools.harn.prompt",
    "agent/prompts/deferred_tool_listing.harn.prompt" => "stdlib/agent/prompts/deferred_tool_listing.harn.prompt",
    "agent/prompts/agent_turn_preamble.harn.prompt" => "stdlib/agent/prompts/agent_turn_preamble.harn.prompt",
    "agent/prompts/default_nudge.harn.prompt" => "stdlib/agent/prompts/default_nudge.harn.prompt",
    "agent/prompts/agentic_user_system.harn.prompt" => "stdlib/agent/prompts/agentic_user_system.harn.prompt",
    "agent/prompts/agentic_user_user.harn.prompt" => "stdlib/agent/prompts/agentic_user_user.harn.prompt",
    "agent/prompts/loop_until_done_system.harn.prompt" => "stdlib/agent/prompts/loop_until_done_system.harn.prompt",
    "agent/prompts/completion_judge_default.harn.prompt" => "stdlib/agent/prompts/completion_judge_default.harn.prompt",
    "agent/prompts/completion_judge_feedback_fallback.harn.prompt" => "stdlib/agent/prompts/completion_judge_feedback_fallback.harn.prompt",
    "agent/prompts/completion_judge_user.harn.prompt" => "stdlib/agent/prompts/completion_judge_user.harn.prompt",
    "agent/prompts/step_judge_system_default.harn.prompt" => "stdlib/agent/prompts/step_judge_system_default.harn.prompt",
    "agent/prompts/step_judge_system_adversarial.harn.prompt" => "stdlib/agent/prompts/step_judge_system_adversarial.harn.prompt",
    "agent/prompts/step_judge_user.harn.prompt" => "stdlib/agent/prompts/step_judge_user.harn.prompt",
    "agent/prompts/parse_guidance.harn.prompt" => "stdlib/agent/prompts/parse_guidance.harn.prompt",
    "agent/prompts/native_tool_contract_feedback.harn.prompt" => "stdlib/agent/prompts/native_tool_contract_feedback.harn.prompt",
    "agent/prompts/verification_gate_feedback.harn.prompt" => "stdlib/agent/prompts/verification_gate_feedback.harn.prompt",
    "agent/prompts/daemon_watch_feedback.harn.prompt" => "stdlib/agent/prompts/daemon_watch_feedback.harn.prompt",
    "agent/prompts/daemon_timer_feedback.harn.prompt" => "stdlib/agent/prompts/daemon_timer_feedback.harn.prompt",
    "llm/prompts/completion_fallback_system.harn.prompt" => "stdlib/llm/prompts/completion_fallback_system.harn.prompt",
    "llm/prompts/completion_fallback_user.harn.prompt" => "stdlib/llm/prompts/completion_fallback_user.harn.prompt",
    "llm/prompts/transcript_summarize_user.harn.prompt" => "stdlib/llm/prompts/transcript_summarize_user.harn.prompt",
    "llm/prompts/sitrep_user.harn.prompt" => "stdlib/llm/prompts/sitrep_user.harn.prompt",
    "llm/prompts/structural_chain_of_draft.harn.prompt" => "stdlib/llm/prompts/structural_chain_of_draft.harn.prompt",
    "llm/prompts/schema_recover_repair.harn.prompt" => "stdlib/llm/prompts/schema_recover_repair.harn.prompt",
    "llm/prompts/structured_envelope_schema_contract.harn.prompt" => "stdlib/llm/prompts/structured_envelope_schema_contract.harn.prompt",
    "llm/prompts/structured_envelope_repair.harn.prompt" => "stdlib/llm/prompts/structured_envelope_repair.harn.prompt",
    "llm/prompts/pairwise_rerank_user.harn.prompt" => "stdlib/llm/prompts/pairwise_rerank_user.harn.prompt",
    "llm/prompts/tool_binder_user.harn.prompt" => "stdlib/llm/prompts/tool_binder_user.harn.prompt",
    "workflow/prompts/stage.harn.prompt" => "stdlib/workflow/prompts/stage.harn.prompt",
    "workflow/prompts/verification_context_intro.harn.prompt" => "stdlib/workflow/prompts/verification_context_intro.harn.prompt",
    "orchestration/prompts/compaction_summary.harn.prompt" => "stdlib/orchestration/prompts/compaction_summary.harn.prompt",
    "orchestration/prompts/compaction_policy_replacement.harn.prompt" => "stdlib/orchestration/prompts/compaction_policy_replacement.harn.prompt",
]);

/// Embedded `.harn` script that backs a CLI subcommand. Looked up by
/// the `harn-cli` dispatch wedge (see harn#2293 epic and harn#2294 G1)
/// so subcommands can ship in Harn instead of Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibCliScript {
    /// Lookup name. For nested scripts this is the path under
    /// `stdlib/cli/` without the `.harn` extension (e.g. `"eval/prompt"`
    /// for `stdlib/cli/eval/prompt.harn`).
    pub name: &'static str,
    /// Embedded source. Run via the existing `harn run` codepath by the
    /// dispatch wedge.
    pub source: &'static str,
}

pub const STDLIB_CLI_SCRIPTS: &[StdlibCliScript] = embedded_catalog!(StdlibCliScript, name, [
    "codemod" => "stdlib/cli/codemod.harn",
    "canon/check" => "stdlib/cli/canon/check.harn",
    "doctor" => "stdlib/cli/doctor.harn",
    "echo" => "stdlib/cli/echo.harn",
    // Helper module for the embedded `eval/*` scripts. Has a stub `main`
    // that exits non-zero — sibling scripts inline its helpers until the
    // dispatch wedge gains a cross-script import surface (#2300 / G7).
    "eval/_runner" => "stdlib/cli/eval/_runner.harn",
    "eval/context" => "stdlib/cli/eval/context.harn",
    "eval/model_selector" => "stdlib/cli/eval/model_selector.harn",
    "eval/tool_calls" => "stdlib/cli/eval/tool_calls.harn",
    "eval/coding_agent" => "stdlib/cli/eval/coding_agent.harn",
    "eval/scope_triage" => "stdlib/cli/eval/scope_triage.harn",
    "eval/prompt" => "stdlib/cli/eval/prompt.harn",
    "explain" => "stdlib/cli/explain.harn",
    "graph" => "stdlib/cli/graph.harn",
    "personas/compile_prompt" => "stdlib/cli/personas/compile_prompt.harn",
    "personas/materialize" => "stdlib/cli/personas/materialize.harn",
    "models/batch_plan" => "stdlib/cli/models/batch_plan.harn",
    "models/list" => "stdlib/cli/models/list.harn",
    "models/lora_inspect" => "stdlib/cli/models/lora_inspect.harn",
    "models/lora_export" => "stdlib/cli/models/lora_export.harn",
    "models/lora_manifest" => "stdlib/cli/models/lora_manifest.harn",
    "models/lora_preflight" => "stdlib/cli/models/lora_preflight.harn",
    "models/lora_promote" => "stdlib/cli/models/lora_promote.harn",
    "models/lora_plan" => "stdlib/cli/models/lora_plan.harn",
    "models/lora_train" => "stdlib/cli/models/lora_train.harn",
    "models/recommend" => "stdlib/cli/models/recommend.harn",
    "models/test" => "stdlib/cli/models/test.harn",
    "precompile" => "stdlib/cli/precompile.harn",
    "providers/cache_probe" => "stdlib/cli/providers/cache_probe.harn",
    "providers/catalog" => "stdlib/cli/providers/catalog.harn",
    "providers/probe" => "stdlib/cli/providers/probe.harn",
    "providers/recommend" => "stdlib/cli/providers/recommend.harn",
    "providers/tool_probe" => "stdlib/cli/providers/tool_probe.harn",
    "providers/tool_scorecard" => "stdlib/cli/providers/tool_scorecard.harn",
    "routes" => "stdlib/cli/routes.harn",
    "scan" => "stdlib/cli/scan.harn",
    "scaffold/init" => "stdlib/cli/scaffold/init.harn",
    "scaffold/tool_new" => "stdlib/cli/scaffold/tool_new.harn",
    "trace_import" => "stdlib/cli/trace_import.harn",
    "try" => "stdlib/cli/try.harn",
    "version" => "stdlib/cli/version.harn",
]);

pub fn get_stdlib_source(module: &str) -> Option<&'static str> {
    STDLIB_SOURCES
        .iter()
        .find_map(|entry| (entry.module == module).then_some(entry.source))
}

/// Builtins a stdlib module re-exports under its own name, so
/// `import { assert_eq } from "std/testing"` resolves.
///
/// Some of the standard library is implemented in Rust rather than Harn — the
/// value differ behind `assert_eq` needs to walk the runtime representation of
/// a value, which Harn source cannot do. Those builtins are callable without an
/// import, but a reader who has just typed `import { assert_throws } from
/// "std/testing"` has every reason to expect its sibling `assert_eq` to come
/// from the same place, and no reason to know which side of the Rust/Harn line
/// a given assertion happens to fall on. This table erases that seam: the
/// module's export surface is its `pub fn`s plus the names listed here.
///
/// The list is explicit rather than derived from the builtins' `category`
/// metadata: a category is a free-form label for docs and observability, and an
/// export surface is an API contract. Deriving one from the other would let an
/// unrelated metadata edit silently add or remove a public export.
///
/// `harn_vm::stdlib::tests` pins every name here to a registered builtin, so an
/// entry cannot rot into a name that no longer exists.
pub fn builtin_reexports(module: &str) -> &'static [&'static str] {
    match module {
        "testing" => &[
            "assert",
            "assert_approx",
            "assert_eq",
            "assert_matches",
            "assert_ne",
            "value_diff",
        ],
        _ => &[],
    }
}

/// Find an embedded CLI subcommand script by name. Returns the embedded
/// source string when present, or `None` if no script with that name is
/// registered in [`STDLIB_CLI_SCRIPTS`].
pub fn find_cli_script(name: &str) -> Option<&'static str> {
    STDLIB_CLI_SCRIPTS
        .iter()
        .find_map(|entry| (entry.name == name).then_some(entry.source))
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
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(offset);
                }
            }
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
        entrypoint_modules, find_cli_script, get_stdlib_prompt_asset, get_stdlib_source,
        matching_paren_len, parse_public_function_line, public_functions_for_module,
        STDLIB_CLI_SCRIPTS, STDLIB_PROMPT_ASSETS, STDLIB_SOURCES,
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
    fn cli_scripts_are_non_empty_and_uniquely_named() {
        let mut seen = BTreeSet::new();
        for entry in STDLIB_CLI_SCRIPTS {
            assert!(
                !entry.source.trim().is_empty(),
                "cli/{} should have non-empty source",
                entry.name
            );
            assert!(
                seen.insert(entry.name),
                "cli/{} is registered more than once in STDLIB_CLI_SCRIPTS",
                entry.name
            );
        }
    }

    #[test]
    fn find_cli_script_round_trips() {
        for entry in STDLIB_CLI_SCRIPTS {
            assert_eq!(find_cli_script(entry.name), Some(entry.source));
        }
        assert!(find_cli_script("not-a-real-script").is_none());
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
            "eval/stats",
            "eval/agreement",
            "edit",
            "verification",
            "disclosure",
            "artifact/web",
            "command",
            "waitpoint",
            "llm/handlers",
            "llm/tool_middleware",
            "llm/tool_binder",
            "llm/missing_tool_call",
            "llm/ensemble",
            "llm/rerank",
            "personas/prelude",
            "personas/bulletins",
            "agent/host_tools",
            "agent/host_injection",
            "agent/tool_lifecycle",
            "agent/user",
            "agent/governors",
            "agent/guardrails",
            "cli",
            "cli/models/batch_artifacts",
            "cli/models/batch_cancel",
            "cli/models/batch_download",
            "cli/models/batch_lifecycle",
            "cli/models/batch_status",
            "cli/models/batch_submit",
            "cli/models/batch_transport",
            "cli/models/lora_render",
            "llm/optimize",
            "llm/judge",
            "llm/faithfulness",
            "llm/refine",
            "connectors/shared",
            "connectors/github",
            "connectors/linear",
            "connectors/notion",
            "connectors/slack",
            "triage",
            "dashboard/jobs",
            "ui_resource",
            "host_lease",
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
            "std/agent/prompts/default_nudge.harn.prompt",
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
            "command_wait",
            "command_cancel",
            "command_run_streaming",
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
    fn disclosure_stdlib_module_exports_render_helpers() {
        let exports = public_functions_for_module("disclosure")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        assert!(
            exports.contains("render"),
            "std/disclosure should export render"
        );
        assert!(
            exports.contains("git_trailers"),
            "std/disclosure should export git_trailers"
        );
        assert!(
            exports.contains("slack_message_disclosure"),
            "std/disclosure should export slack_message_disclosure"
        );
        assert!(
            exports.contains("append_git_trailers"),
            "std/disclosure should export append_git_trailers"
        );
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

        let stop = exports
            .iter()
            .find(|function| function.name == "agent_stop")
            .expect("std/agent/workers should export agent_stop");
        assert_eq!(
            stop.signature,
            "agent_stop(worker, options: AgentStopOptions = nil)"
        );
        assert_eq!(stop.required_params, 1);
        assert_eq!(stop.total_params, 2);

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
    fn semver_stdlib_module_exports_release_helpers() {
        let exports = public_functions_for_module("semver")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "strip_v",
            "add_v",
            "is_v_semver",
            "parse",
            "max_canonical_tag",
            "next",
            "bump_type",
            "version_from_release_branch",
            "version_from_tag",
        ] {
            assert!(exports.contains(name), "std/semver should export {name}");
        }
    }

    #[test]
    fn changelog_stdlib_module_exports_typed_primitives() {
        let exports = public_functions_for_module("changelog")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "changelog_validate_categories",
            "changelog_parse_fragment",
            "changelog_order_fragments",
            "changelog_normalize_fragment_body",
            "changelog_assemble_fragments",
            "changelog_parse_sections",
            "changelog_find_section",
            "changelog_merge_unreleased",
        ] {
            assert!(exports.contains(name), "std/changelog should export {name}");
        }
    }

    #[test]
    fn text_stdlib_module_exports_regex_and_pad_helpers() {
        let exports = public_functions_for_module("text")
            .into_iter()
            .map(|function| function.name)
            .collect::<BTreeSet<_>>();
        for name in [
            "pad_left",
            "pad_right",
            "repeat_string",
            "regex_first_capture",
            "regex_capture_groups",
            "regex_all_first_captures",
        ] {
            assert!(exports.contains(name), "std/text should export {name}");
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

    #[test]
    fn matching_paren_len_closes_on_every_bracket_kind() {
        // Each closer kind must terminate the scan once depth returns to zero,
        // mirroring the symmetric opener arm and `split_top_level_params`.
        assert_eq!(matching_paren_len("a, b)"), Some(4));
        assert_eq!(matching_paren_len("x: [int])"), Some(8));
        assert_eq!(matching_paren_len("x: {a: int})"), Some(11));
        // A top-level `]`/`}` is the matching close for the consumed opener and
        // must return rather than scanning to the end and yielding `None`.
        assert_eq!(matching_paren_len("x]"), Some(1));
        assert_eq!(matching_paren_len("x}"), Some(1));
        assert_eq!(matching_paren_len("unterminated"), None);
    }

    #[test]
    fn parse_public_function_line_handles_record_typed_params() {
        let parsed =
            parse_public_function_line("pub fn configure(opts: {retries: int}) -> bool", None)
                .expect("signature with a record-typed parameter should parse");
        assert_eq!(parsed.name, "configure");
        assert_eq!(parsed.signature, "configure(opts: {retries: int}) -> bool");
        assert_eq!(parsed.total_params, 1);
        assert_eq!(parsed.required_params, 1);
    }
}
