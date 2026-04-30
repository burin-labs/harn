use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{Node, SNode};

use crate::parse_source_file;

pub(super) fn collect_mock_host_capabilities(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    visited: &mut HashSet<PathBuf>,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    if !visited.insert(canonical.clone()) {
        return;
    }
    for node in program {
        collect_mock_host_capabilities_from_node(node, &canonical, source, visited, capabilities);
    }
}

fn collect_mock_host_capabilities_from_node(
    node: &SNode,
    file_path: &Path,
    source: &str,
    visited: &mut HashSet<PathBuf>,
    capabilities: &mut HashMap<String, HashSet<String>>,
) {
    match &node.node {
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::TryExpr { body }
        | Node::MutexBlock { body }
        | Node::DeferStmt { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => {
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::SkillDecl { fields, .. } => {
            for (_k, v) in fields {
                collect_mock_host_capabilities_from_node(
                    v,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::ImportDecl { path, .. } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            let Some(import_path) = resolve_import_path(file_path, path) else {
                return;
            };
            let import_str = import_path.to_string_lossy().into_owned();
            let (import_source, import_program) = parse_source_file(&import_str);
            collect_mock_host_capabilities(
                &import_path,
                &import_source,
                &import_program,
                visited,
                capabilities,
            );
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in then_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(else_body) = else_body {
                for child in else_body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            collect_mock_host_capabilities_from_node(
                iterable,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::Retry { count, body } => {
            collect_mock_host_capabilities_from_node(
                count,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                collect_mock_host_capabilities_from_node(
                    value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::EmitExpr { value } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::RequireStmt { condition, message } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            if let Some(message) = message {
                collect_mock_host_capabilities_from_node(
                    message,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            for child in catch_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(finally_body) = finally_body {
                for child in finally_body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in else_body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::DictLiteral(fields) => {
            for field in fields {
                collect_mock_host_capabilities_from_node(
                    &field.value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_mock_host_capabilities_from_node(
                duration,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::Parallel { expr, body, .. } => {
            collect_mock_host_capabilities_from_node(
                expr,
                file_path,
                source,
                visited,
                capabilities,
            );
            for child in body {
                collect_mock_host_capabilities_from_node(
                    child,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_mock_host_capabilities_from_node(
                    &case.channel,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in &case.body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
            if let Some((timeout_expr, body)) = timeout {
                collect_mock_host_capabilities_from_node(
                    timeout_expr,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
            if let Some(body) = default_body {
                for child in body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::FunctionCall { name, args, .. } => {
            if name == "host_mock" {
                if let (Some(Node::StringLiteral(cap)), Some(Node::StringLiteral(op))) = (
                    args.first().map(|arg| &arg.node),
                    args.get(1).map(|arg| &arg.node),
                ) {
                    capabilities
                        .entry(cap.clone())
                        .or_default()
                        .insert(op.clone());
                }
            }
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        }
        | Node::Spread(object)
        | Node::TryOperator { operand: object }
        | Node::TryStar { operand: object } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                index,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::SliceAccess { object, start, end } => {
            collect_mock_host_capabilities_from_node(
                object,
                file_path,
                source,
                visited,
                capabilities,
            );
            if let Some(start) = start {
                collect_mock_host_capabilities_from_node(
                    start,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
            if let Some(end) = end {
                collect_mock_host_capabilities_from_node(
                    end,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::Assignment {
            target: left,
            value: right,
            ..
        } => {
            collect_mock_host_capabilities_from_node(
                left,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                right,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_mock_host_capabilities_from_node(
                condition,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                true_expr,
                file_path,
                source,
                visited,
                capabilities,
            );
            collect_mock_host_capabilities_from_node(
                false_expr,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::ThrowStmt { value } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
            for arg in args {
                collect_mock_host_capabilities_from_node(
                    arg,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::StructConstruct { fields, .. } => {
            for field in fields {
                collect_mock_host_capabilities_from_node(
                    &field.value,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::MatchExpr { value, arms } => {
            collect_mock_host_capabilities_from_node(
                value,
                file_path,
                source,
                visited,
                capabilities,
            );
            for arm in arms {
                collect_mock_host_capabilities_from_node(
                    &arm.pattern,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
                for child in &arm.body {
                    collect_mock_host_capabilities_from_node(
                        child,
                        file_path,
                        source,
                        visited,
                        capabilities,
                    );
                }
            }
        }
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                collect_mock_host_capabilities_from_node(
                    method,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
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
            collect_mock_host_capabilities_from_node(
                inner,
                file_path,
                source,
                visited,
                capabilities,
            );
        }
        Node::OrPattern(alternatives) => {
            for alt in alternatives {
                collect_mock_host_capabilities_from_node(
                    alt,
                    file_path,
                    source,
                    visited,
                    capabilities,
                );
            }
        }
    }
}
