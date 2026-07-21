//! Namespace import (`import * as alias from "..."`) graph APIs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::package_imports::resolve_import_path_with_snapshots;
use crate::package_snapshot::PackageSnapshot;
use crate::{decl_site, normalize_path, DefKind, DefSite, ImportRef, ModuleGraph, ModuleInfo};

/// One `import * as alias from "..."` binding visible from a consumer file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceImportInfo {
    pub alias: String,
    pub raw_path: String,
    pub resolved_path: Option<PathBuf>,
    /// Public export names from the target module (empty when unresolved).
    pub member_names: Vec<String>,
    /// Declaration kind for each exported member name.
    pub member_kinds: BTreeMap<String, DefKind>,
}

impl ModuleGraph {
    /// Namespace imports (`import * as alias from "..."`) declared by `file`.
    ///
    /// Returns `None` when any namespace import path is unresolved so callers
    /// can fall back to conservative checking. Member names/kinds come from
    /// the target module's public export surface (`exports` + `DefKind`).
    pub fn namespace_imports_for_file(&self, file: &Path) -> Option<Vec<NamespaceImportInfo>> {
        let file = normalize_path(file);
        let module = self.modules.get(&file)?;
        if module.has_unresolved_namespace_import {
            return None;
        }

        let mut out = Vec::new();
        for import in &module.imports {
            let Some(alias) = &import.namespace_alias else {
                continue;
            };
            let (member_names, member_kinds) = match &import.path {
                Some(import_path) => {
                    let imported = self
                        .modules
                        .get(import_path)
                        .or_else(|| self.modules.get(&normalize_path(import_path)))?;
                    if imported.load_error.is_some() {
                        return None;
                    }
                    let mut names: Vec<String> = imported.exports.iter().cloned().collect();
                    names.sort();
                    let mut kinds = BTreeMap::new();
                    for name in &names {
                        if let Some(kind) = self.exported_kind(import_path, name) {
                            kinds.insert(name.clone(), kind);
                        }
                    }
                    (names, kinds)
                }
                None => (Vec::new(), BTreeMap::new()),
            };
            out.push(NamespaceImportInfo {
                alias: alias.clone(),
                raw_path: import.raw_path.clone(),
                resolved_path: import.path.as_ref().map(|path| normalize_path(path)),
                member_names,
                member_kinds,
            });
        }
        Some(out)
    }

    /// Look up a member under a namespace import alias visible from `file`.
    ///
    /// Returns the target module's definition site for `member`, or `None`
    /// when the alias is not a namespace import / the member is not exported.
    pub fn namespace_member_lookup(
        &self,
        file: &Path,
        alias: &str,
        member: &str,
    ) -> Option<DefSite> {
        let file = normalize_path(file);
        let module = self.modules.get(&file)?;
        let target = module
            .imports
            .iter()
            .find(|import| import.namespace_alias.as_deref() == Some(alias))
            .and_then(|import| import.path.as_ref())
            .or_else(|| module.namespace_re_exports.get(alias))?;
        if self.exported_kind(target, member).is_none() {
            return None;
        }
        self.export_definition_of(target, member)
            .or_else(|| self.definition_of(target, member))
    }
}

/// Record a `NamespaceImport` into `module` during graph construction.
pub(crate) fn record_namespace_import(
    module: &mut ModuleInfo,
    file: &Path,
    span: harn_lexer::Span,
    alias: &str,
    path: &str,
    is_pub: bool,
    package_snapshots: &[PackageSnapshot],
) {
    let import_path = resolve_import_path_with_snapshots(file, path, package_snapshots);
    if import_path.is_none() {
        module.has_unresolved_namespace_import = true;
    }
    // Bind the alias locally as a Variable. `pub import * as alias`
    // re-exports the namespace object itself — never flatten target
    // members into this module's public surface (contrast
    // `wildcard_re_export_paths`).
    module.declarations.insert(
        alias.to_string(),
        decl_site(file, span, alias, DefKind::Variable),
    );
    if is_pub {
        module.own_exports.insert(alias.to_string());
        module.exports.insert(alias.to_string());
        if let Some(resolved) = &import_path {
            module
                .namespace_re_exports
                .insert(alias.to_string(), normalize_path(resolved));
        }
    }
    module.imports.push(ImportRef {
        raw_path: path.to_string(),
        path: import_path,
        selective_names: None,
        namespace_alias: Some(alias.to_string()),
        import_span: span,
    });
}
