use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::preflight::PreflightDiagnostic;
use super::source::parse_resolved_module;

/// Tracks the origin of an imported name for collision detection.
struct ImportedName {
    module_path: String,
}

/// Collect all function names that would be imported by each import statement
/// in the program, and flag collisions.
pub(super) fn scan_import_collisions(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let mut imported_names: std::collections::HashMap<String, ImportedName> =
        std::collections::HashMap::new();

    for node in program {
        match &node.node {
            Node::ImportDecl { path, .. } => {
                let Some(import_path) = resolve_import_path(file_path, path) else {
                    // Already diagnosed as unresolved elsewhere.
                    continue;
                };
                let import_str = import_path.to_string_lossy().into_owned();
                let Some(parsed) = parse_resolved_module(&import_path) else {
                    continue;
                };
                let names = collect_exported_names(&parsed.0, &import_path);
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
                let module_path = resolve_import_path(file_path, path)
                    .map(|p| p.to_string_lossy().into_owned())
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

/// Flag selective imports that name a symbol the target module declares but
/// does not export — a non-`pub` function in a module that marks something
/// else `pub`. Such names are not importable (matching the runtime loader and
/// strict-visibility languages like TypeScript/Rust/Go), so point at the
/// import with an actionable "mark it `pub`" message instead of leaving the
/// author to discover it only as a runtime failure. Fires even when the
/// imported name is never called.
///
/// The "is it exported?" determination is owned by the module graph
/// ([`harn_modules::ModuleGraph::non_exported_selective_imports`]) — the single
/// source of truth shared with the typechecker and runtime; here we only map
/// that result onto the import spans in this file, mirroring
/// [`scan_re_export_conflicts`].
pub(super) fn scan_selective_import_visibility(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    graph: &harn_modules::ModuleGraph,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let offenders = graph.non_exported_selective_imports(file_path);
    if offenders.is_empty() {
        return;
    }

    // Selective-import spans keyed by (name, module-as-written) so each
    // diagnostic lands on the exact `import { name } from "module"` line.
    let mut spans: std::collections::HashMap<(String, String), harn_lexer::Span> =
        std::collections::HashMap::new();
    for node in program {
        if let Node::SelectiveImport { names, path, .. } = &node.node {
            for name in names {
                spans
                    .entry((name.clone(), path.clone()))
                    .or_insert(node.span);
            }
        }
    }
    let fallback_span = program
        .first()
        .map(|n| n.span)
        .unwrap_or_else(|| harn_lexer::Span::with_offsets(0, 0, 1, 1));

    for offender in offenders {
        let span = spans
            .get(&(offender.name.clone(), offender.module.clone()))
            .copied()
            .unwrap_or(fallback_span);
        diagnostics.push(PreflightDiagnostic {
            code: Code::ImportSymbolMissing,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span,
            message: format!(
                "imported symbol `{}` is not exported by `{}` — it is defined there but not `pub`",
                offender.name, offender.module
            ),
            help: Some(format!(
                "mark `{}` as `pub` in `{}` to export it",
                offender.name, offender.module
            )),
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

/// Parse a module source and extract the names it would export via wildcard
/// import. Resolves `pub import` re-export chains by recursing into the
/// target module's source so the collision check sees the same names a
/// runtime wildcard import would expose.
fn collect_exported_names(source: &str, file_path: &Path) -> Vec<String> {
    let mut visited = std::collections::HashSet::new();
    let mut names = Vec::new();
    collect_exported_names_into(source, file_path, &mut names, &mut visited);
    names
}

fn collect_exported_names_into(
    source: &str,
    file_path: &Path,
    names: &mut Vec<String>,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    let canonical = harn_modules::canonical_path(file_path);
    if !visited.insert(canonical) {
        return;
    }
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut parser = harn_parser::Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(_) => return,
    };
    let has_pub = program
        .iter()
        .any(|n| matches!(&n.node, Node::FnDecl { is_pub: true, .. }));
    for node in &program {
        match &node.node {
            Node::FnDecl { name, is_pub, .. } if !has_pub || *is_pub => {
                names.push(name.clone());
            }
            Node::SelectiveImport {
                names: import_names,
                is_pub: true,
                ..
            } => {
                names.extend(import_names.iter().cloned());
            }
            Node::ImportDecl {
                path: nested,
                is_pub: true,
            } => {
                if let Some(nested_path) = resolve_import_path(file_path, nested) {
                    if let Some(parsed) = parse_resolved_module(&nested_path) {
                        collect_exported_names_into(&parsed.0, &nested_path, names, visited);
                    }
                }
            }
            _ => {}
        }
    }
}
