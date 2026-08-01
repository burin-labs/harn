//! Process-global registry of builtin contracts.
//!
//! `harn-vm` owns the implementations and emits one `&'static BuiltinDef<H>`
//! per `#[harn_builtin]`-annotated function via the `harn-builtin-macros`
//! crate. At startup the driver installs one immutable manifest containing
//! the signature, source exposure, and effects for every name.
//!
//! This decouples `harn-parser` (which needs to see signatures to typecheck)
//! from `harn-vm` (which owns the impls) without a dependency cycle —
//! `harn-parser` depends only on this crate plus `harn-builtin-meta`, never
//! on the vm.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use harn_builtin_meta::{BuiltinContract, BuiltinSignature};

/// One name-keyed projection of a macro-emitted builtin definition. Aliases
/// receive their own entry so signature and contract can never drift.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinManifestEntry {
    pub name: &'static str,
    pub signature: &'static BuiltinSignature,
    pub contract: BuiltinContract,
}

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
    /// Typed source exposure and effect contract. This is the semantic owner
    /// consumed by every parser/runtime/policy projection.
    pub contract: BuiltinContract,
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
            contract: BuiltinContract::UNDECLARED,
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

static INSTALLED: OnceLock<RwLock<BTreeMap<&'static str, &'static BuiltinManifestEntry>>> =
    OnceLock::new();

fn installed() -> &'static RwLock<BTreeMap<&'static str, &'static BuiltinManifestEntry>> {
    INSTALLED.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Install the process-global builtin manifest.
///
/// # Panics
/// Multiple owners may contribute disjoint manifest fragments (for example
/// the core runtime and an optional host capability crate). Reinstalling the
/// same entries is idempotent; redefining an existing name panics.
pub fn install_builtin_manifest(entries: &'static [&'static BuiltinManifestEntry]) {
    let mut manifest = installed().write().expect("builtin manifest lock poisoned");
    for entry in entries {
        if let Some(previous) = manifest.insert(entry.name, entry) {
            assert!(
                std::ptr::eq(previous, *entry),
                "builtin manifest name `{}` registered by multiple contracts",
                entry.name
            );
        }
    }
}

/// Test-only one-shot manifest install.
#[doc(hidden)]
pub fn _test_only_reinstall(entries: &'static [&'static BuiltinManifestEntry]) {
    install_builtin_manifest(entries);
}

/// Read the installed manifest.
pub fn installed_manifest() -> Vec<&'static BuiltinManifestEntry> {
    installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .values()
        .copied()
        .collect()
}

/// Resolve one installed manifest entry.
pub fn builtin_entry(name: &str) -> Option<&'static BuiltinManifestEntry> {
    installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .get(name)
        .copied()
}

/// Resolve the typed contract for one installed source name.
pub fn builtin_contract(name: &str) -> Option<&'static BuiltinContract> {
    builtin_entry(name).map(|entry| &entry.contract)
}

/// True when the registry has been populated. Useful for guards in parser
/// code that wants to assert it's running in a configured driver context.
pub fn is_installed() -> bool {
    !installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .is_empty()
}
