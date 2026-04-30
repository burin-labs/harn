//! Schema-related builtin signatures.

use super::{BuiltinReturn, BuiltinSig};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "is_type",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "schema_check",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_expect",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_extend",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_from_json_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_from_openapi_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_is",
        return_type: Some(BuiltinReturn::Named("bool")),
    },
    BuiltinSig {
        name: "schema_of",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_omit",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_parse",
        return_type: None,
    },
    BuiltinSig {
        name: "schema_partial",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_pick",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_recover",
        // Returns a diagnostic envelope dict whose `data` field
        // narrows to `T | nil` when called with `Schema<T>` —
        // resolved by `lookup_generic_builtin_sig`. Fall-through
        // `None` keeps the parser from defaulting to a concrete
        // dict type when the schema isn't a typed alias. See
        // harn#906.
        return_type: None,
    },
    BuiltinSig {
        name: "schema_to_json_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "schema_to_openapi_schema",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
];
