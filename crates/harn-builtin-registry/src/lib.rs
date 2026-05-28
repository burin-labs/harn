//! Process-global registry of builtin signatures.
//!
//! `harn-vm` owns the implementations and emits one `&'static BuiltinDef<H>`
//! per `#[harn_builtin]`-annotated function via the `harn-builtin-macros`
//! crate. At startup the driver (CLI, LSP, lint, serve, dap, tests) installs
//! the full slice of signatures here; the parser/typechecker then reads them
//! through [`installed_signatures`].
//!
//! This decouples `harn-parser` (which needs to see signatures to typecheck)
//! from `harn-vm` (which owns the impls) without a dependency cycle —
//! `harn-parser` depends only on this crate plus `harn-builtin-meta`, never
//! on the vm.

use std::sync::OnceLock;

use harn_builtin_meta::BuiltinSignature;

/// A complete description of one builtin: its signature, its aliases, the
/// runtime handler (typed by the consumer via `H`), and optional metadata.
///
/// `H` is parametric so this crate stays free of any handler-type
/// dependency. `harn-vm` instantiates it as
/// `BuiltinDef<VmBuiltinHandler>`; parser-only consumers ignore the handler
/// and read just the [`Self::sig`] field.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinDef<H: 'static> {
    /// Static signature consumed by the parser/typechecker.
    pub sig: BuiltinSignature,
    /// Additional names that share this impl + signature. Each alias gets
    /// its own [`BuiltinSignature`] entry at install time (with the same
    /// param/return types) so the typechecker accepts both.
    pub aliases: &'static [&'static str],
    /// Runtime handler (sync fn, async fn, or `None` for parser-only
    /// builtins). Type is opaque to this crate.
    pub handler: H,
    /// Free-form category label used for metadata/observability.
    pub category: Option<&'static str>,
    /// Human-readable doc, typically the leading `///` block from the impl
    /// function. Surfaced to LSP hover and `harn explain`.
    pub doc: Option<&'static str>,
    /// Free-form Harn-style signature text (e.g. `"foo(a: dict) -> dict"`).
    /// Populated by `#[harn_builtin]` from the `sig = "..."` literal so the
    /// runtime metadata layer can surface the original source spelling
    /// without re-rendering [`Self::sig`]. The DSL builder shape used to
    /// store this via `.signature(...)`; the macro shape replaces it.
    pub signature_text: Option<&'static str>,
    /// Set to `true` for builtins that exist in the parser registry but
    /// have no runtime entry (`len`, `split`, … — see
    /// `PARSER_ONLY_EXCEPTIONS` in the alignment test). The registry
    /// skips runtime registration for these.
    pub parser_only: bool,
    /// Set to `true` for compiler-synthesized runtime helpers (sigil
    /// prefix `__`, opcode keywords, enum constructors) that exist as VM
    /// builtins but should NOT show up in the parser signature table.
    /// The registry skips signature publishing for these.
    pub runtime_only: bool,
}

impl<H: 'static> BuiltinDef<H> {
    /// Compact constructor for the common case: one signature, no aliases,
    /// no metadata flags.
    pub const fn new(sig: BuiltinSignature, handler: H) -> Self {
        Self {
            sig,
            aliases: &[],
            handler,
            category: None,
            doc: None,
            signature_text: None,
            parser_only: false,
            runtime_only: false,
        }
    }
}

/// Process-global slice of installed signatures, populated once by the
/// driver. Reads via [`installed_signatures`] are O(1); writes via
/// [`install_builtin_signatures`] are one-shot.
static INSTALLED: OnceLock<&'static [&'static BuiltinSignature]> = OnceLock::new();

/// Install the process-global signature registry. Called once by the driver
/// (CLI, LSP, lint, serve, dap) at startup. Test harnesses that build a Vm
/// via `harn_vm::stdlib::stdlib_probe_vm()` inherit the install through that
/// helper.
///
/// # Panics
/// Panics if called more than once with different slices. Repeat calls with
/// the same pointer are tolerated (CLI + test harness can both call it).
pub fn install_builtin_signatures(sigs: &'static [&'static BuiltinSignature]) {
    if INSTALLED.set(sigs).is_err() {
        assert!(
            INSTALLED.get().copied().map(<[_]>::as_ptr) == Some(sigs.as_ptr()),
            "install_builtin_signatures called twice with different slices — \
             drivers should install once at startup"
        );
    }
}

/// Reset the installed slice — only callable from tests (`#[cfg(test)]`
/// guarded). Avoids leaking state across in-process unit tests that need
/// to swap the installed slice. **Not** for production use.
#[doc(hidden)]
pub fn _test_only_reinstall(sigs: &'static [&'static BuiltinSignature]) {
    // OnceLock has no public reset, but we can't avoid OnceLock semantics
    // here. Tests that need to reinstall should call this exactly once
    // before any other test calls `install_builtin_signatures`.
    let _ = INSTALLED.set(sigs);
}

/// Read the installed signature slice. Returns an empty slice before the
/// first call to [`install_builtin_signatures`] (e.g. in pure-parser unit
/// tests that don't need a registry).
pub fn installed_signatures() -> &'static [&'static BuiltinSignature] {
    INSTALLED.get().copied().unwrap_or(&[])
}

/// True when the registry has been populated. Useful for guards in parser
/// code that wants to assert it's running in a configured driver context.
pub fn is_installed() -> bool {
    INSTALLED.get().is_some()
}
