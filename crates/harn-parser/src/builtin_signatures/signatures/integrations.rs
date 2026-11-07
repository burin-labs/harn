//! Integration helpers without a typed capability-method projection.

use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BOOL, TY_DICT, TY_DICT_OR_NIL, TY_INT, TY_LIST, TY_NIL,
    TY_STRING, TY_STRING_OR_NIL,
};

const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature::simple(
        "http_header",
        &[
            Param::new("source", Ty::Union(&[TY_DICT, TY_LIST])),
            Param::new("name", TY_STRING),
        ],
        TY_STRING_OR_NIL,
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
        "sse_server_mock_disconnect",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_BOOL,
    ),
    BuiltinSignature::simple(
        "sse_server_mock_receive",
        &[Param::new("stream", TY_STRING_OR_DICT)],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("transport_mock_calls", &[], TY_LIST),
    BuiltinSignature::simple("transport_mock_clear", &[], TY_NIL),
    BuiltinSignature::simple(
        "websocket_mock",
        &[
            Param::new("url_pattern", TY_STRING),
            Param::optional("config", TY_ANY),
        ],
        TY_NIL,
    ),
];
