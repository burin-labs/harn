//! Canonical typed contracts for every `Harness` capability method.
//!
//! This dependency-leaf crate owns the method names, signatures, effects, and
//! documentation shared by parser, IR, runtime policy, receipts, and tooling.
//! Consumers read the immutable manifest directly; correctness never depends
//! on a VM having initialized a process-global registry first.

use std::sync::OnceLock;

use harn_builtin_meta::{BuiltinContract, BuiltinSignature};
use harn_builtin_registry::BuiltinManifestEntry;

/// One method contract before its name-keyed manifest projection.
#[derive(Debug, Clone, Copy)]
pub struct CapabilityMethodDef {
    pub signature: BuiltinSignature,
    pub contract: BuiltinContract,
    pub doc: &'static str,
    pub signature_text: Option<&'static str>,
}

/// Build-generated projection of the source declarations. A plain static
/// slice works on native and `wasm32-unknown-unknown`, unlike linker-section
/// discovery, while the macro declarations remain the single contract owner.
pub static ALL_CAPABILITY_METHOD_DEFS: &[&CapabilityMethodDef] =
    include!(concat!(env!("OUT_DIR"), "/capability_method_defs.rs"));

/// Proc-macro support paths kept in one deliberately boring module.
#[doc(hidden)]
pub mod support {
    pub use crate::{CapabilityMethodDef, ALL_CAPABILITY_METHOD_DEFS};
    pub use harn_builtin_meta::{
        shapes, BuiltinContract, BuiltinExposure, BuiltinSignature, CapabilityId, EffectAccess,
        EffectAuthorization, EffectKind, EffectSpec, Param, ResourceSelector, ShapeFieldDescriptor,
        Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL,
        TY_DURATION, TY_FLOAT, TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER, TY_NIL, TY_NUMBER,
        TY_RESOURCE, TY_STRING, TY_STRING_OR_NIL,
    };
}

use harn_builtin_macros::harn_capability_contract as capability_method;

mod vm_declared;

include!("ai.rs");
include!("data.rs");
include!("host.rs");
include!("io.rs");

/// Deterministic manifest projection consumed directly by every compiler and
/// runtime surface.
pub fn manifest() -> &'static [&'static BuiltinManifestEntry] {
    static MANIFEST: OnceLock<Vec<&'static BuiltinManifestEntry>> = OnceLock::new();
    MANIFEST
        .get_or_init(|| {
            let mut defs = ALL_CAPABILITY_METHOD_DEFS.to_vec();
            defs.sort_by_key(|def| def.signature.name);
            defs.into_iter()
                .map(|def| {
                    Box::leak(Box::new(BuiltinManifestEntry {
                        name: def.signature.name,
                        canonical_name: def.signature.name,
                        signature: &def.signature,
                        contract: def.contract,
                    })) as &'static BuiltinManifestEntry
                })
                .collect()
        })
        .as_slice()
}

/// Resolve one `harness.<field>.<method>` contract without mutable registry
/// initialization.
pub fn capability_method_entry(field: &str, method: &str) -> Option<&'static BuiltinManifestEntry> {
    let capability = harn_builtin_meta::CapabilityId::from_field_name(field)?;
    manifest().iter().copied().find(|entry| {
        matches!(
            entry.contract.exposure,
            harn_builtin_meta::BuiltinExposure::HarnessMethod {
                capability: candidate,
                method: candidate_method,
            } if candidate == capability && candidate_method == method
        )
    })
}

/// Is `harness.<field>.<method>` a declared capability method anywhere in the
/// workspace?
///
/// Weaker than [`capability_method_entry`] on purpose: it answers existence
/// without a contract, because the 280 methods `harn-vm` declares through
/// `#[harn_builtin]` have no leaf-crate contract to return. Their bodies close
/// over VM internals, so the declaration cannot move here — only its name can.
///
/// A consumer that needs the signature must still go through the installed
/// manifest and accept that it is empty before the VM installs it. A consumer
/// that only needs to reject a typo — `harn check` — can use this, and get the
/// same answer whether or not a VM ever starts (#6101).
#[must_use]
pub fn is_declared_capability_method(field: &str, method: &str) -> bool {
    if capability_method_entry(field, method).is_some() {
        return true;
    }
    if vm_declared::VM_DECLARED_CAPABILITY_METHODS
        .binary_search(&(field, method))
        .is_ok()
    {
        return true;
    }
    // The third registry. A host-bridged method has no builtin at all — the VM
    // routes it to `host_call`, and the implementation lives in the embedder.
    // `harness.workspace.search` is real for a host that serves it and reaches
    // no declaration in either table above.
    harn_builtin_meta::CapabilityId::from_field_name(field).is_some_and(|capability| {
        harn_builtin_meta::host_capabilities::is_host_capability_method(capability, method)
    })
}

/// Every method name declared on one capability, contract-owned and
/// `harn-vm`-owned together, sorted and de-duplicated.
///
/// Used to suggest a near miss when [`is_declared_capability_method`] rejects
/// a name.
#[must_use]
pub fn declared_capability_method_names(field: &str) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = vm_declared::VM_DECLARED_CAPABILITY_METHODS
        .iter()
        .filter(|(capability, _)| *capability == field)
        .map(|(_, method)| *method)
        .collect();
    if let Some(capability) = harn_builtin_meta::CapabilityId::from_field_name(field) {
        names.extend(
            manifest()
                .iter()
                .filter_map(|entry| match entry.contract.exposure {
                    harn_builtin_meta::BuiltinExposure::HarnessMethod {
                        capability: candidate,
                        method,
                    } if candidate == capability => Some(method),
                    _ => None,
                }),
        );
        names.extend(
            harn_builtin_meta::host_capabilities::all_host_capability_groups()
                .filter(|group| group.capability == capability)
                .flat_map(|group| group.methods.iter().copied()),
        );
    }
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    /// The generated table is sorted, which `is_declared_capability_method`
    /// binary-searches. A generator change that lost the ordering would make
    /// lookups miss silently rather than fail.
    #[test]
    fn vm_declared_methods_are_sorted_and_unique() {
        let table = super::vm_declared::VM_DECLARED_CAPABILITY_METHODS;
        assert!(!table.is_empty());
        assert!(
            table.windows(2).all(|pair| pair[0] < pair[1]),
            "the generated table must be sorted and free of duplicates"
        );
    }

    /// The motivating pair from #6101: real, VM-owned, and invisible to the
    /// static manifest.
    #[test]
    fn a_vm_only_method_is_declared_without_the_vm() {
        assert!(super::capability_method_entry("runtime", "shared_cell").is_none());
        assert!(super::is_declared_capability_method(
            "runtime",
            "shared_cell"
        ));
    }

    #[test]
    fn a_contract_owned_method_is_declared() {
        assert!(super::is_declared_capability_method("fs", "read_text"));
    }

    /// The third registry. `harness.workspace.search` has no builtin at all:
    /// the VM routes it to `host_call`, and a host serves it. Reading only the
    /// contract manifest and the generated `harn-vm` projection reported it as
    /// a typo, which broke the embedder-bridge tests.
    #[test]
    fn a_host_bridged_method_is_declared() {
        assert!(super::capability_method_entry("workspace", "search").is_none());
        assert!(super::is_declared_capability_method("workspace", "search"));
        assert!(super::declared_capability_method_names("workspace").contains(&"search"));
    }

    #[test]
    fn a_typo_is_not_declared() {
        assert!(!super::is_declared_capability_method("fs", "bogus_method"));
        assert!(!super::is_declared_capability_method(
            "runtime",
            "shared_cel"
        ));
        assert!(!super::is_declared_capability_method(
            "workspace",
            "searchh"
        ));
    }
}
