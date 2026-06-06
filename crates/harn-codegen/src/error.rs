//! Error types for the native compiler.

use std::fmt;

/// Why a chunk could not be lowered to native code.
///
/// `Unsupported` is the common, *expected* outcome: the function uses a
/// language feature outside the scalar-compute subset (a string, a host call,
/// `await`, …). Callers treat it as "fall back to the interpreter", not as a
/// hard failure. `Verify` means the bytecode is in the subset but is not
/// well-typed enough to lower soundly. `Backend` wraps a Cranelift failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenError {
    /// A feature outside the supported scalar subset was encountered. The
    /// payload names the offending construct so tooling can explain *why* a
    /// function stayed on the interpreter.
    Unsupported(String),
    /// The bytecode is in the supported subset but failed verification (e.g.
    /// inconsistent operand-stack types across a control-flow merge).
    Verify(String),
    /// The Cranelift backend rejected or failed to compile the lowered IR.
    Backend(String),
}

impl CodegenError {
    pub(crate) fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub(crate) fn verify(msg: impl Into<String>) -> Self {
        Self::Verify(msg.into())
    }

    pub(crate) fn backend(msg: impl Into<String>) -> Self {
        Self::Backend(msg.into())
    }

    /// True when the function simply falls outside the scalar subset, as
    /// opposed to being malformed or hitting a backend bug.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(msg) => write!(f, "unsupported for native compilation: {msg}"),
            Self::Verify(msg) => write!(f, "bytecode verification failed: {msg}"),
            Self::Backend(msg) => write!(f, "native backend error: {msg}"),
        }
    }
}

impl std::error::Error for CodegenError {}

/// A runtime fault raised by JIT-compiled code through the side-channel trap
/// flag. Mirrors the interpreter's behaviour of returning an error rather than
/// aborting the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTrap {
    /// Integer division or remainder by zero. Matches the interpreter, which
    /// raises a runtime error for `x / 0` and `x % 0` on integers.
    DivideByZero,
}

impl fmt::Display for NativeTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DivideByZero => f.write_str("integer divide by zero"),
        }
    }
}

impl std::error::Error for NativeTrap {}
