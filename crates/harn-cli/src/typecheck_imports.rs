//! Shared import-graph resolution for the CLI's type-check entry points.
//!
//! `harn run`, `precompile`, `bench`, and `counterfactual` each type-check
//! a single file but must resolve that file's imports first — otherwise a
//! call to an imported symbol is checked against nothing (or, worse, a
//! same-named builtin). Factoring the resolution here keeps every entry
//! point consistent with `execute`/`harn check`.

use std::path::Path;

use harn_parser::TypeChecker;

/// Configure `checker` with the resolved imports of the module rooted at
/// `path`, so a call to an imported symbol is checked against its real
/// signature (and an imported name shadows a same-named builtin).
pub(crate) fn checker_with_resolved_imports(mut checker: TypeChecker, path: &Path) -> TypeChecker {
    let graph = harn_modules::build(&[path.to_path_buf()]);
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
