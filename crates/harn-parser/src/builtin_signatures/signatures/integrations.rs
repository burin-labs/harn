//! Connector, host, tool, and shell-facing builtin signatures.

use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT,
    TY_DICT_OR_NIL, TY_INT, TY_LIST, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature::simple(
        "http_delete",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_download",
        &[
            Param::new("url", TY_STRING),
            Param::new("dst_path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_get",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_header",
        &[
            Param::new("source", Ty::Union(&[TY_DICT, TY_LIST])),
            Param::new("name", TY_STRING),
        ],
        TY_STRING_OR_NIL,
    ),
    BuiltinSignature::simple(
        "http_patch",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_post",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_put",
        &[
            Param::new("url", TY_STRING),
            Param::optional("body_or_options", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_request",
        &[
            Param::new("method", TY_STRING),
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response",
        &[
            Param::optional("status", TY_INT),
            Param::optional("body", TY_ANY),
            Param::optional("headers", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_bytes",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_json",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_response_text",
        &[
            Param::optional("body", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_after",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_before",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_on_shutdown",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_readiness",
        &[
            Param::new("server", TY_DICT),
            Param::new("handler", TY_CLOSURE),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_ready",
        &[Param::new("server", TY_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_request",
        &[
            Param::new("server", TY_DICT),
            Param::new("request", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_route",
        &[
            Param::new("server", TY_DICT),
            Param::new("method", TY_STRING),
            Param::new("template", TY_STRING),
            Param::new("handler", TY_CLOSURE),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_security_headers",
        &[Param::new("tls_config", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_set_ready",
        &[Param::new("server", TY_DICT), Param::new("ready", TY_BOOL)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_shutdown",
        &[Param::new("server", TY_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_server_test",
        &[
            Param::new("server", TY_DICT),
            Param::new("request", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_tls_edge",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_server_tls_pem",
        &[
            Param::new("cert_path", TY_STRING),
            Param::new("key_path", TY_STRING),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("http_server_tls_plain", &[], TY_DICT),
    BuiltinSignature::simple(
        "http_server_tls_self_signed_dev",
        &[Param::optional("hosts", Ty::Union(&[TY_LIST, TY_STRING]))],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_session",
        &[Param::optional("options", TY_DICT)],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "http_session_close",
        &[Param::new("session", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_session_request",
        &[
            Param::new("session", TY_STRING),
            Param::new("method", TY_STRING),
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_stream_close",
        &[Param::new("stream", TY_STRING)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "http_stream_info",
        &[Param::new("stream", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "http_stream_open",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "http_stream_read",
        &[
            Param::new("stream", TY_STRING),
            Param::optional("max_bytes", TY_INT),
        ],
        TY_BYTES_OR_NIL,
    ),
    BuiltinSignature::simple("security_policy", &[Param::new("config", TY_DICT)], TY_DICT),
    BuiltinSignature::simple(
        "security_stamp_directive",
        &[
            Param::new("content", TY_STRING),
            Param::optional("emitter", TY_STRING),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "security_verify_directive",
        &[Param::new("content", TY_STRING)],
        TY_DICT,
    ),
    BuiltinSignature::simple("sse_close", &[Param::new("stream", TY_STRING)], TY_BOOL),
    BuiltinSignature::simple(
        "sse_connect",
        &[
            Param::optional("method", TY_STRING),
            Param::optional("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "sse_event",
        &[
            Param::new("event", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "sse_mock",
        &[
            Param::new("url_pattern", TY_STRING),
            Param::optional("events", TY_LIST),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "sse_receive",
        &[
            Param::new("stream", TY_STRING),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "sse_server_cancel",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::optional("reason", TY_STRING),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_cancelled",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_close",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_disconnected",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_flush",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_heartbeat",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::optional("comment", TY_STRING),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_mock_disconnect",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_mock_receive",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "sse_server_response",
        &[Param::optional("options", TY_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "sse_server_send",
        &[
            Param::new("stream", TY_STRING_OR_DICT),
            Param::new("event", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_status",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_DICT,
    ),
    BuiltinSignature::simple("transport_mock_calls", &[], TY_LIST),
    BuiltinSignature::simple("transport_mock_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "websocket_accept",
        &[
            Param::new("server", TY_STRING_OR_DICT),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_close",
        &[Param::new("socket", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_connect",
        &[
            Param::new("url", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_STRING,
    ),
    BuiltinSignature::simple(
        "websocket_mock",
        &[
            Param::new("url_pattern", TY_STRING),
            Param::optional("config", TY_ANY),
        ],
        TY_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_receive",
        &[
            Param::new("socket", TY_STRING_OR_DICT),
            Param::optional("timeout_ms", TY_INT),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple(
        "websocket_route",
        &[
            Param::new("server", TY_STRING_OR_DICT),
            Param::new("path", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_send",
        &[
            Param::new("socket", TY_STRING_OR_DICT),
            Param::new("message", TY_ANY),
            Param::optional("options", TY_DICT),
        ],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "websocket_server",
        &[
            Param::optional("bind", TY_STRING),
            Param::optional("options", TY_DICT),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "websocket_server_close",
        &[Param::new("server", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
];
