use super::*;

impl<'a> HandlerIrBuilder<'a> {
    pub(super) fn build_expr(&mut self, node: &SNode, incoming: Vec<NodeId>) -> Vec<NodeId> {
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                self.build_function_call(node, name, args, incoming)
            }
            Node::ValueCall { callee, args } => {
                let mut exits = self.build_expr(callee, incoming);
                for arg in args {
                    exits = self.build_expr(arg, exits);
                }
                exits
            }
            Node::HitlExpr { kind, args } => self.build_hitl_expr(node, *kind, args, incoming),
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => self.build_method_call(node, object, method, args, incoming),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::Spread(object)
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::UnaryOp {
                operand: object, ..
            } => self.build_expr(object, incoming),
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                let exits = self.build_expr(object, incoming);
                self.build_expr(index, exits)
            }
            Node::SliceAccess { object, start, end } => {
                let mut exits = self.build_expr(object, incoming);
                if let Some(start) = start {
                    exits = self.build_expr(start, exits);
                }
                if let Some(end) = end {
                    exits = self.build_expr(end, exits);
                }
                exits
            }
            Node::BinaryOp { left, right, .. } => {
                let exits = self.build_expr(left, incoming);
                self.build_expr(right, exits)
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "ternary condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);
                let true_entry =
                    self.push_node(node.span, "ternary true".to_string(), NodeSemantics::Marker);
                self.connect(branch, true_entry);
                let false_entry = self.push_node(
                    node.span,
                    "ternary false".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect(branch, false_entry);
                let mut exits = self.build_expr(true_expr, vec![true_entry]);
                exits.extend(self.build_expr(false_expr, vec![false_entry]));
                exits
            }
            Node::ListLiteral(items) | Node::OrPattern(items) => {
                let mut exits = incoming;
                for item in items {
                    exits = self.build_expr(item, exits);
                }
                exits
            }
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => {
                let mut exits = incoming;
                for entry in entries {
                    exits = self.build_expr(&entry.key, exits);
                    exits = self.build_expr(&entry.value, exits);
                }
                exits
            }
            Node::EnumConstruct { args, .. } => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(arg, exits);
                }
                exits
            }
            Node::Block(body) => self.build_block(body, incoming),
            Node::MatchExpr { .. } => self.build_stmt(node, incoming),
            Node::Closure { .. } => incoming,
            _ => incoming,
        }
    }
}
