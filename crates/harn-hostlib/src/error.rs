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

    /// The requested native secret-store backend is unavailable in the current
    /// host session. `reason` is a stable machine-readable classifier;
    /// `message` retains the platform detail for diagnostics.
    #[error("hostlib: {builtin}: backend unavailable ({reason}): {message}")]
    NativeSecretStoreUnavailable {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Stable, typed capability classifier.
        reason: harn_vm::secrets::NativeKeyringUnavailable,
        /// Human-readable platform detail.
        message: String,
    },

    /// The OS could not start a requested process. `kind` is the canonical
    /// `io::ErrorKind` spelling retained for script-side classification.
    #[error("hostlib: {builtin}: process spawn failed: {message}")]
    ProcessSpawn {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Stable kind such as `not_found` or `permission_denied`.
        kind: &'static str,
        /// Human-readable OS error.
        message: String,
        /// Canonical caller-selected directory, absent when the command
        /// inherited its directory from the active execution context.
        requested_cwd: Option<String>,
        /// Canonical directory in which the spawn was attempted.
        cwd: String,
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

    /// A host capability cannot preserve the active sandbox contract.
    #[error("{message}")]
    SandboxUnsupported {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// Active sandbox profile.
        profile: String,
        /// Stable rejection message.
        message: String,
    },

    /// A never-approvable UNIVERSAL catastrophic command (machine/disk/data
    /// destruction) was rejected by the floor BEFORE spawning, at the shared
    /// [`crate::process::spawn_process`] chokepoint. Enforced unconditionally
    /// (no `command_policy` required), so it is universal across every hostlib
    /// process tool, embedders, and standalone Harn. Mirrors the
    /// `catastrophic_floor` disposition the `process.exec` command-policy
    /// preflight surfaces. See
    /// [`harn_vm::orchestration::universal_catastrophic_reason`].
    #[error("{message}")]
    CatastrophicFloor {
        /// Fully-qualified builtin name.
        builtin: &'static str,
        /// The verbatim floor rationale (never-approvable reason).
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
            | HostlibError::NativeSecretStoreUnavailable { builtin, .. }
            | HostlibError::ProcessSpawn { builtin, .. }
            | HostlibError::SandboxViolation { builtin, .. }
            | HostlibError::SandboxUnsupported { builtin, .. }
            | HostlibError::CatastrophicFloor { builtin, .. } => builtin,
        }
    }
}

impl From<HostlibError> for VmError {
    fn from(err: HostlibError) -> VmError {
        // Surface as a `Thrown` dict so Harn `try`/`catch` can pattern-match
        // on `kind`, `builtin`, and `message`. This matches how the existing
        // `host_call` error path shapes its exceptions.
        let kind = match &err {
            HostlibError::Unimplemented { .. } => "unimplemented",
            HostlibError::MissingParameter { .. } => "missing_parameter",
            HostlibError::InvalidParameter { .. } => "invalid_parameter",
            HostlibError::Backend { .. } => "backend_error",
            HostlibError::NativeSecretStoreUnavailable { .. } => "backend_unavailable",
            HostlibError::ProcessSpawn { kind, .. } => *kind,
            HostlibError::SandboxViolation { .. } => "tool_rejected",
            HostlibError::SandboxUnsupported { .. } => "sandbox_unsupported",
            HostlibError::CatastrophicFloor { .. } => "catastrophic_floor",
        };
        // Carry the offending path on sandbox violations so `catch` blocks
        // and telemetry can branch on it without re-parsing the message.
        let path = match &err {
            HostlibError::SandboxViolation { path, .. } => Some(path.clone()),
            _ => None,
        };
        let profile = match &err {
            HostlibError::SandboxUnsupported { profile, .. } => Some(profile.clone()),
            _ => None,
        };
        let unavailable_reason = match &err {
            HostlibError::NativeSecretStoreUnavailable { reason, .. } => Some(*reason),
            _ => None,
        };
        let builtin = err.builtin();
        let is_process_spawn = matches!(&err, HostlibError::ProcessSpawn { .. });
        let process_cwd = match &err {
            HostlibError::ProcessSpawn {
                requested_cwd, cwd, ..
            } => Some((requested_cwd.clone(), cwd.clone())),
            _ => None,
        };
        let message = err.to_string();

        let mut dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        dict.put_str("kind", kind);
        dict.put_str("builtin", builtin);
        dict.put_str("message", message);
        if is_process_spawn {
            dict.put_str("error", "io_error");
            dict.put_str("operation", "process_spawn");
            dict.put_str("category", "environment");
        }
        if let Some((requested_cwd, cwd)) = process_cwd {
            match requested_cwd {
                Some(requested_cwd) => dict.put_str("requested_cwd", requested_cwd),
                None => {
                    dict.insert(harn_vm::value::intern_key("requested_cwd"), VmValue::Nil);
                }
            }
            dict.put_str("cwd", cwd);
        }
        if let Some(path) = path {
            dict.put_str("path", path);
        }
        if let Some(profile) = profile {
            dict.put_str("profile", profile);
        }
        if let Some(reason) = unavailable_reason {
            dict.put_str("reason", reason.as_str());
        }
        VmError::Thrown(VmValue::dict(dict))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_spawn_error_lowers_to_typed_io_value() {
        let error = HostlibError::ProcessSpawn {
            builtin: "hostlib_tools_run_command",
            kind: "not_found",
            message: "No such file or directory".to_string(),
            requested_cwd: Some("/workspace/project".to_string()),
            cwd: "/workspace/project".to_string(),
        };
        let VmError::Thrown(VmValue::Dict(fields)) = VmError::from(error) else {
            panic!("expected a structured thrown value");
        };
        let field = |name| fields.get(name).map(VmValue::display);
        assert_eq!(field("error").as_deref(), Some("io_error"));
        assert_eq!(field("kind").as_deref(), Some("not_found"));
        assert_eq!(field("operation").as_deref(), Some("process_spawn"));
        assert_eq!(field("category").as_deref(), Some("environment"));
        assert_eq!(
            field("requested_cwd").as_deref(),
            Some("/workspace/project")
        );
        assert_eq!(field("cwd").as_deref(), Some("/workspace/project"));
        assert_eq!(
            field("builtin").as_deref(),
            Some("hostlib_tools_run_command")
        );
    }
}
