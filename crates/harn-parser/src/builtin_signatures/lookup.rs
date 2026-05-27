//! Public lookup helpers over the unified [`BuiltinSignature`] registry.
//!
//! Both the type checker (this crate) and the runtime (`harn-vm`) consume
//! these helpers. Generic builtins declare their type parameters via
//! [`BuiltinSignature::type_params`] and use [`Ty::Generic`]/[`Ty::SchemaOf`]
//! in param/return positions; the type checker materializes those at each
//! call site against the surrounding scope.

use crate::ast::TypeExpr;

use super::{all_signatures, BuiltinMetadata, BuiltinSignature, Ty, TyExt};

/// Binary-search the registry for a given name. O(log N).
pub fn lookup(name: &str) -> Option<&'static BuiltinSignature> {
    let signatures = all_signatures();
    signatures
        .binary_search_by_key(&name, |sig| sig.name)
        .ok()
        .map(|idx| &signatures[idx])
}

/// Is `name` a builtin known to the parser?
pub fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

/// Every builtin name in alphabetical order.
pub fn iter_builtin_names() -> impl Iterator<Item = &'static str> {
    all_signatures().iter().map(|sig| sig.name)
}

/// Iterate over every builtin's name and statically-known return-type
/// strings. Used by `harn-lint` and other consumers that want a
/// lightweight "what does this builtin return" view without bringing in
/// the full type IR.
pub fn iter_builtin_metadata() -> impl Iterator<Item = BuiltinMetadata> {
    all_signatures().iter().map(|sig| BuiltinMetadata {
        name: sig.name,
        return_types: builtin_return_type_names(sig),
    })
}

/// Statically-known return type for `name`, materialized as a [`TypeExpr`].
/// Returns `None` for unknown names AND for builtins whose return type is
/// genuinely dynamic ([`Ty::Any`]).
pub fn builtin_return_type(name: &str) -> Option<TypeExpr> {
    let sig = lookup(name)?;
    if sig.returns.is_any() {
        return None;
    }
    Some(sig.returns.to_type_expr())
}

/// Returns true if this builtin produces an untyped/opaque value that
/// should be validated before field access in strict types mode.
///
/// This is the same set the linter's `untyped-dict-access` rule treats
/// as boundary sources — JSON parsing, HTTP responses, LLM outputs,
/// host capability calls, etc.
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
            | "websocket_accept"
            | "websocket_receive"
            | "host_call"
            | "connector_call"
            | "host_tool_call"
            | "mcp_call"
    )
}

/// Convert the signature's return type to a tiny `&'static [&'static str]`
/// view used by `BuiltinMetadata` consumers (linter, LSP) that don't
/// pull in the full type IR. Only basic primitive names and the common
/// `T | nil` unions are exposed; everything else returns an empty slice
/// so callers know to consult [`builtin_return_type`] instead.
fn builtin_return_type_names(sig: &BuiltinSignature) -> &'static [&'static str] {
    match &sig.returns {
        Ty::Named(name) => match *name {
            "bool" => &["bool"],
            "bytes" => &["bytes"],
            "dict" => &["dict"],
            "float" => &["float"],
            "int" => &["int"],
            "list" => &["list"],
            "nil" => &["nil"],
            "string" => &["string"],
            _ => &[],
        },
        Ty::Union(members) => match *members {
            [Ty::Named("string"), Ty::Named("nil")] => &["string", "nil"],
            [Ty::Named("nil"), Ty::Named("string")] => &["string", "nil"],
            [Ty::Named("int"), Ty::Named("nil")] => &["int", "nil"],
            [Ty::Named("nil"), Ty::Named("int")] => &["int", "nil"],
            [Ty::Named("dict"), Ty::Named("nil")] => &["dict", "nil"],
            [Ty::Named("nil"), Ty::Named("dict")] => &["dict", "nil"],
            [Ty::Named("bytes"), Ty::Named("nil")] => &["bytes", "nil"],
            [Ty::Named("nil"), Ty::Named("bytes")] => &["bytes", "nil"],
            _ => &[],
        },
        Ty::Never => &["never"],
        _ => &[],
    }
}
