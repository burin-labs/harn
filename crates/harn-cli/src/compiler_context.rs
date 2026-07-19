use std::path::Path;

/// Install the macro-emitted builtin signature slice into the
/// `harn_parser` registry the first time any harn-cli entry point parses
/// or typechecks a script.
///
/// Every code path that drives the parser — `run()`, `execute_run()`,
/// `parse_source_file()`, `analyze_file()`, every test harness — funnels
/// through this single helper so the registry is always populated by the
/// time the typechecker reads it. `install_builtin_signatures` is
/// idempotent on identical `&'static` slices, so repeat calls are
/// cheap (a `OnceLock::set` that no-ops after the first success).
///
/// Tests cannot rely on `run()` having executed, so they must reach the
/// parser via one of these entry points (which always do call this).
pub(crate) fn ensure_builtin_signatures_installed() {
    harn_parser::install_builtin_signatures(harn_vm::stdlib::all_builtin_signatures());
}

/// Build a compiler with the source file's imported enum names seeded from
/// the module graph. Enum constructor syntax is resolved during bytecode
/// lowering, so every file-backed CLI compile must use the same export
/// contract as typechecking and module-artifact compilation.
pub(crate) fn compiler_for_source(path: &Path, source: &str) -> harn_vm::Compiler {
    harn_vm::Compiler::new()
        .with_imported_enum_candidates(imported_enum_candidates_for_source(path, source))
}

pub(crate) fn imported_enum_candidates_for_source(path: &Path, source: &str) -> Vec<String> {
    let mut candidates = harn_modules::build_with_source(path, source)
        .imported_names_by_kind_for_file(path, harn_modules::DefKind::Enum)
        .unwrap_or_default();
    let mut candidates = candidates.drain().collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates
}

/// Build a compiler from an already-resolved graph projection. This avoids a
/// second graph walk in commands such as `harn check`, whose analysis has
/// already loaded the complete import closure.
pub(crate) fn compiler_with_imported_enum_candidates(
    candidates: impl IntoIterator<Item = String>,
) -> harn_vm::Compiler {
    harn_vm::Compiler::new().with_imported_enum_candidates(candidates)
}
