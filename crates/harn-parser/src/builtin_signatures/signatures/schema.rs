//! Generic schema builtins that the `#[harn_builtin]` macro cannot yet
//! express.
//!
//! The macro grammar in `harn-builtin-macros` does not support type
//! parameters (`<T>`) or `Schema<T>` constraints, so a handful of schema
//! builtins still rely on a hand-written `BuiltinSignature::generic`
//! literal here. Once the macro grows generic support these entries
//! disappear too.
//!
//! Currently only `schema_recover` is parser-only here — its impl lives
//! in `crates/harn-vm/src/llm/schema_recover.rs` and registers via the
//! DSL builder in `llm/mod.rs`. The other generic entries
//! (`schema_check`, `schema_expect`, `schema_parse`) were migrated to
//! `#[harn_builtin]` in `crates/harn-vm/src/stdlib/json.rs`; the merged
//! installed signatures take precedence so any duplicate static entry
//! is dead and stays out of this file.

use super::{
    BuiltinSignature, Param, ShapeFieldDescriptor, Ty, TY_BOOL, TY_DICT_OR_NIL, TY_INT, TY_NIL,
    TY_STRING, TY_STRING_OR_NIL,
};

/// Diagnostic envelope returned by `schema_recover`. `data` narrows to
/// `T | nil` so callers can dispatch on `ok` and unwrap on success. Other
/// envelope fields are stably-typed regardless of `T`.
const SCHEMA_RECOVER_ENVELOPE: Ty = Ty::Shape(&[
    ShapeFieldDescriptor::new("ok", TY_BOOL),
    ShapeFieldDescriptor::new("data", Ty::Union(&[Ty::Generic("T"), TY_NIL])),
    ShapeFieldDescriptor::new("raw_text", TY_STRING),
    ShapeFieldDescriptor::new("error", TY_STRING),
    ShapeFieldDescriptor::new("error_category", TY_STRING_OR_NIL),
    ShapeFieldDescriptor::new("attempts", TY_INT),
    ShapeFieldDescriptor::new("stage", TY_STRING),
    ShapeFieldDescriptor::new("repaired", TY_BOOL),
]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // schema_recover(text, schema, options?) -> diagnostic envelope. When
    // `schema: Schema<T>`, `data` narrows to `T | nil` (nil on failure).
    // See harn#906 for the staged repair pipeline this powers.
    BuiltinSignature::generic(
        "schema_recover",
        &["T"],
        &[
            Param::new("text", TY_STRING),
            Param::new("schema", Ty::SchemaOf("T")),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        SCHEMA_RECOVER_ENVELOPE,
    ),
];
