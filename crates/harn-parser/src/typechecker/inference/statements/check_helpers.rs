use super::*;

impl TypeChecker {
    /// Walk a property/subscript assignment target to its root identifier.
    pub(super) fn assignment_root_identifier(target: &SNode) -> Option<&str> {
        match &target.node {
            Node::Identifier(name) => Some(name.as_str()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. } => {
                Self::assignment_root_identifier(object)
            }
            _ => None,
        }
    }

    pub(super) fn defer_forbidden_transfer(body: &[SNode]) -> Option<(&'static str, Span)> {
        body.iter().find_map(Self::node_defer_forbidden_transfer)
    }

    fn node_defer_forbidden_transfer(node: &SNode) -> Option<(&'static str, Span)> {
        match &node.node {
            Node::ReturnStmt { .. } => Some(("return", node.span)),
            Node::YieldExpr { .. } => Some(("yield", node.span)),
            Node::Closure { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::Pipeline { .. }
            | Node::OverrideDecl { .. } => None,
            _ => crate::visit::immediate_children(node)
                .into_iter()
                .find_map(Self::node_defer_forbidden_transfer),
        }
    }
}
