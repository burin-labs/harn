//! Schema-related builtin signatures.
//!
//! Schema builtins live on the parser/runtime boundary: they take an
//! arbitrary value plus a `Schema<T>` and either validate, parse,
//! transform, or coerce. The generic ones (`schema_parse`,
//! `schema_check`, `schema_expect`, `schema_recover`) declare a `T` type
//! parameter so the type checker can pull `T` out of the schema-value
//! argument and narrow the return.

use super::{
    BuiltinSignature, Param, ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_DICT, TY_DICT_OR_NIL,
    TY_INT, TY_LIST, TY_NIL, TY_STRING, TY_STRING_OR_NIL,
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

const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // is_type(value, schema) — alias for `schema_is`.
    BuiltinSignature {
        name: "is_type",
        params: &[
            Param::new("value", TY_ANY),
            Param::new("schema", TY_SCHEMA_VALUE),
        ],
        returns: TY_BOOL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_check(value, schema) -> Result<T, string>. Runtime returns a
    // result-shaped dict (`{ok, value, errors}`) narrowed to `T` via the
    // schema argument.
    BuiltinSignature {
        name: "schema_check",
        params: &[
            Param::new("value", TY_ANY),
            Param::new("schema", Ty::SchemaOf("T")),
        ],
        returns: Ty::Apply("Result", &[Ty::Generic("T"), TY_STRING]),
        type_params: &["T"],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_expect(value, schema, apply_defaults?) -> T. Throws on
    // validation failure; returns the validated/coerced `T` on success.
    BuiltinSignature {
        name: "schema_expect",
        params: &[
            Param::new("value", TY_ANY),
            Param::new("schema", Ty::SchemaOf("T")),
            Param::optional("apply_defaults", TY_BOOL),
        ],
        returns: Ty::Generic("T"),
        type_params: &["T"],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_extend(base, overrides) — merge two schema dicts.
    BuiltinSignature {
        name: "schema_extend",
        params: &[
            Param::new("base", TY_DICT),
            Param::new("overrides", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_from_json_schema(json_schema) -> canonical Harn schema dict.
    BuiltinSignature {
        name: "schema_from_json_schema",
        params: &[Param::new("json_schema", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_from_openapi_schema(openapi_schema) -> canonical Harn schema
    // dict.
    BuiltinSignature {
        name: "schema_from_openapi_schema",
        params: &[Param::new("openapi_schema", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_is(value, schema) -> bool. Cheap predicate version of
    // `schema_check` / `schema_expect` that swallows error details.
    BuiltinSignature {
        name: "schema_is",
        params: &[
            Param::new("value", TY_ANY),
            Param::new("schema", TY_SCHEMA_VALUE),
        ],
        returns: TY_BOOL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_of(type_alias) — compile-time intrinsic that the compiler
    // rewrites to the alias's JSON-Schema dict constant. The runtime
    // fallback accepts an already-built schema dict and returns it
    // unchanged.
    BuiltinSignature {
        name: "schema_of",
        params: &[Param::new("type_alias", TY_ANY)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_omit(schema, keys) -> dict with the listed keys removed.
    BuiltinSignature {
        name: "schema_omit",
        params: &[
            Param::new("schema", TY_SCHEMA_VALUE),
            Param::new("keys", TY_LIST),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_parse(value, schema) -> Result<T, string>. Same generic
    // shape as `schema_check`; the difference is `schema_parse` applies
    // defaults from the schema.
    BuiltinSignature {
        name: "schema_parse",
        params: &[
            Param::new("value", TY_ANY),
            Param::new("schema", Ty::SchemaOf("T")),
        ],
        returns: Ty::Apply("Result", &[Ty::Generic("T"), TY_STRING]),
        type_params: &["T"],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_partial(schema) -> dict with all top-level fields marked
    // optional.
    BuiltinSignature {
        name: "schema_partial",
        params: &[Param::new("schema", TY_SCHEMA_VALUE)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_pick(schema, keys) -> dict containing only the listed keys.
    BuiltinSignature {
        name: "schema_pick",
        params: &[
            Param::new("schema", TY_SCHEMA_VALUE),
            Param::new("keys", TY_LIST),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_recover(text, schema, options?) -> diagnostic envelope. When
    // `schema: Schema<T>`, `data` narrows to `T | nil` (nil on failure).
    // See harn#906 for the staged repair pipeline this powers.
    BuiltinSignature {
        name: "schema_recover",
        params: &[
            Param::new("text", TY_STRING),
            Param::new("schema", Ty::SchemaOf("T")),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: SCHEMA_RECOVER_ENVELOPE,
        type_params: &["T"],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_to_json_schema(schema) -> draft-07-shaped JSON Schema dict.
    BuiltinSignature {
        name: "schema_to_json_schema",
        params: &[Param::new("schema", TY_SCHEMA_VALUE)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // schema_to_openapi_schema(schema) -> OpenAPI-shaped schema dict.
    BuiltinSignature {
        name: "schema_to_openapi_schema",
        params: &[Param::new("schema", TY_SCHEMA_VALUE)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
];
