use std::path::Path;

/// Manifest-derived authority shared by every compilation product for one
/// source file.
///
/// Resolving this once keeps the typechecker, entry chunk, and importable
/// module on the same authority boundary.
#[derive(Clone, Copy)]
pub(crate) struct SourceCompilerAuthority {
    trusted_host_dispatch: bool,
}

impl SourceCompilerAuthority {
    pub(crate) fn for_source(path: &Path) -> Self {
        Self {
            trusted_host_dispatch: trusted_host_dispatch_for_source(path),
        }
    }

    pub(crate) fn typechecker(self) -> harn_parser::TypeChecker {
        harn_parser::TypeChecker::new().with_privileged_wire_builtins(self.trusted_host_dispatch)
    }

    /// The authority a module compiled under this projection carries.
    ///
    /// Precompiled artifacts are written next to their source, where they are
    /// offered to whoever imports that file. Stamping the authority into their
    /// cache identity is what stops an ordinary import from accepting one
    /// compiled with privileged-wire access.
    pub(crate) fn module_provenance(self) -> harn_vm::module_artifact::ModuleProvenance {
        if self.trusted_host_dispatch {
            harn_vm::module_artifact::ModuleProvenance::TrustedHostDispatch
        } else {
            harn_vm::module_artifact::ModuleProvenance::User
        }
    }

    pub(crate) fn compiler_with_imported_enums(
        self,
        candidates: impl IntoIterator<Item = String>,
    ) -> harn_vm::Compiler {
        let compiler = if self.trusted_host_dispatch {
            harn_vm::Compiler::new_trusted_host_dispatch()
        } else {
            harn_vm::Compiler::new()
        };
        compiler.with_imported_enum_candidates(candidates)
    }

    pub(crate) fn compiler_with_imported_symbols(
        self,
        enum_candidates: impl IntoIterator<Item = String>,
        callable_names: impl IntoIterator<Item = String>,
    ) -> harn_vm::Compiler {
        self.compiler_with_imported_enums(enum_candidates)
            .with_imported_source_callable_names(callable_names)
    }

    pub(crate) fn compile_module_with_imported_symbols(
        self,
        source_path: &Path,
        source: &str,
        context: &harn_vm::module_artifact::ModuleCompilationContext,
    ) -> Result<harn_vm::module_artifact::ModuleArtifact, harn_vm::VmError> {
        if self.trusted_host_dispatch {
            harn_vm::module_artifact::compile_trusted_host_dispatch_module_artifact_from_source_with_context(
                source_path,
                source,
                context,
            )
        } else {
            harn_vm::module_artifact::compile_module_artifact_from_source_with_context(
                source_path,
                source,
                context,
            )
        }
    }
}

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

/// Build a compiler with the source file's imported enum names seeded from
/// the module graph. Enum constructor syntax is resolved during bytecode
/// lowering, so every file-backed CLI compile must use the same export
/// contract as typechecking and module-artifact compilation.
pub(crate) fn compiler_for_source(path: &Path, source: &str) -> harn_vm::Compiler {
    let imported = imported_symbols_for_source(path, source);
    SourceCompilerAuthority::for_source(path).compiler_with_imported_symbols(
        imported.enum_candidates().iter().cloned(),
        imported.source_callable_names().iter().cloned(),
    )
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

/// Read the project's own `[check].trusted_host_dispatch` declaration for the
/// manifest that owns `path`.
///
/// `harn check`, `harn lint`, and `harn test` all read this key. `harn run` did
/// not, and it has no CLI flag to compensate, so a project that declared the
/// authority in its manifest could still be refused every `host_call` the
/// moment it ran a script — including the manifest's own trigger handlers,
/// which install before the script body executes and fail the whole run. That
/// left the key meaning one thing to three commands and nothing to the command
/// most likely to execute the code.
pub(crate) fn trusted_host_dispatch_for_source(path: &Path) -> bool {
    // Walk up from the absolute path. `harn run scripts/main.harn` hands us a
    // relative path whose ancestors run out at `scripts/`, so the repo-root
    // manifest that declares the authority is never reached and the key reads
    // as absent from exactly the invocation people actually type.
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(path));
    crate::package::load_check_config(Some(&absolute)).trusted_host_dispatch
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

/// Build a compiler from an already-resolved graph projection. This avoids a
/// second graph walk in commands such as `harn check`, whose analysis has
/// already loaded the complete import closure.
pub(crate) fn compiler_with_imported_enum_candidates(
    candidates: impl IntoIterator<Item = String>,
) -> harn_vm::Compiler {
    harn_vm::Compiler::new().with_imported_enum_candidates(candidates)
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
