//! Generic schema builtins kept in the static fallback table.
//!
//! `schema_recover` is now published through `#[harn_builtin]` in
//! `crates/harn-vm/src/llm/mod.rs` (the macro sig grammar supports `<T>` type
//! parameters, `Schema<T>`, and `@SCHEMA_RECOVER_ENVELOPE` shape injection).
//! The macro signature is authoritative whenever the registry is installed;
//! this static entry remains only as `harn-parser`'s self-contained fallback
//! for standalone typechecking (e.g. the crate's own unit tests, which never
//! install the VM registry). It references the same
//! `harn_builtin_meta::shapes::SCHEMA_RECOVER_ENVELOPE` const as the macro, so
//! the two cannot diverge structurally.

use harn_builtin_meta::shapes::SCHEMA_RECOVER_ENVELOPE;

use super::{BuiltinSignature, Param, Ty, TY_DICT_OR_NIL, TY_STRING};

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
