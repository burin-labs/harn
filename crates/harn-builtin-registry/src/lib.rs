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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock, RwLock};

use harn_builtin_meta::{BuiltinContract, BuiltinExposure, BuiltinSignature, CapabilityId};

/// Crate-local source contribution to compiler and check cache identities.
pub const SOURCE_FINGERPRINT: &str = env!("HARN_SOURCE_FINGERPRINT");

/// One name-keyed projection of a macro-emitted builtin definition. Aliases
/// receive their own entry so signature and contract can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinManifestEntry {
    pub name: &'static str,
    /// The builtin's primary name. Equal to [`Self::name`] except on an alias
    /// entry, which repeats the primary's signature and contract under a second
    /// name. Projections keyed by something other than the name — a capability
    /// method, say — must use only the entry where these two agree, or they
    /// will see one builtin as several.
    pub canonical_name: &'static str,
    pub signature: &'static BuiltinSignature,
    pub contract: BuiltinContract,
}

impl BuiltinManifestEntry {
    /// Whether this entry is the builtin's primary name rather than an alias.
    pub fn is_canonical(&self) -> bool {
        self.name == self.canonical_name
    }
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

/// The installed manifest and the indexes derived from it. Installs are rare
/// (startup), while lookups run for every call site the type checker visits,
/// so every projection is rebuilt on install and read without copying.
#[derive(Default)]
struct Registry {
    by_name: BTreeMap<&'static str, &'static BuiltinManifestEntry>,
    /// Every entry in name order, shared with whole-manifest readers.
    ordered: Arc<[&'static BuiltinManifestEntry]>,
    /// The first entry in name order exposed as each
    /// `harness.<capability>.<method>`.
    harness_methods: HashMap<CapabilityId, HashMap<&'static str, &'static BuiltinManifestEntry>>,
    /// Manifest slices already installed, by address and length. Drivers
    /// re-install the same static slice once per checked file, so a repeat
    /// install must not take the write lock or compare every entry.
    installed_slices: Vec<(usize, usize)>,
}

impl Registry {
    fn reindex(&mut self) {
        self.ordered = self.by_name.values().copied().collect();
        self.harness_methods.clear();
        for entry in self.ordered.iter().copied() {
            if let BuiltinExposure::HarnessMethod { capability, method } = entry.contract.exposure {
                self.harness_methods
                    .entry(capability)
                    .or_default()
                    .entry(method)
                    .or_insert(entry);
            }
        }
    }
}

static INSTALLED: OnceLock<RwLock<Registry>> = OnceLock::new();

fn installed() -> &'static RwLock<Registry> {
    INSTALLED.get_or_init(|| RwLock::new(Registry::default()))
}

/// Install the process-global builtin manifest.
///
/// # Panics
/// Multiple owners may contribute disjoint manifest fragments (for example
/// the portable kernel and a native host). Installing structurally identical
/// projections is idempotent; redefining an existing name panics.
pub fn install_builtin_manifest(entries: &'static [&'static BuiltinManifestEntry]) {
    let slice = (entries.as_ptr() as usize, entries.len());
    if installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .installed_slices
        .contains(&slice)
    {
        return;
    }
    let mut registry = installed().write().expect("builtin manifest lock poisoned");
    if registry.installed_slices.contains(&slice) {
        return;
    }
    let mut changed = false;
    for entry in entries {
        if let Some(previous) = registry.by_name.get(entry.name) {
            assert!(
                *previous == *entry,
                "builtin manifest name `{}` registered by multiple contracts",
                entry.name
            );
        } else {
            registry.by_name.insert(entry.name, entry);
            changed = true;
        }
    }
    registry.installed_slices.push(slice);
    if changed {
        registry.reindex();
    }
}

/// Test-only one-shot manifest install.
#[doc(hidden)]
pub fn _test_only_reinstall(entries: &'static [&'static BuiltinManifestEntry]) {
    install_builtin_manifest(entries);
}

/// Read the installed manifest in name order. The slice is shared, so reading
/// it costs one reference-count increment rather than a copy of the manifest.
pub fn installed_manifest() -> Arc<[&'static BuiltinManifestEntry]> {
    Arc::clone(
        &installed()
            .read()
            .expect("builtin manifest lock poisoned")
            .ordered,
    )
}

/// Resolve one installed manifest entry.
pub fn builtin_entry(name: &str) -> Option<&'static BuiltinManifestEntry> {
    installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .by_name
        .get(name)
        .copied()
}

/// Resolve the installed entry exposed as `harness.<capability>.<method>`.
/// When several entries share that exposure, the first in name order owns it.
pub fn harness_method_entry(
    capability: CapabilityId,
    method: &str,
) -> Option<&'static BuiltinManifestEntry> {
    installed()
        .read()
        .expect("builtin manifest lock poisoned")
        .harness_methods
        .get(&capability)?
        .get(method)
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
        .by_name
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_builtin_meta::{Param, Ty};

    const SIGNATURE_A: BuiltinSignature = BuiltinSignature::simple(
        "__registry_structural_idempotence",
        &[Param::new("value", Ty::Named("string"))],
        Ty::Named("string"),
    );
    const SIGNATURE_B: BuiltinSignature = SIGNATURE_A;
    static ENTRY_A: BuiltinManifestEntry = BuiltinManifestEntry {
        name: "__registry_structural_idempotence",
        canonical_name: "__registry_structural_idempotence",
        signature: &SIGNATURE_A,
        contract: BuiltinContract::PURE,
    };
    static ENTRY_B: BuiltinManifestEntry = BuiltinManifestEntry {
        name: "__registry_structural_idempotence",
        canonical_name: "__registry_structural_idempotence",
        signature: &SIGNATURE_B,
        contract: BuiltinContract::PURE,
    };
    static MANIFEST_A: &[&BuiltinManifestEntry] = &[&ENTRY_A];
    static MANIFEST_B: &[&BuiltinManifestEntry] = &[&ENTRY_B];

    const HARNESS_METHOD: BuiltinContract =
        BuiltinContract::harness(CapabilityId::Stdio, "__registry_index_probe", &[]);
    static METHOD_LATER: BuiltinManifestEntry = BuiltinManifestEntry {
        name: "__registry_index_probe_b",
        canonical_name: "__registry_index_probe_b",
        signature: &SIGNATURE_A,
        contract: HARNESS_METHOD,
    };
    static METHOD_FIRST: BuiltinManifestEntry = BuiltinManifestEntry {
        name: "__registry_index_probe_a",
        canonical_name: "__registry_index_probe_a",
        signature: &SIGNATURE_A,
        contract: HARNESS_METHOD,
    };
    static METHOD_MANIFEST: &[&BuiltinManifestEntry] = &[&METHOD_LATER, &METHOD_FIRST];

    #[test]
    fn harness_method_index_agrees_with_a_name_ordered_scan() {
        install_builtin_manifest(METHOD_MANIFEST);
        let scanned = installed_manifest()
            .iter()
            .copied()
            .find(|entry| entry.contract.exposure == HARNESS_METHOD.exposure);
        let indexed = harness_method_entry(CapabilityId::Stdio, "__registry_index_probe");

        assert_eq!(indexed.map(|entry| entry.name), Some(METHOD_FIRST.name));
        assert_eq!(
            indexed.map(|entry| entry.name),
            scanned.map(|entry| entry.name)
        );
        assert!(harness_method_entry(CapabilityId::Clock, "__registry_index_probe").is_none());
    }

    #[test]
    fn structurally_identical_projections_are_idempotent() {
        install_builtin_manifest(MANIFEST_A);
        install_builtin_manifest(MANIFEST_B);
        assert!(std::ptr::eq(
            builtin_entry(ENTRY_A.name).unwrap(),
            &raw const ENTRY_A
        ));
    }
}
