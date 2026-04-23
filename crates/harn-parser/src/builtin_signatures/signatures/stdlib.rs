//! Core stdlib builtin signatures that are not in the higher-level namespaces.

use super::{BuiltinReturn, BuiltinSig, UNION_STRING_NIL};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "__waitpoint_cancel",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "__waitpoint_complete",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "__waitpoint_create",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "__waitpoint_wait",
        return_type: None,
    },
    BuiltinSig {
        name: "abs",
        return_type: None,
    },
    BuiltinSig {
        name: "acos",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "addr_of",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "append_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "arch",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "asin",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "ask_user",
        return_type: None,
    },
    BuiltinSig {
        name: "assert",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "assert_eq",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "assert_ne",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "asset_root",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "atan",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "atan2",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "atomic",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_add",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_cas",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "atomic_get",
        return_type: None,
    },
    BuiltinSig {
        name: "atomic_set",
        return_type: None,
    },
    BuiltinSig {
        name: "await",
        return_type: None,
    },
    BuiltinSig {
        name: "base32_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "base32_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "base64_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "base64_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "base64url_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "base64url_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "basename",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bytes_concat",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "bytes_eq",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "bytes_from_base64",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "bytes_from_hex",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "bytes_from_string",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "bytes_len",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "bytes_slice",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "bytes_to_base64",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bytes_to_hex",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bytes_to_string",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bytes_to_string_lossy",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "bold",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_kebab",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_pascal",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "camel_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "cancel",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "cancel_graceful",
        return_type: None,
    },
    BuiltinSig {
        name: "ceil",
        return_type: None,
    },
    BuiltinSig {
        name: "channel",
        return_type: None,
    },
    BuiltinSig {
        name: "circuit_breaker",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "circuit_check",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "circuit_record_failure",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "circuit_record_success",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "circuit_reset",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "clear_tool_hooks",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "close_channel",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "color",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "constant_time_eq",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "contains",
        return_type: None,
    },
    BuiltinSig {
        name: "copy_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "cos",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "cwd",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "daemon_resume",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "daemon_snapshot",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "daemon_spawn",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "daemon_stop",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "daemon_trigger",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "handler_context",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trust_graph_policy_for",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trust_graph_query",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trust_graph_record",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "trust_graph_verify_chain",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trust_query",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trust_record",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trigger_fire",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trigger_inspect_action_graph",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trigger_inspect_dlq",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trigger_inspect_lifecycle",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trigger_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trigger_register",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trigger_replay",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trigger_test_harness",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "date_format",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "date_iso",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "date_now",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "date_parse",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "delete_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "dim",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "dirname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "dual_control",
        return_type: None,
    },
    BuiltinSig {
        name: "e",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "elapsed",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "escalate_to",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "enable_tracing",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "ends_with",
        return_type: None,
    },
    BuiltinSig {
        name: "entries",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "env",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "env_or",
        return_type: None,
    },
    BuiltinSig {
        name: "error_category",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "estimate_tokens",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "event_log_emit",
        return_type: None,
    },
    BuiltinSig {
        name: "execution_root",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "exit",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "exp",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "extname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "file_exists",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "floor",
        return_type: None,
    },
    BuiltinSig {
        name: "format",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hash_value",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "hex_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "hex_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hmac_sha256",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hmac_sha256_base64",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "home_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hostname",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "hitl_pending",
        return_type: None,
    },
    BuiltinSig {
        name: "is_cancelled",
        return_type: None,
    },
    BuiltinSig {
        name: "is_err",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_infinite",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_nan",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_ok",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_rate_limited",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_same",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "is_timeout",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "iter",
        return_type: Some(BuiltinReturn::Named("iter")),
    },
    BuiltinSig {
        name: "join",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "json_extract",
        return_type: None,
    },
    BuiltinSig {
        name: "json_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "json_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "json_validate",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "jwt_sign",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "kebab_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "kebab_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "keys",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "len",
        return_type: None,
    },
    BuiltinSig {
        name: "list_dir",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "ln",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "log",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log10",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "log2",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "log_debug",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_error",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_info",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_json",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_set_level",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "log_warn",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "lowercase",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "lowercase_first",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "max",
        return_type: None,
    },
    BuiltinSig {
        name: "md5",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "metrics_inc",
        return_type: None,
    },
    BuiltinSig {
        name: "microcompact",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "min",
        return_type: None,
    },
    BuiltinSig {
        name: "mkdir",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "monitor_wait_for_native",
        return_type: None,
    },
    BuiltinSig {
        name: "pair",
        return_type: Some(BuiltinReturn::Named("pair")),
    },
    BuiltinSig {
        name: "pascal_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "pascal_to_snake",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_basename",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_extension",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_is_absolute",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "path_is_relative",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "path_join",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_normalize",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_parent",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_parts",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "path_relative_to",
        return_type: None,
    },
    BuiltinSig {
        name: "path_segments",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "path_stem",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_to_native",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_to_posix",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_with_extension",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_with_stem",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "path_workspace_info",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "path_workspace_normalize",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "pi",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "pid",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "platform",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "pow",
        return_type: None,
    },
    BuiltinSig {
        name: "print",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "println",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "progress",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "prompt_mark_rendered",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "random",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "random_int",
        return_type: None,
    },
    BuiltinSig {
        name: "range",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "read_file",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "read_file_bytes",
        return_type: Some(BuiltinReturn::Named("bytes")),
    },
    BuiltinSig {
        name: "read_file_result",
        return_type: None,
    },
    BuiltinSig {
        name: "receive",
        return_type: None,
    },
    BuiltinSig {
        name: "request_approval",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "regex_captures",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "regex_match",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "regex_replace",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "regex_replace_all",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "register_tool_hook",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "render",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "render_prompt",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "render_string",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "render_with_provenance",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "replace",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "round",
        return_type: None,
    },
    BuiltinSig {
        name: "runtime_context",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "runtime_context_clear",
        return_type: None,
    },
    BuiltinSig {
        name: "runtime_context_get",
        return_type: None,
    },
    BuiltinSig {
        name: "runtime_context_set",
        return_type: None,
    },
    BuiltinSig {
        name: "runtime_context_values",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "select",
        return_type: None,
    },
    BuiltinSig {
        name: "send",
        return_type: None,
    },
    BuiltinSig {
        name: "secret_get",
        return_type: None,
    },
    BuiltinSig {
        name: "secret_scan",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "self_review",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "set",
        return_type: None,
    },
    BuiltinSig {
        name: "set_add",
        return_type: None,
    },
    BuiltinSig {
        name: "set_contains",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_difference",
        return_type: None,
    },
    BuiltinSig {
        name: "set_intersect",
        return_type: None,
    },
    BuiltinSig {
        name: "set_is_disjoint",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_is_subset",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_is_superset",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "set_remove",
        return_type: None,
    },
    BuiltinSig {
        name: "set_symmetric_difference",
        return_type: None,
    },
    BuiltinSig {
        name: "set_union",
        return_type: None,
    },
    BuiltinSig {
        name: "sha224",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha256",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha384",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha512",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sha512_256",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sign",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "sin",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "sleep",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "snake_to_camel",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "snake_to_kebab",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "snake_to_pascal",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "source_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "spawn",
        return_type: None,
    },
    BuiltinSig {
        name: "split",
        return_type: None,
    },
    BuiltinSig {
        name: "sqrt",
        return_type: None,
    },
    BuiltinSig {
        name: "starts_with",
        return_type: None,
    },
    BuiltinSig {
        name: "stat",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "substring",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "sync_gate_acquire",
        return_type: None,
    },
    BuiltinSig {
        name: "sync_metrics",
        return_type: None,
    },
    BuiltinSig {
        name: "sync_mutex_acquire",
        return_type: None,
    },
    BuiltinSig {
        name: "sync_release",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "sync_semaphore_acquire",
        return_type: None,
    },
    BuiltinSig {
        name: "tan",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "task_current",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "temp_dir",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "throw_error",
        return_type: None,
    },
    BuiltinSig {
        name: "timer_end",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "timer_start",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "timestamp",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "title_case",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "to_float",
        return_type: Some(BuiltinReturn::Named("float")),
    },
    BuiltinSig {
        name: "to_int",
        return_type: Some(BuiltinReturn::Named("int")),
    },
    BuiltinSig {
        name: "to_list",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "to_string",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "toml_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "toml_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "trace_end",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "trace_id",
        return_type: Some(BuiltinReturn::Union(UNION_STRING_NIL)),
    },
    BuiltinSig {
        name: "trace_spans",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "trace_start",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "trace_summary",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "trim",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "try_receive",
        return_type: None,
    },
    BuiltinSig {
        name: "type_of",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "unreachable",
        return_type: Some(BuiltinReturn::Never),
    },
    BuiltinSig {
        name: "unwrap",
        return_type: None,
    },
    BuiltinSig {
        name: "unwrap_err",
        return_type: None,
    },
    BuiltinSig {
        name: "unwrap_or",
        return_type: None,
    },
    BuiltinSig {
        name: "uppercase",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "uppercase_first",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "url_decode",
        return_type: None,
    },
    BuiltinSig {
        name: "url_encode",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "username",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "uuid",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "values",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "vision_ocr",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "waitpoint_cancel",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "waitpoint_complete",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "waitpoint_create",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "waitpoint_wait",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "with_rate_limit",
        return_type: None,
    },
    BuiltinSig {
        name: "write_file",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "write_file_bytes",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "yaml_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "yaml_stringify",
        return_type: Some(BuiltinReturn::Named("string")),
    },
];
