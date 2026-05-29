//! Core stdlib builtin signatures that are not in the higher-level namespaces.

use super::shapes::SIGNAL_HANDLER_OPTIONS;
use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL,
    TY_DURATION, TY_FLOAT, TY_INT, TY_LIST, TY_NEVER, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

// `int | float | duration` — used by sleep / cancel_graceful timeouts that
// accept either a millisecond int or a duration value.
const TY_DURATION_OR_INT: Ty = Ty::Union(&[TY_DURATION, TY_INT]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // `__deep_merge`, `__dict_filter_nil`, `__dict_from_pairs`,
    // `__dict_merge`, `__list_unique`, `__dict_omit`, `__dict_pick`,
    BuiltinSignature::simple(
        "__files_upload",
        &[
            Param::new("path", TY_STRING),
            Param::new("provider", TY_STRING),
        ],
        TY_STRING,
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
    BuiltinSignature::simple("cancel", &[Param::new("handle", Ty::Named("task"))], TY_NIL),
    BuiltinSignature::simple(
        "cancel_graceful",
        &[
            Param::new("handle", Ty::Named("task")),
            Param::optional("timeout_ms", TY_DURATION_OR_INT),
        ],
        TY_ANY,
    ),
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
    BuiltinSignature::simple(
        "flat_map",
        &[
            Param::new("items", TY_LIST),
            Param::new("callback", TY_CLOSURE),
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
    BuiltinSignature::variadic("receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("request_approval", &[Param::new("args", TY_ANY)], TY_DICT),
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
    BuiltinSignature::variadic("timer_end", &[Param::new("args", TY_ANY)], TY_INT),
    BuiltinSignature::variadic("timer_start", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("try_receive", &[Param::new("args", TY_ANY)], TY_ANY),
    BuiltinSignature::variadic("vision_ocr", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_cancel", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_complete", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_create", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("waitpoint_wait", &[Param::new("args", TY_ANY)], TY_DICT),
    BuiltinSignature::variadic("window", &[Param::new("args", TY_ANY)], TY_LIST),
    BuiltinSignature::simple("yield_now", &[], TY_NIL),
    // Clone / merge / dedupe helpers — see crates/harn-vm/src/stdlib/collections.rs.
    // `clone`, `deep_clone`, `deep_merge`, `unique`,
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
    // Parser-only shadows for migrated builtins that harn-parser unit
    // tests reference by name. The runtime-installed slice from
    // `#[harn_builtin]` shadows these at driver startup; without these
    // entries, pure-parser tests that never call
    // `install_builtin_signatures` (lookup_hits_and_misses,
    // return_type_*_variant, test_builtin_arg_type_mismatch,
    // test_builtin_arity_warning, test_builtin_return_type_inference,
    // never_tail_expression_satisfies_return_type,
    // test_cross_module_builtin_not_flagged,
    // test_harness_fs_method_arg_type_mismatch, …) can't find these
    // names.
    BuiltinSignature::variadic("snake_to_camel", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::simple("env", &[Param::new("name", TY_STRING)], TY_STRING_OR_NIL),
    BuiltinSignature::simple("file_exists", &[Param::new("path", TY_STRING)], TY_BOOL),
    BuiltinSignature::simple("json_parse", &[Param::new("text", TY_STRING)], TY_ANY),
    BuiltinSignature::simple("log", &[Param::new("message", TY_ANY)], TY_NIL),
    BuiltinSignature::simple(
        "mkdtemp",
        &[Param::optional("prefix", TY_STRING)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "len",
        &[Param::new(
            "value",
            Ty::Union(&[
                TY_STRING,
                TY_BYTES,
                TY_LIST,
                TY_DICT,
                Ty::Named("set"),
                Ty::Named("range"),
                TY_NIL,
            ]),
        )],
        TY_INT,
    ),
    BuiltinSignature::variadic("to_int", &[Param::new("args", TY_ANY)], TY_INT),
    BuiltinSignature::variadic("type_of", &[Param::new("args", TY_ANY)], TY_STRING),
    BuiltinSignature::variadic("unreachable", &[Param::new("args", TY_ANY)], TY_NEVER),
    // Harness method targets — typechecker resolves `harness.crypto.sha256`
    // / `harness.term.width` / `harness.term.height` via
    // `harness_methods::harness_*_ambient` to these builtin names. Pure-
    // parser tests need them in the registry to type-check the namespace
    // call sites.
    BuiltinSignature::simple(
        "sha256_hex",
        &[Param::new("input", Ty::Union(&[TY_STRING, TY_BYTES]))],
        TY_STRING,
    ),
    BuiltinSignature::simple("term_width", &[], TY_INT),
    BuiltinSignature::simple("term_height", &[], TY_INT),
];
