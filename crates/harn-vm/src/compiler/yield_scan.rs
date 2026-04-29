use harn_parser::{Node, SNode};

/// Check if a list of AST nodes contains any `yield` expression (used to detect generator functions).
pub(super) fn body_contains_yield(nodes: &[SNode]) -> bool {
    nodes.iter().any(|sn| node_contains_yield(&sn.node))
}

pub(super) fn node_contains_yield(node: &Node) -> bool {
    match node {
        Node::YieldExpr { .. } | Node::EmitExpr { .. } => true,
        // Don't recurse into nested fn/closure: yield in a nested fn does
        // NOT make the outer a generator.
        Node::FnDecl { .. } | Node::Closure { .. } => false,
        Node::Block(stmts) => body_contains_yield(stmts),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            node_contains_yield(&condition.node)
                || body_contains_yield(then_body)
                || else_body.as_ref().is_some_and(|b| body_contains_yield(b))
        }
        Node::WhileLoop { condition, body } => {
            node_contains_yield(&condition.node) || body_contains_yield(body)
        }
        Node::ForIn { iterable, body, .. } => {
            node_contains_yield(&iterable.node) || body_contains_yield(body)
        }
        Node::TryCatch {
            body, catch_body, ..
        } => body_contains_yield(body) || body_contains_yield(catch_body),
        Node::TryExpr { body } => body_contains_yield(body),
        _ => false,
    }
}
