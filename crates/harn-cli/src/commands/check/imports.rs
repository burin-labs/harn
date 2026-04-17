use std::path::Path;

use harn_modules::resolve_import_path;
use harn_parser::{Node, SNode};

use super::preflight::PreflightDiagnostic;

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
            Node::ImportDecl { path } => {
                if path.starts_with("std/") {
                    continue;
                }
                let Some(import_path) = resolve_import_path(file_path, path) else {
                    // Already diagnosed as unresolved elsewhere.
                    continue;
                };
                let import_str = import_path.to_string_lossy().into_owned();
                let Ok(import_source) = std::fs::read_to_string(&import_path) else {
                    continue;
                };
                let names = collect_exported_names(&import_source);
                for name in names {
                    if let Some(existing) = imported_names.get(&name) {
                        if existing.module_path != import_str {
                            diagnostics.push(PreflightDiagnostic {
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
            Node::SelectiveImport { names, path } => {
                if path.starts_with("std/") {
                    continue;
                }
                let module_path = resolve_import_path(file_path, path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                for name in names {
                    if let Some(existing) = imported_names.get(name) {
                        if existing.module_path != module_path {
                            diagnostics.push(PreflightDiagnostic {
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

/// Parse a module source and extract the names it would export via wildcard import.
fn collect_exported_names(source: &str) -> Vec<String> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut parser = harn_parser::Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let has_pub = program
        .iter()
        .any(|n| matches!(&n.node, Node::FnDecl { is_pub: true, .. }));
    program
        .iter()
        .filter_map(|n| match &n.node {
            Node::FnDecl { name, is_pub, .. } => {
                if has_pub && !is_pub {
                    None
                } else {
                    Some(name.clone())
                }
            }
            _ => None,
        })
        .collect()
}
