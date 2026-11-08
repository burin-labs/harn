//! Compile embedded stdlib artifacts using declarations rather than native handlers.
//!
//! Builtin attributes remain authoritative in their implementation crates.
//! `make gen-vm-exposures` projects those declarations here with the original
//! feature and platform conditions. No provider, process, or native runtime
//! implementation is needed to compile bytecode.

mod declarations;

pub use harn_vm_core::{CompiledStdlibArtifact, StdlibArtifactBuild};

/// Compile the selected stdlib and builtin contract into immutable artifacts.
pub fn compile(threads: usize) -> StdlibArtifactBuild {
    declarations::force_link();
    harn_stdlib::install().expect("install the stdlib this compiler builds");
    harn_vm_core::compile_embedded_stdlib_artifacts(threads)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    #[test]
    fn projected_declarations_equal_the_selected_native_contract() {
        harn_runtime::force_link();
        let native_definitions = harn_vm::all_builtin_defs();
        let projected_definitions = super::declarations::definitions();
        let native = native_definitions
            .iter()
            .map(|definition| (definition.sig.name, *definition))
            .collect::<BTreeMap<_, _>>();
        let projected = projected_definitions
            .iter()
            .copied()
            .map(|definition| (definition.sig.name, definition))
            .collect::<BTreeMap<_, _>>();
        assert!(native.len() > 500, "the native contract was not linked");
        assert_eq!(
            native.len(),
            native_definitions.len(),
            "duplicate native name"
        );
        assert_eq!(
            projected.len(),
            projected_definitions.len(),
            "duplicate projected name"
        );
        assert_eq!(
            native.keys().collect::<Vec<_>>(),
            projected.keys().collect::<Vec<_>>()
        );
        for (name, actual) in native {
            let expected = projected[name];
            assert_eq!(actual.sig, expected.sig, "signature for {name}");
            assert_eq!(actual.contract, expected.contract, "authority for {name}");
            assert_eq!(actual.aliases, expected.aliases, "aliases for {name}");
            assert_eq!(
                actual.parser_only, expected.parser_only,
                "parser flag for {name}"
            );
            assert_eq!(
                actual.runtime_only, expected.runtime_only,
                "runtime flag for {name}"
            );
            assert_eq!(actual.category, expected.category, "category for {name}");
            assert_eq!(actual.doc, expected.doc, "documentation for {name}");
            assert_eq!(
                actual.signature_text, expected.signature_text,
                "signature text for {name}"
            );
        }
    }
}
