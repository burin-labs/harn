//! Shared structural-record [`Ty::Shape`] aliases reused across builtin
//! signatures.
//!
//! These live in `harn-builtin-meta` (a dep-free crate) so that **both**
//! sides of the registry can name them:
//!
//! * the `#[harn_builtin]` proc-macro emits `sig` strings that reference a
//!   shape with the `@NAME` injection form (e.g.
//!   `options?: @LLM_CALL_OPTIONS`), resolving to `harn_builtin_meta::shapes::NAME`;
//! * `harn-parser`'s typechecker reads the same consts (re-exported from
//!   `builtin_signatures::signatures::shapes`).
//!
//! Before this module existed every agent/llm/io builtin that took or
//! returned a dict declared its slot as plain [`TY_DICT`], throwing away the
//! type checker's ability to flag typos in well-known option keys. Each
//! [`Ty::Shape`] here captures the *publicly documented* shape of one
//! option-bag or return contract. Adding new options to a shape only requires
//! editing this file — every call-site signature references the named const.
//!
//! Conventions:
//! - Fields appear in roughly the order documented in `docs/llm/harn-quickref.md`.
//! - Only the genuinely-required keys are marked non-optional. The type
//!   checker still accepts a generic `dict` (see
//!   `crates/harn-parser/src/typechecker/inference/subtyping.rs`), so this
//!   is purely additive: dict-literal callsites get checked, dynamic-dict
//!   callsites still work.

use crate::{
    ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_FLOAT,
    TY_INT, TY_LIST, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

const TY_BOOL_OR_DICT: Ty = Ty::Union(&[TY_BOOL, TY_DICT]);
const TY_BOOL_OR_DICT_OR_NIL: Ty = Ty::Union(&[TY_BOOL, TY_DICT, TY_NIL]);
const TY_FLOAT_OR_NIL: Ty = Ty::Union(&[TY_FLOAT, TY_NIL]);
const TY_INT_OR_FLOAT_OR_DICT: Ty = Ty::Union(&[TY_INT, TY_FLOAT, TY_DICT]);
const TY_LIST_OR_STRING: Ty = Ty::Union(&[TY_LIST, TY_STRING]);
const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);
const TY_STRING_OR_DICT_OR_BOOL: Ty = Ty::Union(&[TY_STRING, TY_DICT, TY_BOOL]);
const TY_STRING_OR_DICT_OR_NIL: Ty = Ty::Union(&[TY_STRING, TY_DICT, TY_NIL]);
const TY_STRING_OR_LIST: Ty = Ty::Union(&[TY_STRING, TY_LIST]);
const TY_TOOL_REGISTRY_OR_LIST: Ty = Ty::Union(&[TY_LIST, TY_DICT]);

// ---------------------------------------------------------------------------
// Agent option bags
// ---------------------------------------------------------------------------

const RESUME_TIMEOUT_ACTION: Ty = Ty::Union(&[
    Ty::LitString("resume_with_summary"),
    Ty::LitString("fail"),
    Ty::LitString("resume_with_input"),
]);

pub const RESUME_TIMEOUT_SPEC: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("duration_minutes", TY_INT),
    ShapeFieldDescriptor::optional("on_timeout", RESUME_TIMEOUT_ACTION),
]);

pub const RESUME_CONDITIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("trigger", TY_DICT),
    ShapeFieldDescriptor::optional("timeout", RESUME_TIMEOUT_SPEC),
    ShapeFieldDescriptor::optional("on_event", TY_STRING),
]);

pub const RESUME_CONDITIONS_OR_NIL: Ty = Ty::Union(&[RESUME_CONDITIONS, TY_NIL]);

/// Configuration accepted by `spawn_agent` and `agent.spawn`.
///
/// `task` is the only required field; everything else has stdlib-side defaults.
/// `graph` xor `node` is required at runtime — the type checker doesn't enforce
/// that constraint (would require a sum-of-shapes), but the runtime returns a
/// clear error.
pub const AGENT_SPAWN_CONFIG: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("task", TY_STRING),
    ShapeFieldDescriptor::optional("name", TY_STRING),
    ShapeFieldDescriptor::optional("wait", TY_BOOL),
    ShapeFieldDescriptor::optional("graph", TY_ANY),
    ShapeFieldDescriptor::optional("node", TY_ANY),
    ShapeFieldDescriptor::optional("artifacts", TY_LIST),
    ShapeFieldDescriptor::optional("transcript", TY_ANY),
    ShapeFieldDescriptor::optional("permissions", TY_ANY),
    ShapeFieldDescriptor::optional("options", TY_DICT),
    ShapeFieldDescriptor::optional("execution", TY_DICT),
    ShapeFieldDescriptor::optional("audit", TY_DICT),
    ShapeFieldDescriptor::optional("carry", TY_DICT),
    ShapeFieldDescriptor::optional("policy", TY_ANY),
    ShapeFieldDescriptor::optional("tools", TY_LIST),
]);

/// Options dict accepted by `sub_agent_run` / `sub_agent_request` as their
/// second positional argument. `task` is provided separately as the first
/// positional arg, so this shape has *all* fields optional.
pub const SUB_AGENT_OPTIONS: Ty = Ty::Shape(&[
    // Request envelope / worker controls.
    ShapeFieldDescriptor::optional("_type", TY_STRING),
    ShapeFieldDescriptor::optional("name", TY_STRING),
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("background", TY_BOOL),
    ShapeFieldDescriptor::optional("carry", TY_DICT),
    ShapeFieldDescriptor::optional("allowed_tools", TY_LIST),
    ShapeFieldDescriptor::optional("policy", TY_ANY),
    ShapeFieldDescriptor::optional("anchor", WORKSPACE_ANCHOR_INPUT),
    ShapeFieldDescriptor::optional("reminder_propagation", TY_LIST),
    ShapeFieldDescriptor::optional("resume_when", RESUME_CONDITIONS),
    ShapeFieldDescriptor::optional("returns_schema", TY_ANY),
    ShapeFieldDescriptor::optional("returns", TY_DICT),
    ShapeFieldDescriptor::optional("execution", TY_DICT),
    ShapeFieldDescriptor::optional("request", TY_ANY),
    ShapeFieldDescriptor::optional("research_questions", TY_LIST),
    ShapeFieldDescriptor::optional("questions", TY_LIST),
    ShapeFieldDescriptor::optional("action_items", TY_LIST),
    ShapeFieldDescriptor::optional("actions", TY_LIST),
    ShapeFieldDescriptor::optional("workflow_stages", TY_LIST),
    ShapeFieldDescriptor::optional("stages", TY_LIST),
    ShapeFieldDescriptor::optional("verification_steps", TY_LIST),
    ShapeFieldDescriptor::optional("verification", TY_LIST),
    // Provider / LLM-call controls forwarded through agent_loop.
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_p", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_k", TY_INT),
    ShapeFieldDescriptor::optional("stop", TY_STRING_OR_LIST),
    ShapeFieldDescriptor::optional("seed", TY_INT),
    ShapeFieldDescriptor::optional("frequency_penalty", TY_FLOAT),
    ShapeFieldDescriptor::optional("presence_penalty", TY_FLOAT),
    ShapeFieldDescriptor::optional("response_format", TY_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("schema", TY_ANY),
    ShapeFieldDescriptor::optional("schema_retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_recover", TY_BOOL),
    ShapeFieldDescriptor::optional("cache", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("transcript", TY_ANY),
    ShapeFieldDescriptor::optional("budget", TY_INT_OR_FLOAT_OR_DICT),
    ShapeFieldDescriptor::optional("budget_usd", Ty::Union(&[TY_INT, TY_FLOAT])),
    ShapeFieldDescriptor::optional("mock", TY_ANY),
    ShapeFieldDescriptor::optional("messages", TY_LIST),
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("tool_choice", TY_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("thinking", TY_ANY),
    ShapeFieldDescriptor::optional("reasoning_effort", TY_STRING),
    ShapeFieldDescriptor::optional("interleaved_thinking", TY_BOOL),
    ShapeFieldDescriptor::optional("anthropic_beta_features", TY_LIST),
    // Agent-loop controls.
    ShapeFieldDescriptor::optional("profile", TY_STRING),
    ShapeFieldDescriptor::optional("loop_until_done", TY_BOOL),
    ShapeFieldDescriptor::optional("done_sentinel", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("max_iterations", TY_INT),
    ShapeFieldDescriptor::optional("iteration_budget", TY_STRING_OR_DICT_OR_NIL),
    ShapeFieldDescriptor::optional("loop_control", TY_ANY),
    ShapeFieldDescriptor::optional("max_nudges", TY_INT),
    ShapeFieldDescriptor::optional("nudge", TY_STRING),
    ShapeFieldDescriptor::optional("llm_caller", TY_ANY),
    ShapeFieldDescriptor::optional("llm_caller_transport", TY_DICT),
    ShapeFieldDescriptor::optional("tool_caller", TY_ANY),
    ShapeFieldDescriptor::optional("reasoning_policy", TY_ANY),
    ShapeFieldDescriptor::optional("thinking_policy", TY_ANY),
    ShapeFieldDescriptor::optional("reasoning_scale", TY_STRING),
    ShapeFieldDescriptor::optional("problem_scale", TY_STRING),
    ShapeFieldDescriptor::optional("reasoning_task", TY_STRING),
    ShapeFieldDescriptor::optional("task_kind", TY_STRING),
    ShapeFieldDescriptor::optional("tools", TY_TOOL_REGISTRY_OR_LIST),
    ShapeFieldDescriptor::optional("subagents", TY_BOOL),
    ShapeFieldDescriptor::optional("subagent_tools", TY_BOOL),
    ShapeFieldDescriptor::optional("tool_format", TY_STRING),
    ShapeFieldDescriptor::optional("native_tool_fallback", TY_STRING),
    ShapeFieldDescriptor::optional("tool_search", TY_DICT),
    ShapeFieldDescriptor::optional("tool_retries", TY_INT),
    ShapeFieldDescriptor::optional("tool_backoff_ms", TY_INT),
    ShapeFieldDescriptor::optional("stop_after_successful_tools", TY_LIST),
    ShapeFieldDescriptor::optional("require_successful_tools", TY_LIST),
    ShapeFieldDescriptor::optional("turn_policy", TY_DICT),
    ShapeFieldDescriptor::optional("require_action_or_yield", TY_BOOL),
    ShapeFieldDescriptor::optional("tool_examples", TY_STRING),
    ShapeFieldDescriptor::optional("shared_types", TY_STRING),
    ShapeFieldDescriptor::optional("stall_diagnostics", TY_BOOL_OR_DICT_OR_NIL),
    ShapeFieldDescriptor::optional("permissions", TY_ANY),
    ShapeFieldDescriptor::optional("approval_policy", TY_ANY),
    ShapeFieldDescriptor::optional("command_policy", TY_ANY),
    ShapeFieldDescriptor::optional("autonomy_budget", TY_ANY),
    ShapeFieldDescriptor::optional("token_budget", TY_INT),
    ShapeFieldDescriptor::optional("output_format", TY_ANY),
    ShapeFieldDescriptor::optional("json_schema", TY_ANY),
    ShapeFieldDescriptor::optional("output_schema", TY_ANY),
    ShapeFieldDescriptor::optional("root_task", TY_STRING),
    ShapeFieldDescriptor::optional("deliverables", TY_LIST),
    ShapeFieldDescriptor::optional("task_ledger", TY_DICT),
    ShapeFieldDescriptor::optional("persona", TY_STRING),
    // Daemon / compaction / prompt-context controls.
    ShapeFieldDescriptor::optional("daemon", TY_BOOL),
    ShapeFieldDescriptor::optional("persist_path", TY_STRING),
    ShapeFieldDescriptor::optional("resume_path", TY_STRING),
    ShapeFieldDescriptor::optional("wake_interval_ms", TY_INT),
    ShapeFieldDescriptor::optional("watch_paths", TY_LIST_OR_STRING),
    ShapeFieldDescriptor::optional("consolidate_on_idle", TY_BOOL),
    ShapeFieldDescriptor::optional("compaction", TY_STRING_OR_DICT_OR_BOOL),
    ShapeFieldDescriptor::optional("auto_compact", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("compact_threshold", TY_INT),
    ShapeFieldDescriptor::optional("compact_keep_first", TY_INT),
    ShapeFieldDescriptor::optional("compact_keep_last", TY_INT),
    ShapeFieldDescriptor::optional("compact_strategy", TY_STRING),
    ShapeFieldDescriptor::optional("compact_callback", TY_ANY),
    ShapeFieldDescriptor::optional("idle_watchdog_attempts", TY_INT),
    ShapeFieldDescriptor::optional("context_callback", TY_ANY),
    ShapeFieldDescriptor::optional("context_filter", TY_ANY),
    ShapeFieldDescriptor::optional("timestamp_messages", TY_BOOL),
    ShapeFieldDescriptor::optional("message_timestamps", TY_BOOL),
    ShapeFieldDescriptor::optional("message_decorator", TY_ANY),
    ShapeFieldDescriptor::optional("decorate_message", TY_ANY),
    ShapeFieldDescriptor::optional("prompts", TY_DICT),
    ShapeFieldDescriptor::optional("prompt_overrides", TY_DICT),
    ShapeFieldDescriptor::optional("post_turn_callback", TY_ANY),
    ShapeFieldDescriptor::optional("verify_completion", TY_ANY),
    ShapeFieldDescriptor::optional("verify_completion_judge", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("done_judge", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("max_verify_attempts", TY_INT),
    ShapeFieldDescriptor::optional("llm_transcript_dir", TY_STRING),
    ShapeFieldDescriptor::optional("skills", TY_ANY),
    ShapeFieldDescriptor::optional("skill_registry", TY_ANY),
    ShapeFieldDescriptor::optional("skill_match", TY_DICT),
    ShapeFieldDescriptor::optional("skill_catalog_limit", TY_INT),
    ShapeFieldDescriptor::optional("skill_catalog_budget", TY_INT),
    ShapeFieldDescriptor::optional("skill_catalog_always", TY_BOOL),
    ShapeFieldDescriptor::optional("working_files", TY_LIST_OR_STRING),
    ShapeFieldDescriptor::optional("mcp_servers", TY_LIST),
    ShapeFieldDescriptor::optional("mcp_initialize_advisory", TY_BOOL),
    ShapeFieldDescriptor::optional("mcp_context", TY_DICT),
    ShapeFieldDescriptor::optional("system_preamble", TY_ANY),
    ShapeFieldDescriptor::optional("system_prefix", TY_ANY),
    ShapeFieldDescriptor::optional("system_context", TY_ANY),
    ShapeFieldDescriptor::optional("system_prompt_parts", TY_ANY),
    ShapeFieldDescriptor::optional("system_appendix", TY_ANY),
    ShapeFieldDescriptor::optional("system_suffix", TY_ANY),
]);

/// Input shape for `agent_session_open` `opts.workspace_anchor` and
/// `agent_session_set_workspace_anchor`. `anchored_at` defaults to now()
/// when omitted; `additional_roots` defaults to empty.
pub const WORKSPACE_ANCHOR_INPUT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("primary", TY_STRING),
    ShapeFieldDescriptor::optional("additional_roots", TY_LIST),
    ShapeFieldDescriptor::optional("anchored_at", TY_STRING),
]);

/// Snapshot shape returned by `agent_session_workspace_anchor`.
pub const WORKSPACE_ANCHOR_SNAPSHOT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("primary", TY_STRING),
    ShapeFieldDescriptor::new("additional_roots", TY_LIST),
    ShapeFieldDescriptor::new("anchored_at", TY_STRING),
]);

pub const WORKSPACE_ANCHOR_OR_NIL: Ty = Ty::Union(&[WORKSPACE_ANCHOR_SNAPSHOT, TY_NIL]);

/// Session-local workspace policy defaults.
pub const WORKSPACE_POLICY: Ty = Ty::Shape(&[ShapeFieldDescriptor::optional(
    "default_mount_mode",
    TY_STRING,
)]);

// ---------------------------------------------------------------------------
// LLM option bags
// ---------------------------------------------------------------------------

/// Options dict for `llm_call`, `llm_call_safe`, `llm_call_structured`,
/// `llm_stream_call`, and friends. Mirrors the runtime extractor in
/// `crates/harn-vm/src/llm/helpers/options.rs` plus the structured-call
/// conveniences rewritten before extraction.
pub const LLM_CALL_OPTIONS: Ty = Ty::Shape(&[
    // Routing.
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("model_role", TY_STRING),
    ShapeFieldDescriptor::optional("role", TY_STRING),
    ShapeFieldDescriptor::optional("model_tier", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("api_mode", TY_STRING),
    ShapeFieldDescriptor::optional("api", TY_STRING),
    ShapeFieldDescriptor::optional("route_policy", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("prefer", Ty::Union(&[TY_STRING, TY_LIST])),
    ShapeFieldDescriptor::optional("fallback_strategy", TY_STRING),
    ShapeFieldDescriptor::optional("strategy", TY_STRING),
    ShapeFieldDescriptor::optional("fallback_chain", Ty::Union(&[TY_STRING, TY_LIST])),
    ShapeFieldDescriptor::optional("budget_usd", Ty::Union(&[TY_FLOAT, TY_INT])),
    ShapeFieldDescriptor::optional("routing", TY_DICT),
    // Model ladders: `models:` inline steps, `ladder:` named catalog ladder.
    ShapeFieldDescriptor::optional("models", TY_LIST),
    ShapeFieldDescriptor::optional("ladder", TY_STRING),
    // Conversation and system-prompt composition.
    ShapeFieldDescriptor::optional("system", TY_STRING),
    ShapeFieldDescriptor::optional("messages", TY_LIST),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("system_preamble", TY_ANY),
    ShapeFieldDescriptor::optional("system_prefix", TY_ANY),
    ShapeFieldDescriptor::optional("system_context", TY_ANY),
    ShapeFieldDescriptor::optional("system_prompt_parts", TY_ANY),
    ShapeFieldDescriptor::optional("system_appendix", TY_ANY),
    ShapeFieldDescriptor::optional("system_suffix", TY_ANY),
    ShapeFieldDescriptor::optional("_system_fragments", TY_LIST),
    ShapeFieldDescriptor::optional("context_profile", TY_DICT),
    ShapeFieldDescriptor::optional("project_context_profile", TY_DICT),
    ShapeFieldDescriptor::optional("caps", TY_ANY),
    ShapeFieldDescriptor::optional("capabilities", TY_ANY),
    ShapeFieldDescriptor::optional("previous_response_id", TY_STRING),
    // Generation.
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_p", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_k", TY_INT),
    ShapeFieldDescriptor::optional("logprobs", TY_BOOL),
    ShapeFieldDescriptor::optional("top_logprobs", TY_INT),
    ShapeFieldDescriptor::optional("stop", Ty::Union(&[TY_STRING, TY_LIST])),
    ShapeFieldDescriptor::optional("seed", TY_INT),
    ShapeFieldDescriptor::optional("frequency_penalty", TY_FLOAT),
    ShapeFieldDescriptor::optional("presence_penalty", TY_FLOAT),
    // Structured output.
    ShapeFieldDescriptor::optional("response_format", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("output_format", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("schema", TY_ANY),
    ShapeFieldDescriptor::optional("json_schema", TY_ANY),
    ShapeFieldDescriptor::optional("output_schema", TY_ANY),
    ShapeFieldDescriptor::optional("output_validation", TY_STRING),
    ShapeFieldDescriptor::optional("schema_retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_retry_nudge", Ty::Union(&[TY_BOOL, TY_STRING])),
    ShapeFieldDescriptor::optional("retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_recover", TY_BOOL),
    ShapeFieldDescriptor::optional("repair", Ty::Union(&[TY_BOOL, TY_DICT])),
    ShapeFieldDescriptor::optional("llm_repair", Ty::Union(&[TY_BOOL, TY_DICT])),
    // Reasoning / multimodal options.
    ShapeFieldDescriptor::optional("thinking", Ty::Union(&[TY_BOOL, TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("reasoning_policy", TY_ANY),
    ShapeFieldDescriptor::optional("thinking_policy", TY_ANY),
    ShapeFieldDescriptor::optional("reasoning_scale", TY_STRING),
    ShapeFieldDescriptor::optional("problem_scale", TY_STRING),
    ShapeFieldDescriptor::optional("reasoning_task", TY_STRING),
    ShapeFieldDescriptor::optional("task_kind", TY_STRING),
    ShapeFieldDescriptor::optional("task", TY_STRING),
    ShapeFieldDescriptor::optional("reasoning_effort", TY_STRING),
    ShapeFieldDescriptor::optional("interleaved_thinking", TY_BOOL),
    ShapeFieldDescriptor::optional("anthropic_beta_features", Ty::Union(&[TY_STRING, TY_LIST])),
    ShapeFieldDescriptor::optional("vision", TY_BOOL),
    ShapeFieldDescriptor::optional("audio", TY_BOOL),
    ShapeFieldDescriptor::optional("pdf", TY_BOOL),
    ShapeFieldDescriptor::optional("video", TY_BOOL),
    // Tools and progressive disclosure. Runtime accepts either a raw tool
    // list or a tool_registry dict.
    ShapeFieldDescriptor::optional("tools", Ty::Union(&[TY_LIST, TY_DICT])),
    ShapeFieldDescriptor::optional("provider_tools", Ty::Union(&[TY_LIST, TY_DICT])),
    ShapeFieldDescriptor::optional("hosted_tools", Ty::Union(&[TY_LIST, TY_DICT])),
    ShapeFieldDescriptor::optional("tool_choice", Ty::Union(&[TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("tool_search", Ty::Union(&[TY_BOOL, TY_STRING, TY_DICT])),
    ShapeFieldDescriptor::optional("tool_format", TY_STRING),
    // Caching, budgets, retries, and transport.
    ShapeFieldDescriptor::optional("cache", Ty::Union(&[TY_BOOL, TY_DICT])),
    ShapeFieldDescriptor::optional("budget", Ty::Union(&[TY_FLOAT, TY_INT, TY_DICT])),
    ShapeFieldDescriptor::optional("timeout", TY_INT),
    ShapeFieldDescriptor::optional("timeout_ms", TY_INT),
    ShapeFieldDescriptor::optional("idle_timeout", TY_INT),
    ShapeFieldDescriptor::optional("stream", TY_BOOL),
    ShapeFieldDescriptor::optional("fast", TY_BOOL),
    ShapeFieldDescriptor::optional("speed", TY_STRING),
    ShapeFieldDescriptor::optional("store", Ty::Union(&[TY_BOOL, TY_DICT])),
    ShapeFieldDescriptor::optional("response_store", TY_BOOL),
    ShapeFieldDescriptor::optional("responses_store", TY_BOOL),
    ShapeFieldDescriptor::optional("background", TY_BOOL),
    ShapeFieldDescriptor::optional("truncation", TY_STRING),
    ShapeFieldDescriptor::optional("compact", TY_BOOL),
    ShapeFieldDescriptor::optional("include", TY_LIST),
    ShapeFieldDescriptor::optional("max_tool_calls", TY_INT),
    // Provider-specific and advanced request shaping.
    ShapeFieldDescriptor::optional("anthropic", TY_DICT),
    ShapeFieldDescriptor::optional("openai", TY_DICT),
    ShapeFieldDescriptor::optional("openrouter", TY_DICT),
    ShapeFieldDescriptor::optional("together", TY_DICT),
    ShapeFieldDescriptor::optional("groq", TY_DICT),
    ShapeFieldDescriptor::optional("cerebras", TY_DICT),
    ShapeFieldDescriptor::optional("deepseek", TY_DICT),
    ShapeFieldDescriptor::optional("fireworks", TY_DICT),
    ShapeFieldDescriptor::optional("huggingface", TY_DICT),
    ShapeFieldDescriptor::optional("local", TY_DICT),
    ShapeFieldDescriptor::optional("mlx", TY_DICT),
    ShapeFieldDescriptor::optional("vllm", TY_DICT),
    ShapeFieldDescriptor::optional("tgi", TY_DICT),
    ShapeFieldDescriptor::optional("dashscope", TY_DICT),
    ShapeFieldDescriptor::optional("gemini", TY_DICT),
    ShapeFieldDescriptor::optional("azure_openai", TY_DICT),
    ShapeFieldDescriptor::optional("bedrock", TY_DICT),
    ShapeFieldDescriptor::optional("ollama", TY_DICT),
    ShapeFieldDescriptor::optional("vertex", TY_DICT),
    ShapeFieldDescriptor::optional("mock", TY_ANY),
    ShapeFieldDescriptor::optional("fake", TY_DICT),
    ShapeFieldDescriptor::optional("prefill", TY_STRING),
    ShapeFieldDescriptor::optional(
        "structural_experiment",
        Ty::Union(&[TY_STRING, TY_DICT, TY_CLOSURE]),
    ),
    // Legacy/runtime-adjacent keys handled at the boundary.
    ShapeFieldDescriptor::optional("transcript", TY_ANY),
    ShapeFieldDescriptor::optional("reminders", TY_ANY),
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("_iteration", TY_INT),
]);

/// Options dict for `signal_install` / cooperative signal handler primitives.
pub const SIGNAL_HANDLER_OPTIONS: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::optional("once", TY_BOOL),
    ShapeFieldDescriptor::optional("restore", TY_BOOL),
]);

// ---------------------------------------------------------------------------
// Return contracts
// ---------------------------------------------------------------------------

/// Return type of `spawn_agent`, `wait_agent` (scalar form), `worker_*` lookups
/// and `sub_agent_run` background mode. Mirrors `clone_worker_state` in
/// `crates/harn-vm/src/stdlib/agents_workers/mod.rs`.
pub const WORKER_SUMMARY: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("_type", Ty::LitString("agent_handle")),
    ShapeFieldDescriptor::new("id", TY_STRING),
    ShapeFieldDescriptor::new("name", TY_STRING),
    ShapeFieldDescriptor::new("task", TY_STRING),
    ShapeFieldDescriptor::new("mode", TY_STRING),
    ShapeFieldDescriptor::new("status", TY_STRING),
    ShapeFieldDescriptor::new("created_at", TY_STRING),
    ShapeFieldDescriptor::new("started_at", TY_STRING),
    ShapeFieldDescriptor::optional("finished_at", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("awaiting_started_at", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("history", TY_LIST),
    ShapeFieldDescriptor::optional("request", TY_DICT_OR_NIL),
    ShapeFieldDescriptor::new("provenance", TY_DICT),
    ShapeFieldDescriptor::optional("result", TY_ANY),
    ShapeFieldDescriptor::optional("error", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("artifact_count", TY_INT),
    ShapeFieldDescriptor::new("has_transcript", TY_BOOL),
    ShapeFieldDescriptor::optional("parent_worker_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("parent_stage_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("child_run_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::optional("child_run_path", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("execution", TY_DICT),
    ShapeFieldDescriptor::new("snapshot_path", TY_STRING),
    ShapeFieldDescriptor::new("audit", TY_DICT),
    ShapeFieldDescriptor::optional("suspension", TY_DICT_OR_NIL),
]);

/// User-facing result returned by foreground `sub_agent_run`. Background mode
/// returns [`WORKER_SUMMARY`] instead.
pub const SUB_AGENT_RESULT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("ok", TY_BOOL),
    ShapeFieldDescriptor::new("summary", TY_STRING),
    ShapeFieldDescriptor::new("artifacts", TY_LIST),
    ShapeFieldDescriptor::new("evidence_added", TY_INT),
    ShapeFieldDescriptor::new("tokens_used", TY_INT),
    ShapeFieldDescriptor::new("budget_exceeded", TY_BOOL),
    ShapeFieldDescriptor::new("data", TY_ANY),
    ShapeFieldDescriptor::new("error", TY_ANY),
    ShapeFieldDescriptor::new("session_id", TY_STRING),
    ShapeFieldDescriptor::new("transcript", TRANSCRIPT),
]);

/// Canonical transcript dict returned by the transcript lifecycle builtins.
pub const TRANSCRIPT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("_type", Ty::LitString("transcript")),
    ShapeFieldDescriptor::new("version", TY_INT),
    ShapeFieldDescriptor::new("id", TY_STRING),
    ShapeFieldDescriptor::new("messages", TY_LIST),
    ShapeFieldDescriptor::new("events", TY_LIST),
    ShapeFieldDescriptor::new("assets", TY_LIST),
    ShapeFieldDescriptor::optional("summary", TY_STRING),
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("state", TY_STRING),
    ShapeFieldDescriptor::optional("archived_messages", TY_INT),
]);

/// `agent_session_snapshot(id)` returns the transcript plus session lineage
/// and prompt/tool metadata.
pub const SESSION_SNAPSHOT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("_type", Ty::LitString("transcript")),
    ShapeFieldDescriptor::new("version", TY_INT),
    ShapeFieldDescriptor::new("id", TY_STRING),
    ShapeFieldDescriptor::new("length", TY_INT),
    ShapeFieldDescriptor::new("messages", TY_LIST),
    ShapeFieldDescriptor::new("events", TY_LIST),
    ShapeFieldDescriptor::new("assets", TY_LIST),
    ShapeFieldDescriptor::new("created_at", TY_STRING),
    ShapeFieldDescriptor::new("parent_id", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("child_ids", TY_LIST),
    ShapeFieldDescriptor::new("branched_at_event_index", Ty::Union(&[TY_INT, TY_NIL])),
    ShapeFieldDescriptor::new("system_prompt", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("tool_format", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("pinned_model", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("pinned_reasoning_policy", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("workspace_anchor", WORKSPACE_ANCHOR_OR_NIL),
    ShapeFieldDescriptor::new("workspace_policy", WORKSPACE_POLICY),
    ShapeFieldDescriptor::optional("summary", TY_STRING),
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("state", TY_STRING),
]);

/// Token and provider-cache accounting embedded in `llm_call` results.
pub const LLM_USAGE: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("input_tokens", TY_INT),
    ShapeFieldDescriptor::new("output_tokens", TY_INT),
    ShapeFieldDescriptor::new("cost_usd", TY_FLOAT_OR_NIL),
    ShapeFieldDescriptor::new("cache_read_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_write_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_creation_input_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_hit_ratio", TY_FLOAT),
    ShapeFieldDescriptor::new("cache_savings_usd", TY_FLOAT),
]);

/// Harn-facing response dict assembled by `vm_build_llm_result` for
/// `llm_call` and `llm_completion`.
pub const LLM_CALL_RESULT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("text", TY_STRING),
    ShapeFieldDescriptor::new("model", TY_STRING),
    ShapeFieldDescriptor::new("provider", TY_STRING),
    ShapeFieldDescriptor::new("input_tokens", TY_INT),
    ShapeFieldDescriptor::new("output_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_read_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_write_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_creation_input_tokens", TY_INT),
    ShapeFieldDescriptor::new("cache_hit_ratio", TY_FLOAT),
    ShapeFieldDescriptor::new("cache_savings_usd", TY_FLOAT),
    ShapeFieldDescriptor::new("usage", LLM_USAGE),
    ShapeFieldDescriptor::new("native_tool_calls", TY_LIST),
    ShapeFieldDescriptor::new("prose", TY_STRING),
    ShapeFieldDescriptor::new("visible_text", TY_STRING),
    ShapeFieldDescriptor::new("blocks", TY_LIST),
    ShapeFieldDescriptor::optional("data", TY_ANY),
    ShapeFieldDescriptor::new("tool_calls", TY_LIST),
    ShapeFieldDescriptor::optional("protocol_violations", TY_LIST),
    ShapeFieldDescriptor::optional("tool_parse_errors", TY_LIST),
    ShapeFieldDescriptor::optional("done_marker", TY_STRING),
    ShapeFieldDescriptor::optional("canonical_text", TY_STRING),
    ShapeFieldDescriptor::optional("thinking", TY_STRING),
    ShapeFieldDescriptor::optional("private_reasoning", TY_STRING),
    ShapeFieldDescriptor::optional("thinking_summary", TY_STRING),
    ShapeFieldDescriptor::optional("stop_reason", TY_STRING),
    ShapeFieldDescriptor::new("transcript", TRANSCRIPT),
    ShapeFieldDescriptor::optional("logprobs", TY_LIST),
]);

/// Error dict surfaced by `llm_call` throws and `llm_call_safe`.
pub const LLM_CALL_ERROR: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("category", TY_STRING),
    ShapeFieldDescriptor::new("kind", TY_STRING),
    ShapeFieldDescriptor::new("reason", TY_STRING),
    ShapeFieldDescriptor::new("message", TY_STRING),
    ShapeFieldDescriptor::optional("status", TY_INT),
    ShapeFieldDescriptor::optional("retry_after_ms", TY_INT),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("model", TY_STRING),
]);

/// Non-throwing envelope returned by `llm_call_safe`.
pub const LLM_CALL_SAFE_RESULT: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("ok", TY_BOOL),
    ShapeFieldDescriptor::new("response", LLM_CALL_RESULT),
    ShapeFieldDescriptor::new("error", LLM_CALL_ERROR),
]);

/// Diagnostic envelope returned by `schema_recover`. `data` narrows to
/// `T | nil` so callers can dispatch on `ok` and unwrap on success. Other
/// envelope fields are stably-typed regardless of `T`.
pub const SCHEMA_RECOVER_ENVELOPE: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("ok", TY_BOOL),
    ShapeFieldDescriptor::new("data", Ty::Union(&[Ty::Generic("T"), TY_NIL])),
    ShapeFieldDescriptor::new("raw_text", TY_STRING),
    ShapeFieldDescriptor::new("error", TY_STRING),
    ShapeFieldDescriptor::new("error_category", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("attempts", TY_INT),
    ShapeFieldDescriptor::new("stage", TY_STRING),
    ShapeFieldDescriptor::new("repaired", TY_BOOL),
]);
