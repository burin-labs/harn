use std::path::Path;

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
    let base = if trusted_host_dispatch_for_source(path) {
        harn_vm::Compiler::new_trusted_host_dispatch()
    } else {
        harn_vm::Compiler::new()
    };
    base.with_imported_enum_candidates(imported_enum_candidates_for_source(path, source))
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
