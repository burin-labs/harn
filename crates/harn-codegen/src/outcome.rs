//! The result of running a compiled scalar function: a concrete value, or a
//! *deopt* signal that the computation left the monomorphic scalar subset at
//! runtime.
//!
//! # Why a deopt channel exists
//!
//! The native compiler models Harn `int` as a flat two's-complement `i64`. The
//! Harn VM does **not**: integer `+`, `-`, `*`, and unary negation *promote to
//! `float`* when the `i64` result would overflow, rather than wrapping (see
//! `harn_vm`'s `int_add`/`int_sub`/`int_mul`/`int_neg`, which return a
//! `VmValue::Float` on `checked_*` overflow). A bare `a + b` therefore agrees
//! with `[a, b].sum()` — both promote — and never silently produces a
//! wrong-magnitude wrapped value.
//!
//! That promotion is fundamentally polymorphic: the *type* of `a + b` depends
//! on the runtime *values* of `a` and `b`. A monomorphic `int`-typed native
//! kernel cannot represent it. So instead of silently wrapping (which would
//! make the JIT disagree with the interpreter — an unsound oracle), the native
//! code and the reference interpreter both **deopt**: they stop and report
//! that the VM would have promoted here, leaving the caller to re-run the
//! function on the interpreter or the VM for the true `float` result.
//!
//! This is the standard guard-and-deopt discipline of production JITs (V8's
//! Smi overflow path, for instance): stay on the fast monomorphic path while
//! the guard holds, bail to the general path when it doesn't. It keeps the
//! contract honest — a native result is *always* bit-identical to the VM, or
//! an explicit `Deopt` — never a quietly wrong answer.

use crate::value::ScalarValue;

/// Why a scalar function deoptimised: it left the monomorphic scalar subset at
/// runtime and the Harn VM would produce a value the native representation
/// cannot hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeoptReason {
    /// An integer `+`, `-`, `*`, or unary negation overflowed `i64`. The Harn
    /// VM promotes such a result to `float`; the native subset is monomorphic
    /// `int` and cannot, so it bails. Re-run the function on the interpreter or
    /// VM for the true promoted value.
    IntegerOverflow,
}

impl DeoptReason {
    /// A short human-readable explanation, for diagnostics.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::IntegerOverflow => {
                "integer arithmetic overflowed i64; the VM promotes to float (out of scalar subset)"
            }
        }
    }
}

impl std::fmt::Display for DeoptReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// The outcome of evaluating a scalar function.
///
/// Either a concrete scalar [`Value`](NativeOutcome::Value) — guaranteed
/// bit-identical to what the Harn VM produces — or a
/// [`Deopt`](NativeOutcome::Deopt) signal that the run left the scalar subset
/// and must be re-executed on the VM. Genuine runtime *errors* (integer divide
/// by zero) are reported separately, through the `Err` channel of the calling
/// API, because the VM raises them too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeOutcome {
    /// A scalar result, bit-identical to the Harn VM's.
    Value(ScalarValue),
    /// The computation must be retried on the interpreter/VM; see
    /// [`DeoptReason`].
    Deopt(DeoptReason),
}

impl NativeOutcome {
    /// The scalar value, if the run stayed in the subset.
    #[must_use]
    pub const fn value(self) -> Option<ScalarValue> {
        match self {
            Self::Value(v) => Some(v),
            Self::Deopt(_) => None,
        }
    }

    /// True if the run deoptimised.
    #[must_use]
    pub const fn is_deopt(&self) -> bool {
        matches!(self, Self::Deopt(_))
    }
}

impl std::fmt::Display for NativeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(v) => write!(f, "{v}"),
            Self::Deopt(reason) => write!(f, "deopt ({reason})"),
        }
    }
}
