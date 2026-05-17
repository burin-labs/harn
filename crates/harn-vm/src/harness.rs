//! Capability handle threaded into every Harn script as the `harness`
//! parameter of `main`.
//!
//! `Harness` is the Harn-language analog of an explicit-capability handle: a
//! single value the runtime hands to a script's `main` so that stdio, clock,
//! filesystem, environment, randomness, and network access become surface in
//! the type system instead of ambient globals. Each sub-handle (`stdio`,
//! `clock`, `fs`, `env`, `random`, `net`) is a distinct named type that
//! anchors the surface for its capability slice.
//!
//! This module defines:
//!   * The runtime [`Harness`] value (and six sub-handle wrappers).
//!   * [`Harness::real`], the production constructor that wraps wall-clock
//!     time, `tokio::fs`, `std::env`, `rand::thread_rng`, and `reqwest`. The
//!     downstream migration tickets (E4.2-E4.4) populate the per-handle
//!     method surface against this concrete state; for E4.1 only the
//!     `stdio.println` / `stdio.eprintln` path is wired so the new
//!     entrypoint convention has a working hello-world steel thread.
//!   * [`VmHarness`], the compact `VmValue` payload that carries the same
//!     state through the bytecode VM and distinguishes the root handle from
//!     its sub-handles via [`HarnessKind`].

use std::fmt;
use std::sync::Arc;

use harn_clock::{Clock, RealClock};

/// Six capability slices exposed by a [`Harness`].
///
/// `Root` is the parent handle; the others are the typed sub-handles users
/// reach through field access (`harness.stdio`, `harness.clock`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessKind {
    Root,
    Stdio,
    Clock,
    Fs,
    Env,
    Random,
    Net,
}

impl HarnessKind {
    /// The Harn-language type name for this kind (`Harness`, `HarnessStdio`,
    /// etc.). Used by the typechecker primitive registration and by
    /// `VmValue::type_name`.
    pub const fn type_name(self) -> &'static str {
        match self {
            HarnessKind::Root => "Harness",
            HarnessKind::Stdio => "HarnessStdio",
            HarnessKind::Clock => "HarnessClock",
            HarnessKind::Fs => "HarnessFs",
            HarnessKind::Env => "HarnessEnv",
            HarnessKind::Random => "HarnessRandom",
            HarnessKind::Net => "HarnessNet",
        }
    }

    /// Field name a parent `Harness` exposes for this sub-handle (e.g. the
    /// `stdio` in `harness.stdio`). Returns `None` for the root.
    pub const fn field_name(self) -> Option<&'static str> {
        match self {
            HarnessKind::Root => None,
            HarnessKind::Stdio => Some("stdio"),
            HarnessKind::Clock => Some("clock"),
            HarnessKind::Fs => Some("fs"),
            HarnessKind::Env => Some("env"),
            HarnessKind::Random => Some("random"),
            HarnessKind::Net => Some("net"),
        }
    }

    /// Parse the field name a script uses to reach a sub-handle.
    pub fn from_field_name(name: &str) -> Option<Self> {
        match name {
            "stdio" => Some(HarnessKind::Stdio),
            "clock" => Some(HarnessKind::Clock),
            "fs" => Some(HarnessKind::Fs),
            "env" => Some(HarnessKind::Env),
            "random" => Some(HarnessKind::Random),
            "net" => Some(HarnessKind::Net),
            _ => None,
        }
    }

    /// All six sub-handle kinds, in the canonical field order.
    pub const SUB_HANDLES: &'static [HarnessKind] = &[
        HarnessKind::Stdio,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
    ];

    /// Every kind a Harn-script type annotation may reference.
    pub const ALL: &'static [HarnessKind] = &[
        HarnessKind::Root,
        HarnessKind::Stdio,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
    ];
}

/// Shared, refcounted state backing every sub-handle of a single `Harness`.
///
/// Method implementations (in `crate::vm::methods::harness`) borrow this to
/// reach the concrete OS-backed primitives. Wrapped in `Arc` so handles are
/// `Send + Sync` for VM contexts that move work onto other tasks.
#[derive(Debug)]
pub struct HarnessInner {
    clock: Arc<dyn Clock>,
}

impl HarnessInner {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }
}

/// The runtime handle threaded into `main(harness: Harness)`.
///
/// Cheap to clone; sub-handles share the same `Arc` inner state.
#[derive(Debug, Clone)]
pub struct Harness {
    inner: Arc<HarnessInner>,
}

impl Harness {
    /// Build the production handle wired to wall-clock time. Filesystem,
    /// environment, randomness, and network access are layered on by the
    /// E4.2-E4.4 migration tickets; the constructor only needs to succeed
    /// without panicking today (per the E4.1 exit criteria).
    pub fn real() -> Self {
        Self::with_clock(RealClock::arc())
    }

    /// Build a handle wired to a caller-supplied clock. Test bootstraps that
    /// want deterministic time without depending on the full `NullHarness`
    /// surface (E4.5) drive this directly.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(HarnessInner { clock }),
        }
    }

    /// Field access for `harness.stdio`.
    pub fn stdio(&self) -> HarnessStdio {
        HarnessStdio {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.clock`.
    pub fn clock(&self) -> HarnessClock {
        HarnessClock {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.fs`.
    pub fn fs(&self) -> HarnessFs {
        HarnessFs {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.env`.
    pub fn env(&self) -> HarnessEnv {
        HarnessEnv {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.random`.
    pub fn random(&self) -> HarnessRandom {
        HarnessRandom {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.net`.
    pub fn net(&self) -> HarnessNet {
        HarnessNet {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Lower this handle into the `VmValue::Harness` payload.
    pub fn into_vm_value(self) -> crate::value::VmValue {
        crate::value::VmValue::Harness(VmHarness {
            inner: self.inner,
            kind: HarnessKind::Root,
        })
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::real()
    }
}

/// stdio sub-handle: `println`, `eprintln`, `prompt`, `read_line`.
#[derive(Debug, Clone)]
pub struct HarnessStdio {
    inner: Arc<HarnessInner>,
}

/// clock sub-handle: `now`, `monotonic_now`, `sleep`.
#[derive(Debug, Clone)]
pub struct HarnessClock {
    inner: Arc<HarnessInner>,
}

impl HarnessClock {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        self.inner.clock()
    }
}

/// fs sub-handle: `read_file`, `write_file`, `exists`, `list_dir`,
/// `delete_file`, ...
#[derive(Debug, Clone)]
pub struct HarnessFs {
    inner: Arc<HarnessInner>,
}

/// env sub-handle: `get`, `set`, `vars`.
#[derive(Debug, Clone)]
pub struct HarnessEnv {
    inner: Arc<HarnessInner>,
}

/// random sub-handle: `gen_u64`, `gen_range`, `gen_f64`, ...
#[derive(Debug, Clone)]
pub struct HarnessRandom {
    inner: Arc<HarnessInner>,
}

/// net sub-handle: `http_get`, `http_post`, ...
#[derive(Debug, Clone)]
pub struct HarnessNet {
    inner: Arc<HarnessInner>,
}

macro_rules! sub_handle_inner {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $ty {
                #[allow(dead_code)]
                pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
                    &self.inner
                }
            }
        )*
    };
}
sub_handle_inner!(
    HarnessStdio,
    HarnessFs,
    HarnessEnv,
    HarnessRandom,
    HarnessNet,
);

impl HarnessClock {
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }
}

/// Compact `VmValue` payload for a `Harness` or any of its sub-handles.
///
/// All seven variants share one `Arc<HarnessInner>`; `kind` discriminates the
/// surface the VM exposes for property access and method dispatch.
#[derive(Clone)]
pub struct VmHarness {
    inner: Arc<HarnessInner>,
    kind: HarnessKind,
}

impl VmHarness {
    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    pub fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }

    /// Get the sub-handle reached by a field name (`stdio`, `clock`, etc.).
    /// Returns `None` when the receiver is itself a sub-handle or the field
    /// is unknown.
    pub fn sub_handle(&self, field: &str) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root {
            return None;
        }
        let kind = HarnessKind::from_field_name(field)?;
        Some(VmHarness {
            inner: Arc::clone(&self.inner),
            kind,
        })
    }
}

impl fmt::Debug for VmHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmHarness")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_constructs_without_panic() {
        let _harness = Harness::real();
    }

    #[test]
    fn sub_handles_share_inner_state() {
        let harness = Harness::real();
        let stdio_inner = Arc::as_ptr(harness.stdio().inner());
        let clock_inner = Arc::as_ptr(harness.clock().inner());
        assert_eq!(stdio_inner, clock_inner, "sub-handles share Arc<Inner>");
    }

    #[test]
    fn kinds_round_trip_through_field_names() {
        for kind in HarnessKind::SUB_HANDLES {
            let field = kind.field_name().unwrap();
            assert_eq!(HarnessKind::from_field_name(field), Some(*kind));
        }
        assert!(HarnessKind::from_field_name("nope").is_none());
        assert!(HarnessKind::Root.field_name().is_none());
    }

    #[test]
    fn vm_harness_property_access_returns_sub_handle() {
        let root = match Harness::real().into_vm_value() {
            crate::value::VmValue::Harness(h) => h,
            other => panic!("expected Harness variant, got {}", other.type_name()),
        };
        let stdio = root.sub_handle("stdio").expect("stdio sub-handle");
        assert_eq!(stdio.kind(), HarnessKind::Stdio);
        assert!(stdio.sub_handle("clock").is_none(), "nested access denied");
        assert!(root.sub_handle("not_a_field").is_none());
    }
}
