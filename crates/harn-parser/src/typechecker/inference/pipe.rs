//! Pipe-placeholder discovery for type inference.

use crate::ast::{Node, SNode};

use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn contains_pipe_placeholder(node: &SNode) -> bool {
        match &node.node {
            Node::Identifier(name) if name == "_" => true,
            Node::FunctionCall { args, .. } => args.iter().any(Self::contains_pipe_placeholder),
            Node::ValueCall { callee, args } => {
                Self::contains_pipe_placeholder(callee)
                    || args.iter().any(Self::contains_pipe_placeholder)
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                Self::contains_pipe_placeholder(object)
                    || args.iter().any(Self::contains_pipe_placeholder)
            }
            Node::HitlExpr { args, .. } => args
                .iter()
                .any(|arg| Self::contains_pipe_placeholder(&arg.value)),
            Node::BinaryOp { left, right, .. } => {
                Self::contains_pipe_placeholder(left) || Self::contains_pipe_placeholder(right)
            }
            Node::UnaryOp { operand, .. } => Self::contains_pipe_placeholder(operand),
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                Self::contains_pipe_placeholder(condition)
                    || Self::contains_pipe_placeholder(true_expr)
                    || Self::contains_pipe_placeholder(false_expr)
            }
            Node::Assignment { target, value, .. } => {
                Self::contains_pipe_placeholder(target) || Self::contains_pipe_placeholder(value)
            }
            Node::RangeExpr { start, end, .. } => {
                Self::contains_pipe_placeholder(start) || Self::contains_pipe_placeholder(end)
            }
            Node::ListLiteral(items) => items.iter().any(Self::contains_pipe_placeholder),
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => entries.iter().any(|entry| {
                Self::contains_pipe_placeholder(&entry.key)
                    || Self::contains_pipe_placeholder(&entry.value)
            }),
            Node::EnumConstruct { args, .. } => args.iter().any(Self::contains_pipe_placeholder),
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                Self::contains_pipe_placeholder(object)
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                Self::contains_pipe_placeholder(object) || Self::contains_pipe_placeholder(index)
            }
            Node::SliceAccess { object, start, end } => {
                Self::contains_pipe_placeholder(object)
                    || start
                        .as_ref()
                        .is_some_and(|start| Self::contains_pipe_placeholder(start))
                    || end
                        .as_ref()
                        .is_some_and(|end| Self::contains_pipe_placeholder(end))
            }
            Node::Spread(inner)
            | Node::TryOperator { operand: inner }
            | Node::TryStar { operand: inner } => Self::contains_pipe_placeholder(inner),
            _ => false,
        }
    }
}
