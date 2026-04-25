use crate::ast::TypeExpr;

use super::{
    all_signatures, generics, BuiltinGenericSig, BuiltinMetadata, BuiltinReturn, BuiltinSig,
    EMPTY_RETURN_TYPES, RETURN_BOOL, RETURN_BYTES, RETURN_DICT, RETURN_FLOAT, RETURN_INT,
    RETURN_LIST, RETURN_NEVER, RETURN_NIL, RETURN_STRING,
};

/// Binary-search the registry for a given name.
fn lookup(name: &str) -> Option<&'static BuiltinSig> {
    let signatures = all_signatures();
    signatures
        .binary_search_by_key(&name, |sig| sig.name)
        .ok()
        .map(|idx| &signatures[idx])
}

/// Is `name` a builtin known to the parser?
pub(crate) fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

/// Every builtin name in alphabetical order, exposed via
/// [`crate::known_builtin_names`] for cross-crate drift testing.
pub(crate) fn iter_builtin_names() -> impl Iterator<Item = &'static str> {
    all_signatures().iter().map(|sig| sig.name)
}

pub(crate) fn iter_builtin_metadata() -> impl Iterator<Item = BuiltinMetadata> {
    all_signatures().iter().map(|sig| BuiltinMetadata {
        name: sig.name,
        return_types: match sig.return_type {
            Some(BuiltinReturn::Named(name)) => match name {
                "bool" => RETURN_BOOL,
                "bytes" => RETURN_BYTES,
                "dict" => RETURN_DICT,
                "float" => RETURN_FLOAT,
                "int" => RETURN_INT,
                "list" => RETURN_LIST,
                "nil" => RETURN_NIL,
                "string" => RETURN_STRING,
                _ => EMPTY_RETURN_TYPES,
            },
            Some(BuiltinReturn::Union(names)) => names,
            Some(BuiltinReturn::Never) => RETURN_NEVER,
            None => EMPTY_RETURN_TYPES,
        },
    })
}

/// Statically-known return type for `name`. `None` for unknown names OR
/// builtins with a dynamic return type (e.g. `json_parse`).
pub(crate) fn builtin_return_type(name: &str) -> Option<TypeExpr> {
    let sig = lookup(name)?;
    match sig.return_type? {
        BuiltinReturn::Named(ty) => Some(TypeExpr::Named(ty.into())),
        BuiltinReturn::Union(tys) => Some(TypeExpr::Union(
            tys.iter().map(|ty| TypeExpr::Named((*ty).into())).collect(),
        )),
        BuiltinReturn::Never => Some(TypeExpr::Never),
    }
}

pub(crate) fn lookup_generic_builtin_sig(name: &str) -> Option<BuiltinGenericSig> {
    generics::lookup_generic_builtin_sig(name)
}

/// Returns true if this builtin produces an untyped/opaque value that should
/// be validated before field access in strict types mode.
pub fn is_untyped_boundary_source(name: &str) -> bool {
    matches!(
        name,
        "json_parse"
            | "json_extract"
            | "yaml_parse"
            | "toml_parse"
            | "llm_call"
            | "llm_call_safe"
            | "llm_completion"
            | "http_get"
            | "http_post"
            | "http_put"
            | "http_patch"
            | "http_delete"
            | "http_download"
            | "http_request"
            | "http_session_request"
            | "http_stream_info"
            | "sse_receive"
            | "sse_server_mock_receive"
            | "sse_server_response"
            | "sse_server_status"
            | "websocket_receive"
            | "host_call"
            | "connector_call"
            | "host_tool_call"
            | "mcp_call"
    )
}
