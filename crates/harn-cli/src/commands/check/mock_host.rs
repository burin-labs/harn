use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{Node, SNode};

use super::scan_memo::ScanMemo;
use super::source::parse_resolved_module;

/// One module's own `host_mock("cap", "op")` declarations and resolved
/// imports. Imports are followed at replay time so a module shared by many
/// checked files is walked once per run.
#[derive(Default)]
pub(super) struct MockHostScan {
    operations: Vec<(String, String)>,
    /// `(resolved path, canonical path)` per import, in source order.
    imports: Vec<(PathBuf, PathBuf)>,
}

/// Collect every capability the file's import closure mocks with
/// `host_mock(...)` into `capabilities`.
pub(super) fn collect_mock_host_capabilities(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    memo: &ScanMemo,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    let canonical = harn_modules::canonical_path(file_path);
    let mut visited = HashSet::from([canonical.clone()]);
    let entry = scan_module(&canonical, source, program);
    replay(&entry, memo, &mut visited, capabilities);
}

fn scan_module(canonical: &Path, _source: &str, program: &[SNode]) -> MockHostScan {
    let mut scan = MockHostScan::default();
    for node in program {
        collect_mock_host_capabilities_from_node(node, canonical, &mut scan);
    }
    scan
}

fn replay(
    scan: &MockHostScan,
    memo: &ScanMemo,
    visited: &mut HashSet<PathBuf>,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    for (capability, operation) in &scan.operations {
        capabilities
            .entry(capability.clone())
            .or_default()
            .insert(operation.clone());
    }
    for (path, canonical) in &scan.imports {
        if visited.contains(canonical) {
            continue;
        }
        let imported = memo.mock_host(canonical, || {
            let parsed = parse_resolved_module(path)?;
            Some(scan_module(canonical, &parsed.source, &parsed.program))
        });
        if let Some(imported) = imported {
            visited.insert(canonical.clone());
            replay(&imported, memo, visited, capabilities);
        }
    }
}

fn collect_mock_host_capabilities_from_node(
    node: &SNode,
    file_path: &Path,
    scan: &mut MockHostScan,
) {
    harn_parser::visit::walk_node(node, &mut |node| match &node.node {
        Node::ImportDecl { path, .. }
        | Node::SelectiveImport { path, .. }
        | Node::NamespaceImport { path, .. } => {
            if let Some(import_path) = resolve_import_path(file_path, path) {
                let canonical = harn_modules::canonical_path(&import_path);
                scan.imports.push((import_path, canonical));
            }
        }
        Node::FunctionCall { name, args, .. } if name == "host_mock" => {
            if let (Some(Node::StringLiteral(cap)), Some(Node::StringLiteral(op))) = (
                args.first().map(|arg| &arg.node),
                args.get(1).map(|arg| &arg.node),
            ) {
                scan.operations.push((cap.clone(), op.clone()));
            }
        }
        _ => {}
    });
}
