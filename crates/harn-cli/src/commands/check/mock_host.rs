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

fn scan_module(canonical: &Path, source: &str, program: &[SNode]) -> MockHostScan {
    let mut scan = MockHostScan::default();
    for node in program {
        collect_mock_host_capabilities_from_node(node, canonical, source, &mut scan);
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
    source: &str,
    scan: &mut MockHostScan,
) {
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::ScopeBlock { body }
        | Node::TryExpr { body }
        | Node::MutexBlock { body, .. }
        | Node::DeferStmt { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => {
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::SkillDecl { fields, .. } => {
            for (_k, v) in fields {
                collect_mock_host_capabilities_from_node(v, file_path, source, scan);
            }
        }
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            for (_k, v) in fields {
                collect_mock_host_capabilities_from_node(v, file_path, source, scan);
            }
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
            if let Some(summary_body) = summarize {
                for child in summary_body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
        }
        Node::ImportDecl { path, .. }
        | Node::SelectiveImport { path, .. }
        | Node::NamespaceImport { path, .. } => {
            let Some(import_path) = resolve_import_path(file_path, path) else {
                return;
            };
            let canonical = harn_modules::canonical_path(&import_path);
            scan.imports.push((import_path, canonical));
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
            ..
        } => {
            collect_mock_host_capabilities_from_node(condition, file_path, source, scan);
            for child in then_body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
            if let Some(else_body) = else_body {
                for child in else_body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            collect_mock_host_capabilities_from_node(iterable, file_path, source, scan);
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::Retry { count, body } => {
            collect_mock_host_capabilities_from_node(count, file_path, source, scan);
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                collect_mock_host_capabilities_from_node(value, file_path, source, scan);
            }
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                collect_mock_host_capabilities_from_node(value, file_path, source, scan);
            }
        }
        Node::EmitExpr { value } => {
            collect_mock_host_capabilities_from_node(value, file_path, source, scan);
        }
        Node::RequireStmt { condition, message } => {
            collect_mock_host_capabilities_from_node(condition, file_path, source, scan);
            if let Some(message) = message {
                collect_mock_host_capabilities_from_node(message, file_path, source, scan);
            }
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
            for child in catch_body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
            if let Some(finally_body) = finally_body {
                for child in finally_body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_mock_host_capabilities_from_node(condition, file_path, source, scan);
            for child in else_body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::DictLiteral(fields) => {
            for field in fields {
                collect_mock_host_capabilities_from_node(&field.value, file_path, source, scan);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_mock_host_capabilities_from_node(duration, file_path, source, scan);
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::Parallel { expr, body, .. } => {
            collect_mock_host_capabilities_from_node(expr, file_path, source, scan);
            for child in body {
                collect_mock_host_capabilities_from_node(child, file_path, source, scan);
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_mock_host_capabilities_from_node(&case.channel, file_path, source, scan);
                for child in &case.body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
            if let Some((timeout_expr, body)) = timeout {
                collect_mock_host_capabilities_from_node(timeout_expr, file_path, source, scan);
                for child in body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
            if let Some(body) = default_body {
                for child in body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
        }
        Node::FunctionCall { name, args, .. } => {
            if name == "host_mock" {
                if let (Some(Node::StringLiteral(cap)), Some(Node::StringLiteral(op))) = (
                    args.first().map(|arg| &arg.node),
                    args.get(1).map(|arg| &arg.node),
                ) {
                    scan.operations.push((cap.clone(), op.clone()));
                }
            }
            for arg in args {
                collect_mock_host_capabilities_from_node(arg, file_path, source, scan);
            }
        }
        Node::ValueCall { callee, args } => {
            collect_mock_host_capabilities_from_node(callee, file_path, source, scan);
            for arg in args {
                collect_mock_host_capabilities_from_node(arg, file_path, source, scan);
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_mock_host_capabilities_from_node(object, file_path, source, scan);
            for arg in args {
                collect_mock_host_capabilities_from_node(arg, file_path, source, scan);
            }
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        }
        | Node::Spread(object)
        | Node::TryOperator { operand: object }
        | Node::NonNullAssert { operand: object }
        | Node::TryStar { operand: object } => {
            collect_mock_host_capabilities_from_node(object, file_path, source, scan);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_mock_host_capabilities_from_node(object, file_path, source, scan);
            collect_mock_host_capabilities_from_node(index, file_path, source, scan);
        }
        Node::SliceAccess { object, start, end } => {
            collect_mock_host_capabilities_from_node(object, file_path, source, scan);
            if let Some(start) = start {
                collect_mock_host_capabilities_from_node(start, file_path, source, scan);
            }
            if let Some(end) = end {
                collect_mock_host_capabilities_from_node(end, file_path, source, scan);
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::Assignment {
            target: left,
            value: right,
            ..
        } => {
            collect_mock_host_capabilities_from_node(left, file_path, source, scan);
            collect_mock_host_capabilities_from_node(right, file_path, source, scan);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_mock_host_capabilities_from_node(condition, file_path, source, scan);
            collect_mock_host_capabilities_from_node(true_expr, file_path, source, scan);
            collect_mock_host_capabilities_from_node(false_expr, file_path, source, scan);
        }
        Node::ThrowStmt { value } => {
            collect_mock_host_capabilities_from_node(value, file_path, source, scan);
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
            for arg in args {
                collect_mock_host_capabilities_from_node(arg, file_path, source, scan);
            }
        }
        Node::StructConstruct { fields, .. } => {
            for field in fields {
                collect_mock_host_capabilities_from_node(&field.value, file_path, source, scan);
            }
        }
        Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
            collect_mock_host_capabilities_from_node(value, file_path, source, scan);
        }
        Node::MatchExpr { value, arms } => {
            collect_mock_host_capabilities_from_node(value, file_path, source, scan);
            for arm in arms {
                collect_mock_host_capabilities_from_node(&arm.pattern, file_path, source, scan);
                for child in &arm.body {
                    collect_mock_host_capabilities_from_node(child, file_path, source, scan);
                }
            }
        }
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                collect_mock_host_capabilities_from_node(method, file_path, source, scan);
            }
        }
        Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_)
        | Node::RangeExpr { .. }
        | Node::TypeDecl { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt => {
            let _ = source;
        }
        Node::AttributedDecl { inner, .. } => {
            collect_mock_host_capabilities_from_node(inner, file_path, source, scan);
        }
        Node::OrPattern(alternatives) => {
            for alt in alternatives {
                collect_mock_host_capabilities_from_node(alt, file_path, source, scan);
            }
        }
    }
}
