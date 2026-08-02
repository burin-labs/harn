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
        EffectKind, EffectSpec, Param, ResourceSelector, ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL,
        TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT,
        TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER, TY_NIL, TY_NUMBER, TY_RESOURCE, TY_STRING,
        TY_STRING_OR_NIL,
    };
}

use harn_builtin_macros::harn_capability_contract as capability_method;

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
            let mut defs = ALL_CAPABILITY_METHOD_DEFS
                .iter()
                .copied()
                .collect::<Vec<_>>();
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
