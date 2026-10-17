//! Cyclomatic complexity analysis for the linter's cyclomatic-complexity rule.

use harn_parser::{Node, SNode};

pub(crate) fn cyclomatic_complexity(nodes: &[SNode]) -> usize {
    fn body_complexity(nodes: &[SNode]) -> usize {
        nodes.iter().map(node_complexity).sum()
    }

    fn node_complexity(node: &SNode) -> usize {
        let own = match &node.node {
            // Nested declarations have their own complexity diagnostic. A closure
            // remains part of its enclosing callable, as in the existing rule.
            Node::Pipeline { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::OverrideDecl { .. }
            | Node::SkillDecl { .. }
            | Node::EvalPackDecl { .. }
            | Node::ImplBlock { .. }
            | Node::EnumDecl { .. }
            | Node::StructDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. } => return 0,
            Node::IfElse { .. }
            | Node::ForIn { .. }
            | Node::WhileLoop { .. }
            | Node::Retry { .. }
            | Node::GuardStmt { .. }
            | Node::TryCatch { .. }
            | Node::Ternary { .. } => 1,
            Node::SelectExpr { cases, .. } => cases.len(),
            Node::BinaryOp { op, .. } => usize::from(op.is_logical()),
            Node::MatchExpr { value, arms } => {
                // Pattern alternatives describe values, not control-flow paths.
                return 1
                    + node_complexity(value)
                    + arms
                        .iter()
                        .map(|arm| {
                            arm.guard
                                .as_ref()
                                .map(|guard| node_complexity(guard))
                                .unwrap_or(0)
                                + body_complexity(&arm.body)
                        })
                        .sum::<usize>();
            }
            _ => 0,
        };
        let mut total = own;
        harn_parser::visit::for_each_immediate_child(node, &mut |child| {
            total += node_complexity(child);
        });
        total
    }

    1 + body_complexity(nodes)
}
