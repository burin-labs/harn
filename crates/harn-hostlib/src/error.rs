//! Error type for hostlib host calls.
//!
//! Builtins translate this into VM-level errors via [`Into<harn_vm::VmError>`]
//! so that Harn scripts see structured exceptions rather than panics.

use harn_vm::VmDictExt;

use harn_vm::{VmError, VmValue};

/// All errors a hostlib builtin can surface.
///
/// Variants intentionally describe the *kind* of failure rather than the
/// specific module — every module routes its missing-implementation errors
/// through [`HostlibError::Unimplemented`] so embedders and tests can
/// distinguish intentionally scaffolded contracts from runtime failures.
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

    /// A path the builtin resolved fell outside the session's workspace
    /// roots under a restricted sandbox profile. The mirror of the
    /// `harness.fs.*` `tool_rejected` rejection — both surfaces reject an
    /// out-of-root path with the same message.
    #[error("{message}")]
    SandboxViolation {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// The normalized path that was rejected, for telemetry.
        path: String,
        /// The canonical rejection message (see
        /// [`harn_vm::process_sandbox::SandboxViolation::message`]).
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
            | HostlibError::Backend { builtin, .. }
            | HostlibError::SandboxViolation { builtin, .. } => builtin,
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
            HostlibError::SandboxViolation { .. } => "tool_rejected",
        };
        // Carry the offending path on sandbox violations so `catch` blocks
        // and telemetry can branch on it without re-parsing the message.
        let path = match &err {
            HostlibError::SandboxViolation { path, .. } => Some(path.clone()),
            _ => None,
        };
        let builtin = err.builtin();
        let message = err.to_string();

        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.put_str("kind", kind);
        dict.put_str("builtin", builtin);
        dict.put_str("message", message);
        if let Some(path) = path {
            dict.put_str("path", path);
        }
        VmError::Thrown(VmValue::dict(dict))
    }
}
