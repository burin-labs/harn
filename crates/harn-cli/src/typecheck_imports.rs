//! Shared import-graph resolution for the CLI's type-check entry points.
//!
//! `harn run`, `precompile`, `bench`, and `counterfactual` each type-check
//! a single file but must resolve that file's imports first — otherwise a
//! call to an imported symbol is checked against nothing (or, worse, a
//! same-named builtin). Factoring the resolution here keeps every entry
//! point consistent with `execute`/`harn check`.
//!
//! The same resolved import declarations feed the compiler: the importing
//! module must be able to construct and `match` an imported enum's variants,
//! which requires the compiler to know the imported enum catalog at
//! compile time (harn#5203). `configure_compiler_with_graph` mirrors
//! `configure_checker_with_graph` so the checker and compiler agree on which
//! imported types are visible.

use std::path::Path;

use harn_parser::TypeChecker;
use harn_vm::Compiler;

/// Configure `checker` from an already-built `graph` for the module at `path`.
pub(crate) fn configure_checker_with_graph(
    mut checker: TypeChecker,
    graph: &harn_modules::ModuleGraph,
    path: &Path,
) -> TypeChecker {
    if let Some(imported) = graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker
}

/// Configure `compiler` from an already-built `graph` for the module at `path`,
/// so references to an imported enum's variants lower against a known enum
/// instead of a bare variable load. Uses the same imported type declarations
/// the checker receives, keeping check-time and run-time resolution aligned.
pub(crate) fn configure_compiler_with_graph(
    compiler: Compiler,
    graph: &harn_modules::ModuleGraph,
    path: &Path,
) -> Compiler {
    match graph.imported_type_declarations_for_file(path) {
        Some(imported) => compiler.with_imported_type_decls(imported),
        None => compiler,
    }
}

/// Configure `checker` with the resolved imports of the module rooted at
/// `path`, so a call to an imported symbol is checked against its real
/// signature (and an imported name shadows a same-named builtin).
pub(crate) fn checker_with_resolved_imports(checker: TypeChecker, path: &Path) -> TypeChecker {
    let graph = harn_modules::build(&[path.to_path_buf()]);
    configure_checker_with_graph(checker, &graph, path)
}

/// Configure `compiler` with the resolved imports of the module rooted at
/// `path`. The compiler analogue of [`checker_with_resolved_imports`].
pub(crate) fn compiler_with_resolved_imports(compiler: Compiler, path: &Path) -> Compiler {
    let graph = harn_modules::build(&[path.to_path_buf()]);
    configure_compiler_with_graph(compiler, &graph, path)
}
