//! Front-end glue: go from Harn source (or an already-compiled function) to a
//! verified [`ScalarFunction`].
//!
//! This reuses the real Harn compiler ([`harn_vm::compile_source`]) — the
//! native compiler never re-implements lexing, parsing, or bytecode emission.
//! It only consumes the public [`harn_vm::Chunk`]/[`harn_vm::CompiledFunction`]
//! surface, so it always tracks the language the installed VM actually speaks.

use harn_parser::TypeExpr;
use harn_vm::{compile_source, CompiledFunction};

use crate::error::CodegenError;
use crate::value::ScalarType;
use crate::verify::{verify, ScalarFunction};

/// Verify an already-compiled Harn function, inferring its scalar signature
/// from its declared parameter types.
///
/// # Errors
///
/// Returns [`CodegenError::Unsupported`] if a parameter lacks a scalar type
/// annotation, or any verification error from the body.
pub fn analyze_function(func: &CompiledFunction) -> Result<ScalarFunction, CodegenError> {
    if func.is_generator || func.is_stream {
        return Err(CodegenError::unsupported("generator/stream function"));
    }
    if func.has_rest_param {
        return Err(CodegenError::unsupported("rest parameter"));
    }
    if func.default_start.is_some() {
        return Err(CodegenError::unsupported("default parameter values"));
    }

    let mut params = Vec::with_capacity(func.params.len());
    for param in &func.params {
        let ty = param
            .type_expr
            .as_ref()
            .and_then(scalar_from_type_expr)
            .ok_or_else(|| {
                CodegenError::unsupported(format!(
                    "parameter `{}` must be annotated `int`, `float`, or `bool`",
                    param.name
                ))
            })?;
        params.push(ty);
    }

    verify(
        func.name.clone(),
        &func.chunk.code,
        &func.chunk.constants,
        &params,
    )
}

/// Compile Harn `source`, then verify the named top-level function.
///
/// # Errors
///
/// Returns [`CodegenError::Verify`] if the source fails to compile,
/// [`CodegenError::Unsupported`] if no such function exists, or any error from
/// [`analyze_function`].
pub fn analyze_named(source: &str, function: &str) -> Result<ScalarFunction, CodegenError> {
    let chunk = compile_source(source).map_err(CodegenError::verify)?;
    let func = chunk
        .functions
        .iter()
        .find(|f| f.name == function)
        .ok_or_else(|| CodegenError::unsupported(format!("no function named `{function}`")))?;
    analyze_function(func)
}

/// Map a parameter `TypeExpr` to a scalar type, if it names exactly one of the
/// three unboxed scalars.
fn scalar_from_type_expr(type_expr: &TypeExpr) -> Option<ScalarType> {
    match type_expr {
        TypeExpr::Named(name) => ScalarType::from_harn_name(name),
        _ => None,
    }
}
