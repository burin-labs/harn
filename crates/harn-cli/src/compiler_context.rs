use std::path::Path;

pub(crate) use harn_package::precompile::{
    trusted_host_dispatch_for_source, SourceCompilerAuthority,
};

/// Install the macro-emitted builtin signature slice into the
/// `harn_parser` registry the first time any harn-cli entry point parses
/// or typechecks a script.
///
/// Every code path that drives the parser — `run()`, `execute_run()`,
/// `parse_source_file()`, `analyze_file()`, every test harness — funnels
/// through this single helper so the registry is always populated by the
/// time the typechecker reads it. `install_builtin_manifest` is
/// idempotent on identical `&'static` slices, so repeat calls are
/// cheap (a `OnceLock::set` that no-ops after the first success).
///
/// Tests cannot rely on `run()` having executed, so they must reach the
/// parser via one of these entry points (which always do call this).
pub(crate) fn ensure_builtin_signatures_installed() {
    harn_parser::install_builtin_manifest(harn_vm::stdlib::all_builtin_manifest());
}

/// Build a compiler from the source file's resolved import interface. Every
/// name consulted during lowering uses the same export contract as
/// typechecking, module-artifact compilation, and cached execution.
pub(crate) fn compiler_for_source(path: &Path, source: &str) -> harn_vm::Compiler {
    let imported = imported_symbols_for_source(path, source);
    SourceCompilerAuthority::for_source(path).compiler_with_imported_symbols(&imported)
}

/// Build the same import-aware compiler without consulting ambient project
/// authority. Standalone execution uses this path so a nearby manifest cannot
/// elevate an otherwise identical script or its cached entry chunk.
pub(crate) fn compiler_for_standalone_source(path: &Path, source: &str) -> harn_vm::Compiler {
    let graph = harn_modules::build_with_standalone_source(path, source);
    let imported = harn_vm::module_artifact::ModuleCompilationContext::for_source_in_graph(
        &graph, path, source,
    )
    .unwrap_or_default();
    SourceCompilerAuthority::standalone().compiler_with_imported_symbols(&imported)
}

pub(crate) fn imported_symbols_for_source(
    path: &Path,
    source: &str,
) -> harn_vm::module_artifact::ModuleCompilationContext {
    let graph = harn_modules::build_with_source(path, source);
    harn_vm::module_artifact::ModuleCompilationContext::for_source_in_graph(&graph, path, source)
        // This projection feeds compiler construction, whose canonical parse owns
        // syntax diagnostics. Invalid source cannot produce or cache an artifact;
        // defaulting here preserves that diagnostic instead of introducing a
        // second fallible parse boundary. Successfully parsed precompile inputs
        // therefore never take this branch.
        .unwrap_or_default()
}

/// Apply the source project's trusted host-dispatch declaration to a fresh VM.
///
/// File-backed CLI entry points must call this before loading any module. Keeping
/// the manifest lookup and VM transition together prevents `run`, source
/// execution, and ACP from silently assigning different authority to the same
/// project.
pub(crate) fn enable_trusted_host_dispatch_for_source(
    vm: &mut harn_vm::Vm,
    path: &Path,
) -> Result<(), harn_vm::VmError> {
    if !trusted_host_dispatch_for_source(path) {
        return Ok(());
    }
    vm.enable_trusted_host_dispatch()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority_fixture(manifest: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf) {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::create_dir(project.path().join(".git")).expect("project boundary");
        if let Some(manifest) = manifest {
            std::fs::write(project.path().join("harn.toml"), manifest).expect("manifest fixture");
        }
        let source = project.path().join("main.harn");
        std::fs::write(&source, "pipeline main(harness: Harness) {}\n").expect("source fixture");
        (project, source)
    }

    #[test]
    fn manifest_authority_boundary_allows_only_an_explicit_valid_declaration() {
        let cases = [
            (
                "allowed",
                Some("[check]\ntrusted_host_dispatch = true\n"),
                true,
            ),
            (
                "denied",
                Some("[check]\ntrusted_host_dispatch = false\n"),
                false,
            ),
            ("missing", None, false),
            (
                "malformed",
                Some("[check\ntrusted_host_dispatch = true\n"),
                false,
            ),
        ];

        for (case, manifest, expected) in cases {
            let (_project, source) = authority_fixture(manifest);
            assert_eq!(
                trusted_host_dispatch_for_source(&source),
                expected,
                "{case} manifest authority decision"
            );
        }
    }
}
