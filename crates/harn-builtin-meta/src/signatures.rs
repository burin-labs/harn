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
use crate::{BuiltinSignature, Param, Ty, TY_ANY, TY_DICT, TY_DICT_OR_NIL, TY_LIST, TY_STRING};

/// `dict | Schema<T>` — structured-call `schema` argument. Schema aliases
/// type-check as `Schema<T>` but compile to JSON-Schema dicts at runtime.
const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

/// `llm_call(prompt, system?, options?) -> LlmCallResult`
pub const LLM_CALL: BuiltinSignature = BuiltinSignature::simple(
    "llm_call",
    &[
        Param::new("prompt", TY_STRING),
        Param::optional("system", TY_STRING),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    LLM_CALL_RESULT,
);

/// `llm_call_safe(prompt, system?, options?) -> LlmCallSafeResult`
pub const LLM_CALL_SAFE: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_safe",
    &[
        Param::new("prompt", TY_STRING),
        Param::optional("system", TY_STRING),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    LLM_CALL_SAFE_RESULT,
);

/// `llm_completion(prefix, suffix?, system?, options?) -> LlmCallResult`
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

/// `llm_call_structured(prompt, schema, options?) -> any`
pub const LLM_CALL_STRUCTURED: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_ANY,
);

/// `llm_call_structured_safe(prompt, schema, options?) -> dict`
pub const LLM_CALL_STRUCTURED_SAFE: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured_safe",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_DICT,
);

/// `llm_call_structured_result(prompt, schema, options?) -> any`
pub const LLM_CALL_STRUCTURED_RESULT: BuiltinSignature = BuiltinSignature::simple(
    "llm_call_structured_result",
    &[
        Param::new("prompt", TY_STRING),
        Param::new("schema", TY_SCHEMA_VALUE),
        Param::optional("options", LLM_CALL_OPTIONS),
    ],
    TY_ANY,
);

/// `llm_catalog() -> list`. Reachable from the parser as `harness.llm.catalog`.
pub const LLM_CATALOG: BuiltinSignature = BuiltinSignature::simple("llm_catalog", &[], TY_LIST);

/// `llm_provider_status() -> list`. Reachable from the parser as
/// `harness.llm.providers`.
pub const LLM_PROVIDER_STATUS: BuiltinSignature =
    BuiltinSignature::simple("llm_provider_status", &[], TY_LIST);

/// `<T> schema_recover(text, schema: Schema<T>, options?) -> SchemaRecoverEnvelope`.
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
