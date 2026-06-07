//! Agent / orchestration / sub-agent builtin signatures.

use super::shapes::{
    AGENT_SPAWN_CONFIG, LLM_CALL_RESULT, RESUME_CONDITIONS_OR_NIL, SESSION_SNAPSHOT,
    SUB_AGENT_OPTIONS, SUB_AGENT_RESULT, TRANSCRIPT, WORKER_SUMMARY,
};
use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_FLOAT,
    TY_INT, TY_LIST, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

/// `list | dict | Transcript | SessionSnapshot` — used for
/// transcripts/message-list arguments where either a raw `messages` list, a
/// dynamic transcript dict, or one of the typed transcript shapes is accepted.
const TY_MESSAGES_OR_TRANSCRIPT: Ty = Ty::Union(&[TY_LIST, TY_DICT, TRANSCRIPT, SESSION_SNAPSHOT]);

/// `list | dict | Transcript | SessionSnapshot | nil` — read-only transcript
/// helpers are often fed values through optional chaining. Runtime validation
/// still rejects nil for the core transcript readers; this keeps the static
/// checker aligned with established call sites that know the value is present.
const TY_MESSAGES_OR_TRANSCRIPT_OR_NIL: Ty =
    Ty::Union(&[TY_LIST, TY_DICT, TRANSCRIPT, SESSION_SNAPSHOT, TY_NIL]);

/// `dict | Schema<any>` — schema aliases type-check as `Schema<T>` but
/// compile down to JSON-Schema dictionaries at runtime.
const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

/// `dict | list` — return type for `wait_agent`, which yields a single
/// worker summary for a scalar handle and a list of summaries when
/// passed a list of handles.
const TY_DICT_OR_LIST: Ty = Ty::Union(&[TY_DICT, TY_LIST]);

/// `float | nil` — return for `llm_budget_remaining` (nil when no
/// budget is set).
const TY_FLOAT_OR_NIL: Ty = Ty::Union(&[TY_FLOAT, TY_NIL]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature::simple(
        "add_assistant",
        &[
            Param::new("messages_or_transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("content", TY_ANY),
        ],
        TY_MESSAGES_OR_TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "add_message",
        &[
            Param::new("messages_or_transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("role", TY_STRING),
            Param::new("content", TY_ANY),
        ],
        TY_MESSAGES_OR_TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "add_system",
        &[
            Param::new("messages_or_transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("content", TY_ANY),
        ],
        TY_MESSAGES_OR_TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "add_tool_result",
        &[
            Param::new("messages_or_transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("tool_use_id", TY_STRING),
            Param::new("content", TY_ANY),
        ],
        TY_MESSAGES_OR_TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "add_user",
        &[
            Param::new("messages_or_transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("content", TY_ANY),
        ],
        TY_MESSAGES_OR_TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "agent",
        &[
            Param::new("name", TY_STRING),
            Param::optional("config", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_await_resumption",
        &[
            Param::new("reason", TY_ANY),
            Param::optional("conditions", RESUME_CONDITIONS_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_config",
        &[Param::new("agent", TY_DICT), Param::new("prompt", TY_ANY)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_daemon_snapshot",
        &[
            Param::new("session", TY_DICT),
            Param::new("opts", TY_DICT),
            Param::optional("daemon_state", TY_STRING),
            Param::optional("iteration", TY_INT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_daemon_step",
        &[
            Param::new("session", TY_DICT),
            Param::new("opts", TY_DICT),
            Param::new("iteration", TY_INT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_dispatch_tool_batch",
        &[
            Param::new("calls", TY_LIST),
            Param::optional("tools", TY_DICT_OR_NIL),
            Param::optional("options", TY_DICT),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "agent_dispatch_tool_call",
        &[
            Param::new("call", TY_DICT),
            Param::optional("tools", TY_DICT_OR_NIL),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_chat_loop",
        &[Param::optional("opts", TY_DICT_OR_NIL)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_chat_route_input",
        &[
            Param::new("line", TY_ANY),
            Param::optional("state", TY_DICT_OR_NIL),
            Param::optional("handlers", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_chat_wait_for_user_tools",
        &[Param::optional("registry", TY_DICT_OR_NIL)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_llm_turn",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("system", TY_STRING_OR_NIL),
            Param::optional("options", TY_DICT),
        ],
        LLM_CALL_RESULT,
    ),
    BuiltinSignature::simple(
        "agent_loop",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("system", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_lifecycle_tools",
        &[
            Param::optional("registry", TY_DICT_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_parse_tool_calls",
        &[
            Param::new("text", TY_STRING),
            Param::optional("tools", TY_DICT_OR_NIL),
            Param::optional("tool_format", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_stop",
        &[
            Param::new("worker", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_turn",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("opts", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_preset",
        &[
            Param::new("kind", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_budget",
        &[
            Param::new("kind_or_options", Ty::Union(&[TY_STRING, TY_DICT])),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "audit_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "repair_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "summary_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "verify_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "merge_captain_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "review_captain_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "oncall_captain_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "release_captain_agent",
        &[
            Param::new("prompt", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_name",
        &[Param::new("agent", TY_DICT)],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::simple(
        "agent_typed_output_checkpoint",
        &[
            Param::new("name", TY_STRING),
            Param::new("prompt", TY_STRING),
            Param::new("schema", TY_SCHEMA_VALUE),
            Param::optional("options", TY_DICT_OR_NIL),
            Param::optional("validator", Ty::Union(&[TY_CLOSURE, TY_NIL])),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agentic_user",
        &[
            Param::new("task_or_config", TY_ANY),
            Param::optional("behavior", TY_ANY),
            Param::optional("tools", TY_ANY),
            Param::optional("model", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("close_agent", &[Param::new("handle", TY_ANY)], TY_DICT),
    BuiltinSignature::simple("conversation", &[], TY_LIST),
    BuiltinSignature::simple(
        "fixture_user",
        &[
            Param::new("script", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("list_agents", &[], TY_LIST),
    BuiltinSignature::simple(
        "llm_budget",
        &[Param::new("max_cost", Ty::Union(&[TY_FLOAT, TY_INT]))],
        TY_NIL,
    ),
    BuiltinSignature::simple("llm_budget_remaining", &[], TY_FLOAT_OR_NIL),
    BuiltinSignature::simple(
        "tiktoken_count_tokens",
        &[
            Param::new("text", TY_STRING),
            Param::new("model", TY_STRING),
        ],
        TY_INT,
    ),
    BuiltinSignature::simple(
        "tiktoken_tokenizer_info",
        &[Param::new("model", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "llm_compare_costs",
        &[
            Param::new("candidates", TY_LIST),
            Param::new("opts", TY_DICT),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "llm_cost",
        &[
            Param::new("model", TY_STRING),
            Param::new("input_tokens", TY_INT),
            Param::new("output_tokens", TY_INT),
        ],
        TY_FLOAT,
    ),
    BuiltinSignature::simple(
        "llm_format_usd",
        &[
            Param::new("amount", Ty::Union(&[TY_FLOAT, TY_INT])),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "llm_pricing",
        &[
            Param::new("model_or_dict", Ty::Union(&[TY_STRING, TY_DICT])),
            Param::optional("model", TY_STRING),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("llm_session_cost", &[], TY_DICT),
    BuiltinSignature::simple(
        "runtime_introspection_tools",
        &[
            Param::optional("registry", TY_DICT_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "parse_resume_conditions",
        &[Param::optional("conditions", RESUME_CONDITIONS_OR_NIL)],
        RESUME_CONDITIONS_OR_NIL,
    ),
    BuiltinSignature::simple(
        "resume_agent",
        &[
            Param::new("worker_or_snapshot", TY_ANY),
            Param::optional("resume_input", TY_ANY),
            Param::optional("continue_transcript", TY_BOOL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "send_input",
        &[Param::new("handle", TY_ANY), Param::new("task", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "suspend_agent",
        &[
            Param::new("worker", TY_ANY),
            Param::optional("reason", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "scripted_user",
        &[
            Param::new("script", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "simulated_user_post_turn",
        &[
            Param::new("answerer", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_CLOSURE,
    ),
    BuiltinSignature::simple(
        "simulated_user_read_tools",
        &[
            Param::optional("registry", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "simulated_user_respond",
        &[
            Param::new("answerer", TY_ANY),
            Param::optional("payload", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "simulated_user_status",
        &[Param::new("answerer", TY_ANY)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "spawn_agent",
        &[Param::new("config", AGENT_SPAWN_CONFIG)],
        WORKER_SUMMARY,
    ),
    BuiltinSignature::simple(
        "sub_agent_run",
        &[
            Param::new("task", TY_STRING),
            Param::optional("options", SUB_AGENT_OPTIONS),
        ],
        Ty::Union(&[SUB_AGENT_RESULT, WORKER_SUMMARY]),
    ),
    BuiltinSignature::simple(
        "sub_agent_request",
        &[
            Param::new("task", TY_STRING),
            Param::optional("options", SUB_AGENT_OPTIONS),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript",
        &[Param::optional("metadata", TY_DICT)],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript.clear_reminders",
        &[
            Param::new("transcript", TRANSCRIPT),
            Param::new("selector", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_abandon",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT)],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_add_asset",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::new("asset", TY_DICT),
        ],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_archive",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT)],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_assets",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "transcript_compact",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::optional("options", TY_DICT),
        ],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_project",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::optional("options", TY_ANY),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_events",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "transcript_events_by_kind",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL),
            Param::new("kind", TY_STRING),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "transcript_export",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "transcript_fork",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::optional("options", TY_DICT),
        ],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_from_messages",
        &[Param::new(
            "messages_or_transcript",
            TY_MESSAGES_OR_TRANSCRIPT,
        )],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_id",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "transcript_import",
        &[Param::new("text", TY_STRING)],
        TY_ANY,
    ),
    BuiltinSignature::simple(
        "transcript.inject_reminder",
        &[
            Param::new("transcript", TRANSCRIPT),
            Param::new("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_messages",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "transcript_render_full",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "transcript_render_visible",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "transcript_reminder_event",
        &[Param::new("reminder", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_suspension_event",
        &[Param::new("suspension", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_resumption_event",
        &[Param::new("resumption", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_drain_decision_event",
        &[Param::new("drain", TY_DICT)],
        TY_DICT,
    ),
    // Lifecycle replay determinism (#1861) — record / verify / replay
    // suspension, resumption,
    BuiltinSignature::simple(
        "transcript_reset",
        &[Param::optional("opts", TY_DICT)],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_resume",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT)],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_stats",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "transcript_summarize",
        &[
            Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT),
            Param::optional("options", TY_DICT),
        ],
        TRANSCRIPT,
    ),
    BuiltinSignature::simple(
        "transcript_summary",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::simple(
        "wait_agent",
        &[Param::new("handle", TY_ANY)],
        TY_DICT_OR_LIST,
    ),
    BuiltinSignature::simple(
        "user_tools",
        &[
            Param::new("answerer", TY_ANY),
            Param::optional("registry", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "worker_trigger",
        &[Param::new("handle", TY_ANY), Param::new("payload", TY_ANY)],
        TY_DICT,
    ),
];
