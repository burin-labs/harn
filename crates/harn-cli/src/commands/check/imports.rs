use std::path::{Path, PathBuf};

use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::preflight::PreflightDiagnostic;

/// Tracks the origin of an imported name for collision detection.
struct ImportedName {
    module_path: String,
}

/// Collect the public names imported by each import statement and flag
/// collisions. The module graph owns export visibility and re-export
/// traversal, so this preflight does not maintain a second export table.
pub(super) fn scan_import_collisions(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    graph: &harn_modules::ModuleGraph,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let mut imported_names: std::collections::HashMap<String, ImportedName> =
        std::collections::HashMap::new();
    let imports = graph.imports_for_module(file_path);

    for node in program {
        match &node.node {
            Node::ImportDecl { path, .. } => {
                let Some(import_path) = imports
                    .iter()
                    .find(|import| import.raw_path == *path)
                    .and_then(|import| import.resolved_path.clone())
                else {
                    // Already diagnosed as unresolved elsewhere.
                    continue;
                };
                let import_str = import_path.display().to_string();
                let names = graph.exports_for_module(Path::new(&import_path));
                for name in names {
                    if let Some(existing) = imported_names.get(&name) {
                        if existing.module_path != import_str {
                            diagnostics.push(PreflightDiagnostic {
                                code: Code::ModuleImportCollision,
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: node.span,
                                message: format!(
                                    "preflight: import collision — '{name}' is exported by both '{}' and '{path}'",
                                    existing.module_path
                                ),
                                help: Some(format!(
                                    "use selective imports to disambiguate: import {{ {name} }} from \"...\""
                                )),
                                tags: None,
                            });
                        }
                    } else {
                        imported_names.insert(
                            name,
                            ImportedName {
                                module_path: import_str.clone(),
                            },
                        );
                    }
                }
            }
            Node::SelectiveImport { names, path, .. } => {
                let module_path = imports
                    .iter()
                    .find(|import| import.raw_path == *path)
                    .and_then(|import| import.resolved_path.clone())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| path.clone());
                for name in names {
                    if let Some(existing) = imported_names.get(name) {
                        if existing.module_path != module_path {
                            diagnostics.push(PreflightDiagnostic {
                                code: Code::ModuleImportCollision,
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: node.span,
                                message: format!(
                                    "preflight: import collision — '{name}' is exported by both '{}' and '{path}'",
                                    existing.module_path
                                ),
                                help: Some(
                                    "rename one of the imported modules or avoid importing conflicting names"
                                        .to_string(),
                                ),
                                tags: None,
                            });
                        }
                    } else {
                        imported_names.insert(
                            name.clone(),
                            ImportedName {
                                module_path: module_path.clone(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Project invalid selective imports from the module graph into preflight
/// diagnostics. Name resolution, classification, wording, help, and spans are
/// all graph-owned so every static consumer matches the runtime boundary.
pub(super) fn scan_selective_import_visibility(
    file_path: &Path,
    source: &str,
    graph: &harn_modules::ModuleGraph,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    for issue in graph.selective_import_issues(file_path) {
        diagnostics.push(PreflightDiagnostic {
            code: Code::ImportSymbolMissing,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: issue.span,
            message: issue.message(),
            help: Some(issue.help()),
            tags: None,
        });
    }
}

/// Emit diagnostics for ambiguous or conflicting `pub import` re-exports
/// declared in `file_path`. Two re-exports of the same name from
/// different source modules — or a re-export that shadows a locally
/// declared exported symbol — produce one diagnostic naming every
/// contributing source.
pub(super) fn scan_re_export_conflicts(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    graph: &harn_modules::ModuleGraph,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let conflicts = graph.re_export_conflicts(file_path);
    if conflicts.is_empty() {
        return;
    }

    // Re-export sites in the AST keyed by name so we can attach the
    // diagnostic to the offending `pub import` line. A local declaration
    // colliding with a re-export gets the file-level fallback span.
    let mut name_spans: std::collections::HashMap<String, harn_lexer::Span> =
        std::collections::HashMap::new();
    let fallback_span = program
        .first()
        .map(|n| n.span)
        .unwrap_or_else(|| harn_lexer::Span::with_offsets(0, 0, 1, 1));
    for node in program {
        match &node.node {
            Node::SelectiveImport {
                names,
                is_pub: true,
                ..
            } => {
                for name in names {
                    name_spans.entry(name.clone()).or_insert(node.span);
                }
            }
            Node::ImportDecl { is_pub: true, .. } => {
                // Spans on wildcard re-exports are best-effort: we don't
                // know which names came from this site without re-loading
                // the source module. The diagnostic message lists every
                // contributing module path explicitly, so the location is
                // mostly cosmetic.
            }
            _ => {}
        }
    }

    for conflict in conflicts {
        let span = name_spans
            .get(&conflict.name)
            .copied()
            .unwrap_or(fallback_span);
        let sources_pretty: Vec<String> = conflict
            .sources
            .iter()
            .map(|p: &PathBuf| p.display().to_string())
            .collect();
        diagnostics.push(PreflightDiagnostic {
            code: Code::ModuleReExportConflict,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span,
            message: format!(
                "preflight: re-export conflict — '{}' is re-exported (or locally defined) by multiple sources: {}",
                conflict.name,
                sources_pretty.join(", ")
            ),
            help: Some(
                "remove or rename one of the conflicting `pub import` declarations"
                    .to_string(),
            ),
            tags: None,
        });
    }
}
