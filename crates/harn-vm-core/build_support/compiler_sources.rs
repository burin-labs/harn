/// Local compiler, artifact-validation, and derived-interface inputs.
/// Dependency contributions are supplied by their compiled crate owners.
pub const CORE_COMPILER_PATHS: &[&str] = &[
    "build.rs",
    "build_support/compiler_sources.rs",
    "src/chunk",
    "src/bytecode_cache",
    "src/module_artifact",
    "src/schema",
    "src/bytecode_cache.rs",
    "src/chunk.rs",
    "src/compiler.rs",
    "src/context_manifest.rs",
    "src/module_artifact.rs",
    "src/module_source.rs",
    "src/source_compile.rs",
    "src/typecheck.rs",
    "src/vm/builtin_manifest.rs",
];
