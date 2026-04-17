use harn_parser::{Node, SNode};

/// Check if an AST node contains `_` identifier (pipe placeholder).
pub(super) fn contains_pipe_placeholder(node: &SNode) -> bool {
    match &node.node {
        Node::Identifier(name) if name == "_" => true,
        Node::FunctionCall { args, .. } => args.iter().any(contains_pipe_placeholder),
        Node::MethodCall { object, args, .. } => {
            contains_pipe_placeholder(object) || args.iter().any(contains_pipe_placeholder)
        }
        Node::BinaryOp { left, right, .. } => {
            contains_pipe_placeholder(left) || contains_pipe_placeholder(right)
        }
        Node::UnaryOp { operand, .. } => contains_pipe_placeholder(operand),
        Node::ListLiteral(items) => items.iter().any(contains_pipe_placeholder),
        Node::PropertyAccess { object, .. } => contains_pipe_placeholder(object),
        Node::SubscriptAccess { object, index } => {
            contains_pipe_placeholder(object) || contains_pipe_placeholder(index)
        }
        _ => false,
    }
}

/// Replace all `_` identifiers with `__pipe` in an AST node (for pipe placeholder desugaring).
pub(super) fn replace_pipe_placeholder(node: &SNode) -> SNode {
    let new_node = match &node.node {
        Node::Identifier(name) if name == "_" => Node::Identifier("__pipe".into()),
        Node::FunctionCall { name, args } => Node::FunctionCall {
            name: name.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::MethodCall {
            object,
            method,
            args,
        } => Node::MethodCall {
            object: Box::new(replace_pipe_placeholder(object)),
            method: method.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::BinaryOp { op, left, right } => Node::BinaryOp {
            op: op.clone(),
            left: Box::new(replace_pipe_placeholder(left)),
            right: Box::new(replace_pipe_placeholder(right)),
        },
        Node::UnaryOp { op, operand } => Node::UnaryOp {
            op: op.clone(),
            operand: Box::new(replace_pipe_placeholder(operand)),
        },
        Node::ListLiteral(items) => {
            Node::ListLiteral(items.iter().map(replace_pipe_placeholder).collect())
        }
        Node::PropertyAccess { object, property } => Node::PropertyAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            property: property.clone(),
        },
        Node::SubscriptAccess { object, index } => Node::SubscriptAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            index: Box::new(replace_pipe_placeholder(index)),
        },
        _ => return node.clone(),
    };
    SNode::new(new_node, node.span)
}
