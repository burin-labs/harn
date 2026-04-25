//! Error type for hostlib host calls.
//!
//! Builtins translate this into VM-level errors via [`Into<harn_vm::VmError>`]
//! so that Harn scripts see structured exceptions rather than panics.

use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::{VmError, VmValue};

/// All errors a hostlib builtin can surface.
///
/// Variants intentionally describe the *kind* of failure rather than the
/// specific module — every module routes its missing-implementation errors
/// through [`HostlibError::Unimplemented`] so embedders and tests can
/// distinguish intentionally scaffolded contracts from real failures once
/// implementations land.
#[derive(Debug, thiserror::Error)]
pub enum HostlibError {
    /// The method exists in the registration table but has no implementation
    /// yet. This is the canonical scaffold-stage error: it tells callers
    /// "the contract is stable, but this module has not been implemented."
    #[error(
        "hostlib: {builtin} is not implemented yet (scaffolded contract without an implementation)"
    )]
    Unimplemented {
        /// Fully-qualified builtin name, e.g. `"hostlib_ast_parse_file"`.
        builtin: &'static str,
    },

    /// A required parameter was missing from the call payload.
    #[error("hostlib: {builtin}: missing required parameter '{param}'")]
    MissingParameter {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Name of the missing parameter.
        param: &'static str,
    },

    /// A parameter was present but had the wrong shape (wrong type, malformed).
    #[error("hostlib: {builtin}: invalid parameter '{param}': {message}")]
    InvalidParameter {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Name of the invalid parameter.
        param: &'static str,
        /// Human-readable description of the violation.
        message: String,
    },

    /// Catch-all wrapper for I/O, parsing, or other backend failures.
    #[error("hostlib: {builtin}: {message}")]
    Backend {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Human-readable failure description.
        message: String,
    },
}

impl HostlibError {
    /// The fully-qualified builtin name this error came from. Useful for
    /// embedder logging and for the routing tests in `tests/`.
    pub fn builtin(&self) -> &'static str {
        match self {
            HostlibError::Unimplemented { builtin }
            | HostlibError::MissingParameter { builtin, .. }
            | HostlibError::InvalidParameter { builtin, .. }
            | HostlibError::Backend { builtin, .. } => builtin,
        }
    }
}

impl From<HostlibError> for VmError {
    fn from(err: HostlibError) -> VmError {
        // Surface as a `Thrown` dict so Harn `try`/`catch` can pattern-match
        // on `kind`, `builtin`, and `message`. This matches how the existing
        // `host_call` error path shapes its exceptions.
        let kind = match err {
            HostlibError::Unimplemented { .. } => "unimplemented",
            HostlibError::MissingParameter { .. } => "missing_parameter",
            HostlibError::InvalidParameter { .. } => "invalid_parameter",
            HostlibError::Backend { .. } => "backend_error",
        };
        let builtin = err.builtin();
        let message = err.to_string();

        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        dict.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
        dict.insert("builtin".to_string(), VmValue::String(Rc::from(builtin)));
        dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
        VmError::Thrown(VmValue::Dict(Rc::new(dict)))
    }
}
