#[path = "../build_support/compiler_sources.rs"]
mod compiler_sources;

#[test]
fn compiled_compiler_contribution_matches_its_owned_inputs() {
    let actual = harn_source_fingerprint_build::fingerprint(
        "harn-vm-core",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        compiler_sources::CORE_COMPILER_PATHS,
    )
    .expect("all compiler-owned inputs are readable");
    assert_eq!(
        actual.as_str(),
        env!("HARN_CORE_COMPILER_SOURCE_FINGERPRINT")
    );
    assert_ne!(
        actual.as_str(),
        harn_vm_core::bytecode_cache::CODEGEN_FINGERPRINT
    );
}
