//! Canonical type-checker projection of a resolved module graph.

use std::path::Path;

use harn_parser::analysis::TypeCheckConfig;
use harn_parser::NamespaceImportBinding;

use crate::ModuleGraph;

impl ModuleGraph {
    /// Project every import visible to `file` into one type-checker config.
    ///
    /// Callers layer execution-specific strictness and authority on top. The
    /// graph remains the sole owner of named, typed, callable, and namespace
    /// import resolution.
    pub fn typecheck_import_config_for_file(&self, file: &Path) -> TypeCheckConfig {
        let namespace_imports = self
            .namespace_imports_for_file(file)
            .unwrap_or_default()
            .into_iter()
            .map(|info| {
                (
                    info.alias,
                    NamespaceImportBinding::from_member_signatures(
                        info.raw_path,
                        info.member_names.into_iter().collect(),
                        info.member_signatures,
                    ),
                )
            })
            .collect();

        TypeCheckConfig::new()
            .with_imported_names(self.imported_names_for_file(file))
            .with_imported_type_decls(
                self.imported_type_declarations_for_file(file)
                    .unwrap_or_default(),
            )
            .with_imported_callable_decls(
                self.imported_callable_declarations_for_file(file)
                    .unwrap_or_default(),
            )
            .with_namespace_imports(namespace_imports)
    }
}
