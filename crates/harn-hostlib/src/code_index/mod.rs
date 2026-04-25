//! Code index host capability.
//!
//! Ports the deterministic trigram/word index that lived in
//! `Sources/BurinCodeIndex/` on the Swift side. Implementation lands in
//! issue B3; this scaffold registers the contract so consumers can wire
//! against the stable surface today.

use crate::registry::{BuiltinRegistry, HostlibCapability};

/// Code-index capability handle. Stateless today; will own the index actor
/// (or its async handle) once the implementation lands.
#[derive(Default)]
pub struct CodeIndexCapability;

impl HostlibCapability for CodeIndexCapability {
    fn module_name(&self) -> &'static str {
        "code_index"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_unimplemented("hostlib_code_index_query", "code_index", "query");
        registry.register_unimplemented("hostlib_code_index_rebuild", "code_index", "rebuild");
        registry.register_unimplemented("hostlib_code_index_stats", "code_index", "stats");
        registry.register_unimplemented(
            "hostlib_code_index_imports_for",
            "code_index",
            "imports_for",
        );
        registry.register_unimplemented(
            "hostlib_code_index_importers_of",
            "code_index",
            "importers_of",
        );
    }
}
