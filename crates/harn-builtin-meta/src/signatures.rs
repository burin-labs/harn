//! Canonical [`BuiltinSignature`] definitions for builtins that **both**
//! `harn-parser` (typecheck) and `harn-vm` (runtime) must agree on at compile
//! time.
//!
//! Most builtins are defined once via the `#[harn_builtin(sig = "…")]` macro in
//! `harn-vm`; the parser learns them from the driver-installed registry. But a
//! handful of LLM builtins are *first-class to the typechecker itself*
//! (boundary-source tracking, structured-output schema typing) and so are
//! referenced by `harn-parser`'s own unit tests, which run without a driver and
//! cannot see harn-vm (it compiles later). Rather than hand-maintain a second
//! definition in the parser's static tables — the duplication that let LLM
//! signatures silently drift — both sides reference the single `const` here:
//!
//! * `harn-parser`'s static signature table lists these consts directly;
//! * the `#[harn_builtin(sig_expr = …)]` macro in harn-vm uses the same const
//!   as the published runtime signature.
//!
//! Adding a field to one of these shapes (in [`crate::shapes`]) updates both
//! sides at once; there is no second place to forget.

use crate::shapes::{
    LLM_CALL_OPTIONS, LLM_CALL_RESULT, LLM_CALL_SAFE_RESULT, SCHEMA_RECOVER_ENVELOPE,
};
use crate::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_BYTES, TY_DICT, TY_DICT_OR_NIL, TY_INT, TY_LIST,
    TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

/// Pure source builtins implemented by the portable kernel. The parser's
/// standalone fallback, native handlers, browser compiler, and portable
/// execution registry all project these exact values.
pub const PORTABLE_LEN: BuiltinSignature = BuiltinSignature::simple(
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
);
pub const PORTABLE_TO_STRING: BuiltinSignature =
    BuiltinSignature::variadic("to_string", &[Param::new("args", TY_ANY)], TY_STRING);
pub const PORTABLE_HEX_ENCODE: BuiltinSignature = BuiltinSignature::simple(
    "hex_encode",
    &[Param::new("input", Ty::Union(&[TY_STRING, TY_BYTES]))],
    TY_STRING,
);
pub const PORTABLE_HEX_DECODE: BuiltinSignature = BuiltinSignature::simple(
    "hex_decode",
    &[Param::new("text", TY_STRING_OR_NIL)],
    TY_STRING,
);
pub const PORTABLE_TRIM: BuiltinSignature =
    BuiltinSignature::simple("trim", &[Param::new("text", TY_STRING_OR_NIL)], TY_STRING);
pub const PORTABLE_REPLACE: BuiltinSignature = BuiltinSignature::simple(
    "replace",
    &[
        Param::new("text", TY_STRING_OR_NIL),
        Param::new("old", TY_STRING),
        Param::new("new", TY_STRING),
    ],
    TY_STRING,
);
pub const PORTABLE_STARTS_WITH: BuiltinSignature = BuiltinSignature::simple(
    "starts_with",
    &[
        Param::new("text", TY_STRING_OR_NIL),
        Param::new("prefix", TY_STRING_OR_NIL),
    ],
    crate::TY_BOOL,
);
pub const PORTABLE_JSON_STRINGIFY: BuiltinSignature =
    BuiltinSignature::simple("json_stringify", &[Param::new("value", TY_ANY)], TY_STRING);
pub const PORTABLE_REGEX_MATCH: BuiltinSignature = BuiltinSignature::simple(
    "regex_match",
    &[
        Param::new("pattern", TY_STRING_OR_NIL),
        Param::new("text", TY_STRING_OR_NIL),
        Param::optional("flags", TY_STRING),
    ],
    Ty::Union(&[TY_LIST, TY_NIL]),
);
pub const PORTABLE_REGEX_REPLACE: BuiltinSignature = BuiltinSignature::simple(
    "regex_replace",
    &[
        Param::new("pattern", TY_STRING_OR_NIL),
        Param::new("replacement", TY_STRING_OR_NIL),
        Param::new("text", TY_STRING_OR_NIL),
        Param::optional("flags", TY_STRING),
    ],
    TY_STRING,
);
pub const PORTABLE_REGEX_CAPTURES: BuiltinSignature = BuiltinSignature::simple(
    "regex_captures",
    &[
        Param::new("pattern", TY_STRING_OR_NIL),
        Param::new("text", TY_STRING_OR_NIL),
        Param::optional("flags", TY_STRING),
    ],
    TY_LIST,
);
pub const PORTABLE_REGEX_SPLIT: BuiltinSignature = BuiltinSignature::simple(
    "regex_split",
    &[
        Param::new("text", TY_STRING_OR_NIL),
        Param::new("pattern", TY_STRING_OR_NIL),
        Param::optional("flags", TY_STRING),
    ],
    TY_LIST,
);
pub const PORTABLE_SHA256: BuiltinSignature = BuiltinSignature::simple(
    "sha256",
    &[Param::new("input", Ty::Union(&[TY_STRING, TY_BYTES]))],
    TY_STRING,
);
pub const PORTABLE_SECRET_SCAN: BuiltinSignature =
    BuiltinSignature::simple("secret_scan", &[Param::new("content", TY_ANY)], TY_LIST);
pub const PORTABLE_PATH_JOIN: BuiltinSignature =
    BuiltinSignature::variadic("path_join", &[Param::new("args", TY_ANY)], TY_STRING);

pub const PORTABLE_SOURCE_BUILTINS: &[BuiltinSignature] = &[
    PORTABLE_LEN,
    PORTABLE_TO_STRING,
    PORTABLE_HEX_ENCODE,
    PORTABLE_HEX_DECODE,
    PORTABLE_TRIM,
    PORTABLE_REPLACE,
    PORTABLE_STARTS_WITH,
    PORTABLE_JSON_STRINGIFY,
    PORTABLE_REGEX_MATCH,
    PORTABLE_REGEX_REPLACE,
    PORTABLE_REGEX_CAPTURES,
    PORTABLE_REGEX_SPLIT,
    PORTABLE_SHA256,
    PORTABLE_SECRET_SCAN,
    PORTABLE_PATH_JOIN,
];

/// `dict | Schema<T>` — structured-call `schema` argument. Schema aliases
/// type-check as `Schema<T>` but compile to JSON-Schema dicts at runtime.
const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

/// `harness.llm.call(prompt, system?, options?) -> LlmCallResult`
pub const LLM_CALL: BuiltinSignature = BuiltinSignature::simple(
    "llm_call",
    &[
        Param::new("prompt", TY_STRING),
        Param::optional("system", TY_STRING),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    LLM_CALL_RESULT,
);

/// `harness.llm.call_safe(prompt, system?, options?) -> LlmCallSafeResult`
pub const LLM_CALL_SAFE: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_safe",
    &[
        Param::new("prompt", TY_STRING),
        Param::optional("system", TY_STRING),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    LLM_CALL_SAFE_RESULT,
);

/// `harness.llm.completion(prefix, suffix?, system?, options?) -> LlmCallResult`
pub const LLM_COMPLETION: BuiltinSignature = BuiltinSignature::simple(
    "llm_completion",
    &[
        Param::new("prefix", TY_STRING),
        Param::optional("suffix", TY_STRING),
        Param::optional("system", TY_STRING),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    LLM_CALL_RESULT,
);

/// `harness.llm.call_structured(prompt, schema, options?) -> any`
pub const LLM_CALL_STRUCTURED: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_ANY,
);

/// `harness.llm.call_structured_safe(prompt, schema, options?) -> dict`
pub const LLM_CALL_STRUCTURED_SAFE: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured_safe",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_DICT,
);

/// `harness.llm.call_structured_result(prompt, schema, options?) -> any`
pub const LLM_CALL_STRUCTURED_RESULT: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured_result",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_ANY,
);

/// `harness.llm.catalog() -> list`.
pub const LLM_CATALOG: BuiltinSignature = BuiltinSignature::simple("llm_catalog", &[], TY_LIST);

/// `harness.llm.catalog_refresh(options?) -> dict`.
pub const LLM_CATALOG_REFRESH: BuiltinSignature = BuiltinSignature::simple(
    "llm_catalog_refresh",
    &[Param::optional("options", TY_DICT_OR_NIL)],
    TY_DICT,
);

/// `harness.llm.providers() -> list`.
pub const LLM_PROVIDER_STATUS: BuiltinSignature =
    BuiltinSignature::simple("llm_provider_status", &[], TY_LIST);

/// `<T> harness.llm.recover_schema(text, schema: Schema<T>, options?) -> SchemaRecoverEnvelope`.
/// When `schema: Schema<T>`, the envelope's `data` narrows to `T | nil`.
pub const SCHEMA_RECOVER: BuiltinSignature = BuiltinSignature::generic(
    "schema_recover",
    &["T"],
    &[
        Param::new("text", TY_STRING),
        Param::new("schema", Ty::SchemaOf("T")),
        Param::optional("options", TY_DICT_OR_NIL),
    ],
    SCHEMA_RECOVER_ENVELOPE,
);
