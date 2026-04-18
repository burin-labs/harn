//! Cyclomatic complexity analysis for the linter's cyclomatic-complexity
//! rule. Isolated from the main linter so the scoring logic is easy to
//! evolve without touching rule dispatch.

use harn_parser::{Node, SNode};

pub(crate) fn cyclomatic_complexity(nodes: &[SNode]) -> usize {
    fn node_complexity(node: &SNode) -> usize {
        match &node.node {
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                1 + node_complexity(condition)
                    + body_complexity(then_body)
                    + else_body
                        .as_ref()
                        .map(|body| body_complexity(body))
                        .unwrap_or(0)
            }
            Node::ForIn { iterable, body, .. } => {
                1 + node_complexity(iterable) + body_complexity(body)
            }
            Node::WhileLoop { condition, body } => {
                1 + node_complexity(condition) + body_complexity(body)
            }
            Node::Retry { count, body } => 1 + node_complexity(count) + body_complexity(body),
            Node::MatchExpr { value, arms } => {
                1 + node_complexity(value)
                    + arms
                        .iter()
                        .map(|arm| {
                            arm.guard
                                .as_ref()
                                .map(|guard| node_complexity(guard))
                                .unwrap_or(0)
                                + body_complexity(&arm.body)
                        })
                        .sum::<usize>()
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                cases.len()
                    + cases
                        .iter()
                        .map(|case| node_complexity(&case.channel) + body_complexity(&case.body))
                        .sum::<usize>()
                    + timeout
                        .as_ref()
                        .map(|(duration, body)| node_complexity(duration) + body_complexity(body))
                        .unwrap_or(0)
                    + default_body
                        .as_ref()
                        .map(|body| body_complexity(body))
                        .unwrap_or(0)
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => 1 + node_complexity(condition) + body_complexity(else_body),
            Node::RequireStmt { condition, message } => {
                node_complexity(condition)
                    + message
                        .as_ref()
                        .map(|expr| node_complexity(expr))
                        .unwrap_or(0)
            }
            Node::TryCatch {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                1 + body_complexity(body)
                    + body_complexity(catch_body)
                    + finally_body
                        .as_ref()
                        .map(|body| body_complexity(body))
                        .unwrap_or(0)
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                1 + node_complexity(condition)
                    + node_complexity(true_expr)
                    + node_complexity(false_expr)
            }
            Node::BinaryOp { op, left, right } => {
                usize::from(op == "&&" || op == "||")
                    + node_complexity(left)
                    + node_complexity(right)
            }
            Node::Parallel { expr, body, .. }
            | Node::DeadlineBlock {
                duration: expr,
                body,
            } => node_complexity(expr) + body_complexity(body),
            Node::MutexBlock { body }
            | Node::TryExpr { body }
            | Node::DeferStmt { body }
            | Node::Block(body)
            | Node::SpawnExpr { body } => body_complexity(body),
            Node::FunctionCall { args, .. } => args.iter().map(node_complexity).sum(),
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                node_complexity(object) + args.iter().map(node_complexity).sum::<usize>()
            }
            Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => fields
                .iter()
                .map(|entry| node_complexity(&entry.key) + node_complexity(&entry.value))
                .sum(),
            Node::ListLiteral(items) => items.iter().map(node_complexity).sum(),
            Node::Assignment { target, value, .. } => {
                node_complexity(target) + node_complexity(value)
            }
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::UnaryOp {
                operand: object, ..
            }
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::Spread(object) => node_complexity(object),
            Node::SubscriptAccess { object, index } => {
                node_complexity(object) + node_complexity(index)
            }
            Node::SliceAccess { object, start, end } => {
                node_complexity(object)
                    + start
                        .as_ref()
                        .map(|expr| node_complexity(expr))
                        .unwrap_or(0)
                    + end.as_ref().map(|expr| node_complexity(expr)).unwrap_or(0)
            }
            Node::RangeExpr { start, end, .. } => node_complexity(start) + node_complexity(end),
            Node::ThrowStmt { value } | Node::ReturnStmt { value: Some(value) } => {
                node_complexity(value)
            }
            Node::YieldExpr { value } => value
                .as_ref()
                .map(|expr| node_complexity(expr))
                .unwrap_or(0),
            Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
                node_complexity(value)
            }
            Node::EnumConstruct { args, .. } => args.iter().map(node_complexity).sum(),
            Node::Closure { body, .. } => body_complexity(body),
            Node::ReturnStmt { value: None }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::InterpolatedString(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::DurationLiteral(_)
            | Node::Identifier(_)
            | Node::ImportDecl { .. }
            | Node::SelectiveImport { .. }
            | Node::OverrideDecl { .. }
            | Node::EnumDecl { .. }
            | Node::StructDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::SkillDecl { .. }
            | Node::TypeDecl { .. }
            | Node::Pipeline { .. }
            | Node::ImplBlock { .. } => 0,
            Node::AttributedDecl { inner, .. } => node_complexity(inner),
        }
    }

    fn body_complexity(nodes: &[SNode]) -> usize {
        nodes.iter().map(node_complexity).sum()
    }

    1 + body_complexity(nodes)
}
