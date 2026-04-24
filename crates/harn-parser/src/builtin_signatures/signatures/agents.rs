//! Agent, session, LLM, and transcript builtin signatures.

use super::{BuiltinReturn, BuiltinSig, UNION_DICT_NIL, UNION_STRING_NIL};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "add_assistant",
        return_type: None,
    },
    BuiltinSig {
        name: "add_message",
        return_type: None,
    },
    BuiltinSig {
        name: "add_system",
        return_type: None,
    },
    BuiltinSig {
        name: "add_tool_result",
        return_type: None,
    },
    BuiltinSig {
        name: "add_user",
        return_type: None,
    },
    BuiltinSig {
        name: "agent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_config",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_inject_feedback",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "agent_loop",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "agent_name",
        return_type: None,
    },
    BuiltinSig {
        name: "agent_session_ancestry",
        return_type: Some(BuiltinReturn::Union(UNION_DICT_NIL)),
    },
    BuiltinSig {
        name: "agent_session_close",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "agent_session_compact",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "agent_session_current_id",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "agent_session_exists",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "agent_session_fork",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "agent_session_fork_at",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "agent_session_inject",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "agent_session_length",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "agent_session_open",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "agent_session_reset",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "agent_session_snapshot",
        return_type: Some(BuiltinReturn::Union(UNION_DICT_NIL)),
    },
    BuiltinSig {
        name: "agent_session_trim",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "agent_subscribe",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "agent_trace",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "agent_trace_summary",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "close_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "conversation",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "list_agents",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_budget",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_budget_remaining",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_call",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_call_safe",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_call_structured",
        // Return type is schema-dependent (Schema<T> → T) and resolved
        // by `lookup_generic_builtin_sig`. Fall-through `None` keeps the
        // parser from assuming a concrete return type when the schema
        // argument isn't a typed alias.
        return_type: None,
    },
    BuiltinSig {
        name: "llm_call_structured_safe",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_completion",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_config",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_cost",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "llm_healthcheck",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_infer_provider",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "llm_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_mock",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_mock_calls",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_mock_clear",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "llm_model_tier",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "llm_pick_model",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_providers",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "llm_rate_limit",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_resolve_model",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_session_cost",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "llm_stream",
        return_type: None,
    },
    BuiltinSig {
        name: "llm_usage",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "resume_agent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "send_input",
        return_type: None,
    },
    BuiltinSig {
        name: "spawn_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "sub_agent_run",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_abandon",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_add_asset",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_archive",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_assets",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_auto_compact",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_compact",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_events",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_events_by_kind",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_export",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_fork",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_from_messages",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_id",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_import",
        return_type: None,
    },
    BuiltinSig {
        name: "transcript_messages",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "transcript_render_full",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_render_visible",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "transcript_reset",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_resume",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_stats",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_summarize",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "transcript_summary",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "wait_agent",
        return_type: None,
    },
    BuiltinSig {
        name: "worker_trigger",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
];
