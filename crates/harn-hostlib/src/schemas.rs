//! Embedded JSON Schemas for every hostlib host method.
//!
//! Schemas live at `schemas/<module>/<method>.{request,response}.json` and
//! are baked into the crate at compile time via `include_str!`. They're the
//! source of truth for hostlib request/response compatibility: the schema
//! files ship with the crate (see the `include` field in `Cargo.toml`),
//! and consumers fetch them through this module.
//!
//! Schemas use JSON Schema draft 2020-12.

mod validation;

pub(crate) use validation::{validate_request_args, validate_response};

/// Direction of a schema (request body vs. response body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaKind {
    /// Schema for the *input* of a host method.
    Request,
    /// Schema for the *output* of a host method.
    Response,
}

// The build script derives the catalog from the directory. Embedders use it
// for drift checks, schema export, and live contract validation.
include!(concat!(env!("OUT_DIR"), "/hostlib_schemas.rs"));

/// Look up a single schema as raw JSON text.
pub fn lookup(module: &str, method: &str, kind: SchemaKind) -> Option<&'static str> {
    SCHEMAS
        .iter()
        .find(|(m, mt, k, _)| *m == module && *mt == method && *k == kind)
        .map(|(_, _, _, body)| *body)
}
