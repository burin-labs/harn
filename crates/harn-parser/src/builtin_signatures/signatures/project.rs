//! Project + store builtin signatures that haven't migrated to
//! `#[harn_builtin]` yet. Metadata/checkpoint/scan entries now ship from
//! their stdlib modules.

use super::{BuiltinSignature, Param, TY_ANY, TY_DICT, TY_DICT_OR_NIL, TY_LIST, TY_NIL, TY_STRING};

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // project_enrich_native(path?, options?) -> dict (LLM-enriched evidence).
    BuiltinSignature::simple(
        "project_enrich_native",
        &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple("store_clear", &[], TY_NIL),
    BuiltinSignature::simple("store_delete", &[Param::new("key", TY_STRING)], TY_NIL),
    // store_get(key) -> stored value | nil. Values round-trip through JSON
    // so the static type is dynamic.
    BuiltinSignature::simple("store_get", &[Param::new("key", TY_STRING)], TY_ANY),
    BuiltinSignature::simple("store_list", &[], TY_LIST),
    BuiltinSignature::simple("store_save", &[], TY_NIL),
    BuiltinSignature::simple(
        "store_set",
        &[Param::new("key", TY_STRING), Param::new("value", TY_ANY)],
        TY_NIL,
    ),
];
