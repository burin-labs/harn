use harn_parser::{DictEntry, HitlArg, Node, SNode};

/// Check if an AST node contains `_` identifier (pipe placeholder).
pub(super) fn contains_pipe_placeholder(node: &SNode) -> bool {
    match &node.node {
        Node::Identifier(name) if name == "_" => true,
        Node::FunctionCall { args, .. } => args.iter().any(contains_pipe_placeholder),
        Node::ValueCall { callee, args } => {
            contains_pipe_placeholder(callee) || args.iter().any(contains_pipe_placeholder)
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            contains_pipe_placeholder(object) || args.iter().any(contains_pipe_placeholder)
        }
        Node::HitlExpr { args, .. } => args.iter().any(|arg| contains_pipe_placeholder(&arg.value)),
        Node::BinaryOp { left, right, .. } => {
            contains_pipe_placeholder(left) || contains_pipe_placeholder(right)
        }
        Node::UnaryOp { operand, .. } => contains_pipe_placeholder(operand),
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            contains_pipe_placeholder(condition)
                || contains_pipe_placeholder(true_expr)
                || contains_pipe_placeholder(false_expr)
        }
        Node::Assignment { target, value, .. } => {
            contains_pipe_placeholder(target) || contains_pipe_placeholder(value)
        }
        Node::RangeExpr { start, end, .. } => {
            contains_pipe_placeholder(start) || contains_pipe_placeholder(end)
        }
        Node::ListLiteral(items) => items.iter().any(contains_pipe_placeholder),
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => entries.iter().any(contains_pipe_placeholder_in_entry),
        Node::EnumConstruct { args, .. } => args.iter().any(contains_pipe_placeholder),
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            contains_pipe_placeholder(object)
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            contains_pipe_placeholder(object) || contains_pipe_placeholder(index)
        }
        Node::SliceAccess { object, start, end } => {
            contains_pipe_placeholder(object)
                || start
                    .as_ref()
                    .is_some_and(|start| contains_pipe_placeholder(start))
                || end
                    .as_ref()
                    .is_some_and(|end| contains_pipe_placeholder(end))
        }
        Node::Spread(inner)
        | Node::TryOperator { operand: inner }
        | Node::TryStar { operand: inner } => contains_pipe_placeholder(inner),
        _ => false,
    }
}

/// Replace all `_` identifiers with `__pipe` in an AST node (for pipe placeholder desugaring).
pub(super) fn replace_pipe_placeholder(node: &SNode) -> SNode {
    let new_node = match &node.node {
        Node::Identifier(name) if name == "_" => Node::Identifier("__pipe".into()),
        Node::FunctionCall {
            name,
            type_args,
            args,
        } => Node::FunctionCall {
            name: name.clone(),
            type_args: type_args.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::ValueCall { callee, args } => Node::ValueCall {
            callee: Box::new(replace_pipe_placeholder(callee)),
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
        Node::OptionalMethodCall {
            object,
            method,
            args,
        } => Node::OptionalMethodCall {
            object: Box::new(replace_pipe_placeholder(object)),
            method: method.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::HitlExpr { kind, args } => Node::HitlExpr {
            kind: *kind,
            args: args
                .iter()
                .map(|arg| HitlArg {
                    name: arg.name.clone(),
                    value: replace_pipe_placeholder(&arg.value),
                    span: arg.span,
                })
                .collect(),
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
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => Node::Ternary {
            condition: Box::new(replace_pipe_placeholder(condition)),
            true_expr: Box::new(replace_pipe_placeholder(true_expr)),
            false_expr: Box::new(replace_pipe_placeholder(false_expr)),
        },
        Node::Assignment { target, value, op } => Node::Assignment {
            target: Box::new(replace_pipe_placeholder(target)),
            value: Box::new(replace_pipe_placeholder(value)),
            op: op.clone(),
        },
        Node::RangeExpr {
            start,
            end,
            inclusive,
        } => Node::RangeExpr {
            start: Box::new(replace_pipe_placeholder(start)),
            end: Box::new(replace_pipe_placeholder(end)),
            inclusive: *inclusive,
        },
        Node::ListLiteral(items) => {
            Node::ListLiteral(items.iter().map(replace_pipe_placeholder).collect())
        }
        Node::DictLiteral(entries) => {
            Node::DictLiteral(entries.iter().map(replace_pipe_placeholder_entry).collect())
        }
        Node::PropertyAccess { object, property } => Node::PropertyAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            property: property.clone(),
        },
        Node::OptionalPropertyAccess { object, property } => Node::OptionalPropertyAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            property: property.clone(),
        },
        Node::SubscriptAccess { object, index } => Node::SubscriptAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            index: Box::new(replace_pipe_placeholder(index)),
        },
        Node::OptionalSubscriptAccess { object, index } => Node::OptionalSubscriptAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            index: Box::new(replace_pipe_placeholder(index)),
        },
        Node::SliceAccess { object, start, end } => Node::SliceAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            start: start
                .as_ref()
                .map(|start| Box::new(replace_pipe_placeholder(start))),
            end: end
                .as_ref()
                .map(|end| Box::new(replace_pipe_placeholder(end))),
        },
        Node::EnumConstruct {
            enum_name,
            variant,
            args,
        } => Node::EnumConstruct {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::StructConstruct {
            struct_name,
            fields,
        } => Node::StructConstruct {
            struct_name: struct_name.clone(),
            fields: fields.iter().map(replace_pipe_placeholder_entry).collect(),
        },
        Node::Spread(inner) => Node::Spread(Box::new(replace_pipe_placeholder(inner))),
        Node::TryOperator { operand } => Node::TryOperator {
            operand: Box::new(replace_pipe_placeholder(operand)),
        },
        Node::TryStar { operand } => Node::TryStar {
            operand: Box::new(replace_pipe_placeholder(operand)),
        },
        _ => return node.clone(),
    };
    SNode::new(new_node, node.span)
}

fn contains_pipe_placeholder_in_entry(entry: &DictEntry) -> bool {
    contains_pipe_placeholder(&entry.key) || contains_pipe_placeholder(&entry.value)
}

fn replace_pipe_placeholder_entry(entry: &DictEntry) -> DictEntry {
    DictEntry {
        key: replace_pipe_placeholder(&entry.key),
        value: replace_pipe_placeholder(&entry.value),
    }
}
