//! Core stdlib builtin signatures that are not in the higher-level namespaces.

use super::shapes::{
    DAEMON_CONFIG, DAEMON_SUMMARY, IO_RESULT_ENVELOPE, PAGER_OPTIONS, READ_LINE_OPTIONS,
    SIGNAL_HANDLER_OPTIONS,
};
use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL,
    TY_DURATION, TY_FLOAT, TY_INT, TY_LIST, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

// `string | dict` — used by waitpoint and daemon handles which accept either
// a raw id string or a dict containing an `id` field.
const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);
// `int | float | duration` — used by sleep / cancel_graceful timeouts that
// accept either a millisecond int or a duration value.
const TY_DURATION_OR_INT: Ty = Ty::Union(&[TY_DURATION, TY_INT]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature::simple(
        "__cache_clear",
        &[Param::optional("options", TY_DICT_OR_NIL)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__cache_get",
        &[
            Param::new("key", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__cache_put",
        &[
            Param::new("key", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__cache_stats",
        &[Param::optional("options", TY_DICT_OR_NIL)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__cache_stats_reset",
        &[Param::optional("options", TY_DICT_OR_NIL)],
        TY_NIL,
    ),
    // `__deep_merge`, `__dict_filter_nil`, `__dict_from_pairs`,
    // `__dict_merge`, `__list_unique`, `__dict_omit`, `__dict_pick`,
    // `__dict_pick_keys` migrated to `#[harn_builtin]` in
    // `harn-vm/src/stdlib/collections.rs`.
    BuiltinSignature::simple(
        "__from_xml",
        &[
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__to_xml",
        &[
            Param::new("value", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "__files_upload",
        &[
            Param::new("path", TY_STRING),
            Param::new("provider", TY_STRING),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "__io_read_line",
        &[Param::optional(
            "options",
            Ty::Union(&[READ_LINE_OPTIONS, TY_NIL]),
        )],
        IO_RESULT_ENVELOPE,
    ),
    BuiltinSignature::simple(
        "__io_write_stderr",
        &[Param::new("message", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__io_write_stdout",
        &[Param::new("message", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::variadic("__io_print", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("__io_println", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::simple("__io_eprint", &[Param::new("message", TY_ANY)], TY_NIL),
    BuiltinSignature::simple("__io_eprintln", &[Param::new("message", TY_ANY)], TY_NIL),
    BuiltinSignature::simple(
        "__oauth_storage_cloud_handle",
        &[Param::new("scope", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__oauth_storage_delete",
        &[Param::new("handle", TY_DICT), Param::new("key", TY_STRING)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__oauth_storage_file_handle",
        &[
            Param::new("path", TY_STRING),
            Param::new("encryption_key", Ty::Union(&[TY_STRING, TY_BYTES])),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__oauth_storage_get",
        &[Param::new("handle", TY_DICT), Param::new("key", TY_STRING)],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("__oauth_storage_memory_handle", &[], TY_DICT),
    BuiltinSignature::simple(
        "__oauth_storage_set",
        &[
            Param::new("handle", TY_DICT),
            Param::new("key", TY_STRING),
            Param::new("token_set", TY_DICT),
            Param::optional("ttl_seconds", Ty::Union(&[TY_INT, TY_DURATION, TY_NIL])),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__oauth_dynreg_build_authorization_server_metadata",
        &[
            Param::new("provider", TY_DICT),
            Param::optional("overrides", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__oauth_dynreg_build_client_metadata",
        &[Param::new("metadata", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__oauth_dynreg_get",
        &[
            Param::new("handle", TY_DICT),
            Param::new("client_id", TY_STRING),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "__oauth_dynreg_list",
        &[Param::new("handle", TY_DICT)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "__oauth_dynreg_register",
        &[
            Param::new("handle", TY_DICT),
            Param::new("metadata", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("__oauth_dynreg_store_handle", &[], TY_DICT),
    BuiltinSignature::simple(
        "__oauth_dynreg_validate_metadata",
        &[Param::new("metadata", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple("__token_redaction_clear_custom_patterns", &[], TY_NIL),
    BuiltinSignature::simple("__token_redaction_custom_patterns", &[], TY_LIST),
    BuiltinSignature::simple("__token_redaction_default_patterns", &[], TY_LIST),
    BuiltinSignature::simple("__token_redaction_drain_audit", &[], TY_LIST),
    BuiltinSignature::simple(
        "__token_redaction_redact",
        &[Param::new("text", TY_STRING)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "__token_redaction_register_pattern",
        &[
            Param::new("name", TY_STRING),
            Param::new("regex", TY_STRING),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__ansi_enabled",
        &[Param::optional("stream", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "__llm_cache_key",
        &[
            Param::new("prompt", TY_ANY),
            Param::optional("system", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple("__select_list", &[Param::new("channels", TY_LIST)], TY_DICT),
    BuiltinSignature::simple(
        "__select_timeout",
        &[
            Param::new("channels", TY_LIST),
            Param::new("timeout", TY_DURATION_OR_INT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("__select_try", &[Param::new("channels", TY_LIST)], TY_DICT),
    BuiltinSignature::simple("__signal_interrupted", &[], TY_BOOL),
    BuiltinSignature::simple(
        "__signal_off_interrupt",
        &[Param::new("handle", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "__signal_on_interrupt",
        &[
            Param::new("handler", TY_CLOSURE),
            Param::optional("options", Ty::Union(&[SIGNAL_HANDLER_OPTIONS, TY_NIL])),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__signal_raise",
        &[Param::optional("signal", TY_STRING)],
        TY_NIL,
    ),
    BuiltinSignature::simple("__tui_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "__tui_page",
        &[Param::new("options", PAGER_OPTIONS)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "__tui_terminal_width",
        &[Param::optional("default_width", TY_INT)],
        TY_INT,
    ),
    BuiltinSignature::simple(
        "append_file",
        &[
            Param::new("path", TY_STRING),
            Param::new("content", TY_STRING),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "assert",
        &[
            Param::new("condition", TY_ANY),
            Param::optional("message", TY_STRING),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "assert_eq",
        &[
            Param::new("actual", TY_ANY),
            Param::new("expected", TY_ANY),
            Param::optional("message", TY_STRING),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "assert_ne",
        &[
            Param::new("a", TY_ANY),
            Param::new("b", TY_ANY),
            Param::optional("message", TY_STRING),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "atomic",
        &[Param::optional(
            "initial",
            Ty::Union(&[TY_INT, TY_FLOAT, TY_BOOL]),
        )],
        Ty::Named("atomic"),
    ),
    BuiltinSignature::simple(
        "atomic_add",
        &[
            Param::new("handle", Ty::Named("atomic")),
            Param::new("delta", TY_INT),
        ],
        TY_INT,
    ),
    BuiltinSignature::simple(
        "atomic_cas",
        &[
            Param::new("handle", Ty::Named("atomic")),
            Param::new("expected", TY_INT),
            Param::new("new_value", TY_INT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "atomic_get",
        &[Param::new("handle", Ty::Named("atomic"))],
        TY_INT,
    ),
    BuiltinSignature::simple(
        "atomic_set",
        &[
            Param::new("handle", Ty::Named("atomic")),
            Param::new("value", TY_INT),
        ],
        TY_INT,
    ),
    BuiltinSignature::simple("await", &[Param::new("handle", Ty::Named("task"))], TY_ANY),
    BuiltinSignature::simple("bold", &[Param::new("text", TY_STRING)], TY_STRING),
    BuiltinSignature::simple("cancel", &[Param::new("handle", Ty::Named("task"))], TY_NIL),
    BuiltinSignature::simple(
        "cancel_graceful",
        &[
            Param::new("handle", Ty::Named("task")),
            Param::optional("timeout_ms", TY_DURATION_OR_INT),
        ],
        TY_ANY,
    ),
    BuiltinSignature::simple("capture_stderr_start", &[], TY_NIL),
    BuiltinSignature::simple("capture_stderr_take", &[], TY_STRING),
    BuiltinSignature::simple(
        "channel",
        &[
            Param::optional("name", TY_STRING),
            Param::optional("capacity", TY_INT),
        ],
        Ty::Named("channel"),
    ),
    BuiltinSignature::simple(
        "channel_select",
        &[
            Param::new("channels", TY_LIST),
            Param::optional("timeout_ms", TY_DURATION_OR_INT),
        ],
        Ty::Union(&[TY_DICT, TY_NIL]),
    ),
    BuiltinSignature::simple(
        "chunk",
        &[Param::new("items", TY_LIST), Param::new("size", TY_INT)],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "circuit_breaker",
        &[
            Param::new("name", TY_STRING),
            Param::optional("threshold", TY_INT),
            Param::optional("reset_ms", TY_INT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple("circuit_check", &[Param::new("name", TY_STRING)], TY_STRING),
    BuiltinSignature::simple(
        "circuit_record_failure",
        &[Param::new("name", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "circuit_record_success",
        &[Param::new("name", TY_STRING)],
        TY_NIL,
    ),
    BuiltinSignature::simple("circuit_reset", &[Param::new("name", TY_STRING)], TY_NIL),
    BuiltinSignature::simple("clear_path_scope_guard", &[], TY_NIL),
    BuiltinSignature::simple("clear_tool_hooks", &[], TY_NIL),
    BuiltinSignature::simple("clear_persona_hooks", &[], TY_NIL),
    BuiltinSignature::simple("clear_session_hooks", &[], TY_NIL),
    BuiltinSignature::simple("clear_reminder_providers", &[], TY_NIL),
    BuiltinSignature::simple(
        "close_channel",
        &[Param::new("channel", Ty::Named("channel"))],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "channel_is_closed",
        &[Param::new("channel", Ty::Named("channel"))],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "color",
        &[
            Param::new("text", TY_STRING),
            Param::new("color", TY_STRING),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "composition_binding_manifest",
        &[
            Param::new("tools", Ty::Union(&[TY_LIST, TY_DICT])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "composition_execute",
        &[
            Param::new("snippet", TY_STRING),
            Param::new("manifest", TY_DICT),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "composition_crystallization_trace",
        &[
            Param::new("report", TY_DICT),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "composition_search_examples",
        &[
            Param::optional("query", TY_STRING),
            Param::optional("limit", TY_INT),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "composition_typescript_declarations",
        &[Param::new("manifest", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "copy_file",
        &[Param::new("src", TY_STRING), Param::new("dst", TY_STRING)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "daemon_resume",
        &[Param::new("path", TY_STRING)],
        DAEMON_SUMMARY,
    ),
    BuiltinSignature::simple(
        "daemon_snapshot",
        &[Param::new("handle", TY_STRING_OR_DICT)],
        DAEMON_SUMMARY,
    ),
    BuiltinSignature::simple(
        "daemon_spawn",
        &[Param::new("config", DAEMON_CONFIG)],
        DAEMON_SUMMARY,
    ),
    BuiltinSignature::simple(
        "daemon_stop",
        &[Param::new("handle", TY_STRING_OR_DICT)],
        DAEMON_SUMMARY,
    ),
    BuiltinSignature::simple(
        "daemon_trigger",
        &[
            Param::new("handle", TY_STRING_OR_DICT),
            Param::new("event", TY_ANY),
        ],
        DAEMON_SUMMARY,
    ),
    BuiltinSignature::simple(
        "dedup_by",
        &[
            Param::new("items", TY_LIST),
            Param::new("key_fn", TY_CLOSURE),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "count_by",
        // Generic shape: <T, K> List<T> -> ((T) -> K) -> Dict<K, int>.
        // The signature surface uses TY_LIST + TY_CLOSURE because Harn's
        // builtin signature DSL is untyped at the element layer; the
        // typechecker layers parametric inference on top via VmValue
        // tagging. Return type is TY_DICT — the values are integers keyed
        // by stringified callback output.
        &[
            Param::new("items", TY_LIST),
            Param::new("key_fn", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("delete_file", &[Param::new("path", TY_STRING)], TY_NIL),
    BuiltinSignature::simple("dim", &[Param::new("text", TY_STRING)], TY_STRING),
    BuiltinSignature::simple(
        "drop_while",
        // <T> List<T> -> ((T) -> bool) -> List<T>. Skips the leading run
        // of items for which the predicate returns true, keeps the rest.
        &[
            Param::new("items", TY_LIST),
            Param::new("predicate", TY_CLOSURE),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple("e", &[], TY_FLOAT),
    BuiltinSignature::simple("error_category", &[Param::new("error", TY_ANY)], TY_STRING),
    BuiltinSignature::simple(
        "estimate_tokens",
        &[Param::new("messages", TY_LIST)],
        TY_INT,
    ),
    BuiltinSignature::simple(
        "emit_channel",
        &[
            Param::new("name", TY_STRING),
            Param::new("payload", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "channel_events",
        &[
            Param::new("name", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple("flush_trigger_aggregations", &[], TY_NIL),
    // CH-11 (#1911): channel guardrails middleware. Register/list/unregister
    // run synchronously; the actual scan executes inside `emit_channel(...)`.
    BuiltinSignature::simple(
        "channel_guardrail_register",
        &[Param::new("config", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "channel_guardrail_unregister",
        &[Param::new("id", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple("channel_guardrail_list", &[], TY_LIST),
    BuiltinSignature::simple("channel_guardrail_clear", &[], TY_NIL),
    BuiltinSignature::simple("file_exists", &[Param::new("path", TY_STRING)], TY_BOOL),
    BuiltinSignature::simple(
        "flat_map",
        &[
            Param::new("items", TY_LIST),
            Param::new("callback", TY_CLOSURE),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "glob",
        &[
            Param::new("pattern", TY_STRING),
            Param::optional("base_or_options", Ty::Union(&[TY_STRING, TY_DICT, TY_NIL])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple(
        "group_by",
        &[
            Param::new("items", TY_LIST),
            Param::new("key_fn", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("is_cancelled", &[], TY_BOOL),
    BuiltinSignature::simple("is_rate_limited", &[Param::new("error", TY_ANY)], TY_BOOL),
    BuiltinSignature::simple("is_stderr_tty", &[], TY_BOOL),
    BuiltinSignature::simple("is_stdin_tty", &[], TY_BOOL),
    BuiltinSignature::simple("is_stdout_tty", &[], TY_BOOL),
    BuiltinSignature::simple("is_timeout", &[Param::new("error", TY_ANY)], TY_BOOL),
    BuiltinSignature::simple("list_dir", &[Param::optional("path", TY_STRING)], TY_LIST),
    BuiltinSignature::simple("log", &[Param::new("message", TY_ANY)], TY_NIL),
    BuiltinSignature::simple(
        "log_debug",
        &[
            Param::new("message", TY_ANY),
            Param::optional("fields", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "log_error",
        &[
            Param::new("message", TY_ANY),
            Param::optional("fields", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "log_info",
        &[
            Param::new("message", TY_ANY),
            Param::optional("fields", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "log_json",
        &[
            Param::new("key", TY_STRING),
            Param::optional("value", TY_ANY),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple("log_set_level", &[Param::new("level", TY_STRING)], TY_NIL),
    BuiltinSignature::simple(
        "log_warn",
        &[
            Param::new("message", TY_ANY),
            Param::optional("fields", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "take_while",
        // <T> List<T> -> ((T) -> bool) -> List<T>. Returns the longest
        // prefix of items for which the predicate returns true.
        &[
            Param::new("items", TY_LIST),
            Param::new("predicate", TY_CLOSURE),
        ],
        TY_LIST,
    ),
    BuiltinSignature::variadic("metrics_inc", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("microcompact", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("mkdir", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::simple(
        "mkdtemp",
        &[Param::optional("prefix", TY_STRING)],
        TY_STRING,
    ),
    BuiltinSignature::variadic(
        "monitor_wait_for_native",
        &[Param::new("args", TY_ANY)],
        TY_ANY,
    ),
    BuiltinSignature::variadic("mailbox_close", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("mailbox_lookup", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("mailbox_metrics", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("mailbox_open", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("mailbox_receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("mailbox_send", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("mailbox_try_receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("path_join", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("partition", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("pg_close", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("pg_connect", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("pg_execute", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("pg_mock_calls", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("pg_mock_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("pg_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("pg_query", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("pg_query_one", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("pg_transaction", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("pi", &[Param::new("args", TY_ANY)], TY_FLOAT),
    BuiltinSignature::simple("pipeline_lifecycle_audit_log_snapshot", &[], TY_LIST),
    BuiltinSignature::simple("pipeline_lifecycle_audit_log_take", &[], TY_LIST),
    BuiltinSignature::simple(
        "pipeline_on_finish",
        &[Param::new("callback", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::variadic("progress", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("read_file", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("read_file_bytes", &[Param::new("args", TY_ANY)], TY_BYTES),
    BuiltinSignature::variadic("read_file_result", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("request_approval", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("register_tool_hook", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::simple(
        "register_persona_hook",
        &[
            Param::new("persona_pattern", TY_STRING),
            Param::new("event", TY_STRING),
            Param::new("handler", TY_ANY),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "register_step_hook",
        &[
            Param::new("persona_pattern", TY_STRING),
            Param::new("step_name", TY_STRING),
            Param::new("event", TY_STRING),
            Param::new("handler", TY_ANY),
        ],
        TY_NIL,
    ),
    BuiltinSignature::variadic(
        "register_session_hook",
        &[Param::new("args", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "register_checkpoint_hook",
        &[Param::new("kinds", TY_ANY), Param::new("handler", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "register_path_scope_guard",
        &[Param::optional("opts", TY_DICT_OR_NIL)],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "register_reminder_provider",
        &[Param::new("config", TY_DICT)],
        TY_NIL,
    ),
    BuiltinSignature::variadic("notify_file_edited", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("runtime_context", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic(
        "runtime_context_clear",
        &[Param::new("args", TY_ANY)],
        TY_ANY,
    ),
    BuiltinSignature::variadic("runtime_context_get", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("runtime_context_set", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic(
        "runtime_context_values",
        &[Param::new("args", TY_ANY)],
        TY_DICT,
    ),
    BuiltinSignature::variadic("select", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("send", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_cas", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("shared_cell", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("shared_get", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_map", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("shared_map_cas", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("shared_map_delete", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_map_entries", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("shared_map_get", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_map_set", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic(
        "shared_map_snapshot",
        &[Param::new("args", TY_ANY)],
        TY_DICT,
    ),
    BuiltinSignature::variadic("shared_metrics", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_scope_id", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("shared_set", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("shared_snapshot", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("secret_get", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("secret_scan", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("self_review", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("sleep", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("spawn", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stat", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("stream", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("stream.broadcast", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("stream.collect", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("stream.debounce", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.filter", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.first", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.fold", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.interleave", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.map", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.merge", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.race", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.scan", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.take", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.take_until", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.tap", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.throttle", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("stream.zip", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("supervisor_events", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("supervisor_metrics", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("supervisor_start", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("supervisor_state", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("supervisor_stop", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("sync_gate_acquire", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("sync_metrics", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("sync_mutex_acquire", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("sync_release", &[Param::new("args", TY_ANY)], TY_BOOL),
    BuiltinSignature::variadic("sync_rwlock_acquire", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic(
        "sync_semaphore_acquire",
        &[Param::new("args", TY_ANY)],
        TY_ANY,
    ),
    BuiltinSignature::variadic("task_current", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("temp_dir", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("throw_error", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("timer_end", &[Param::new("args", TY_ANY)], TY_INT),
    BuiltinSignature::variadic("timer_start", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("try_receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("uuid", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("uuid_nil", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic(
        "uuid_parse",
        &[Param::new("args", TY_ANY)],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::variadic("uuid_v5", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("uuid_v7", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("vision_ocr", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_cancel", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_complete", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_create", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_wait", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("window", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("with_rate_limit", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("write_file", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("write_file_bytes", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::simple("yield_now", &[], TY_NIL),
    BuiltinSignature::variadic(
        "read_stdin",
        &[Param::new("args", TY_ANY)],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::variadic("mock_stdin", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("unmock_stdin", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("mock_tty", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("unmock_tty", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("set_color_mode", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::simple("testbench_is_active", &[], TY_BOOL),
    BuiltinSignature::simple("testbench_fs_diff", &[], TY_LIST),
    BuiltinSignature::simple("testbench_clock_leaks", &[], TY_LIST),
    BuiltinSignature::variadic("walk_dir", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("move_file", &[Param::new("args", TY_ANY)], TY_NIL),
    BuiltinSignature::variadic("read_lines", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("url_parse", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("url_build", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("query_parse", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::variadic("query_stringify", &[Param::new("args", TY_ANY)], TY_STRING),
    // Clone / merge / dedupe helpers — see crates/harn-vm/src/stdlib/collections.rs.
    // `clone`, `deep_clone`, `deep_merge`, `unique`,
    // XML conversion — see crates/harn-vm/src/stdlib/xml.rs.
    BuiltinSignature::simple(
        "to_xml",
        &[
            Param::new("value", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "from_xml",
        &[
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "jsonrpc_batch",
        &[
            Param::new("url", TY_STRING),
            Param::new("calls", TY_LIST),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // Generic JSON-RPC client — see crates/harn-vm/src/stdlib/jsonrpc.rs.
    BuiltinSignature::simple(
        "jsonrpc_call",
        &[
            Param::new("url", TY_STRING),
            Param::new("method", TY_STRING),
            Param::optional("params", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_ANY,
    ),
];
