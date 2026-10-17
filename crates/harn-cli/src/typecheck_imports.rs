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
pub(crate) fn checker_with_resolved_imports(checker: TypeChecker, path: &Path) -> TypeChecker {
    let graph = harn_modules::build(&[path.to_path_buf()]);
    checker_with_resolved_graph(checker, path, &graph)
}

pub(crate) fn checker_with_resolved_graph(
    checker: TypeChecker,
    path: &Path,
    graph: &harn_modules::ModuleGraph,
) -> TypeChecker {
    graph
        .typecheck_import_config_for_file(path)
        .apply_imports_to(checker)
}
