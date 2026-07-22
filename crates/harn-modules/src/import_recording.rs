//! Record `import` AST nodes into [`ModuleInfo`] during graph construction.

use std::collections::HashSet;
use std::path::Path;

use harn_parser::{Node, SNode};

use crate::namespace_imports::record_namespace_import;
use crate::package_imports::resolve_import_path_with_snapshots;
use crate::package_snapshot::PackageSnapshot;
use crate::{normalize_path, ImportRef, ModuleInfo};

/// Fold one top-level import declaration into `module`.
pub(crate) fn record_import_node(
    module: &mut ModuleInfo,
    file: &Path,
    snode: &SNode,
    package_snapshots: &[PackageSnapshot],
) -> bool {
    match &snode.node {
        Node::ImportDecl { path, is_pub } => {
            let import_path = resolve_import_path_with_snapshots(file, path, package_snapshots);
            if import_path.is_none() {
                module.has_unresolved_wildcard_import = true;
            }
            if *is_pub {
                if let Some(resolved) = &import_path {
                    module
                        .wildcard_re_export_paths
                        .push(normalize_path(resolved));
                }
            }
            module.imports.push(ImportRef {
                raw_path: path.clone(),
                path: import_path,
                selective_names: None,
                namespace_alias: None,
                import_span: snode.span,
            });
            true
        }
        Node::SelectiveImport {
            names,
            path,
            is_pub,
        } => {
            let import_path = resolve_import_path_with_snapshots(file, path, package_snapshots);
            if import_path.is_none() {
                module.has_unresolved_selective_import = true;
            }
            if *is_pub {
                if let Some(resolved) = &import_path {
                    let canonical = normalize_path(resolved);
                    for name in names {
                        module
                            .selective_re_exports
                            .entry(name.clone())
                            .or_default()
                            .push(canonical.clone());
                    }
                }
            }
            let names: HashSet<String> = names.iter().cloned().collect();
            module.selective_import_names.extend(names.iter().cloned());
            module.imports.push(ImportRef {
                raw_path: path.clone(),
                path: import_path,
                selective_names: Some(names),
                namespace_alias: None,
                import_span: snode.span,
            });
            true
        }
        Node::NamespaceImport {
            alias,
            path,
            is_pub,
        } => {
            record_namespace_import(
                module,
                file,
                snode.span,
                alias,
                path,
                *is_pub,
                package_snapshots,
            );
            true
        }
        _ => false,
    }
}
