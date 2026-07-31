//! Agent / orchestration / sub-agent builtin signatures.

use super::shapes::{SESSION_SNAPSHOT, TRANSCRIPT};
use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_CLOSURE, TY_DECIMAL, TY_DICT, TY_DICT_OR_NIL, TY_FLOAT,
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
        "agent_preset",
        &[
            Param::new("kind", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "agent_preset_register",
        &[Param::new("kind", TY_STRING), Param::new("spec", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple("agent_preset_kinds", &[], TY_LIST),
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
    BuiltinSignature::simple("conversation", &[], TY_LIST),
    BuiltinSignature::simple(
        "fixture_user",
        &[
            Param::new("script", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
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
        // Exact money: `llm_cost` returns a `decimal`, not a binary `float`.
        TY_DECIMAL,
    ),
    BuiltinSignature::simple(
        "llm_format_usd",
        &[
            Param::new("amount", Ty::Union(&[TY_DECIMAL, TY_FLOAT, TY_INT])),
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
        "transcript_summary",
        &[Param::new("transcript", TY_MESSAGES_OR_TRANSCRIPT_OR_NIL)],
        TY_STRING_OR_NIL,
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
];
