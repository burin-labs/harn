//! Building a handler's IR graph from its AST.
//!
//! `HandlerIrBuilder` walks the handler body, emitting nodes and the edges
//! between them. Expression-level walking lives in the sibling `builder_expr`
//! module, which extends this same type.

use harn_lexer::Span;
use harn_parser::{BindingPattern, HitlArg, HitlKind, Node, SNode};

use crate::classify::*;
use crate::types::*;
pub(crate) struct HandlerIrBuilder<'a> {
    handler: &'a HandlerSpec,
    nodes: Vec<IrNode>,
    edges: Vec<IrEdge>,
}

impl<'a> HandlerIrBuilder<'a> {
    pub(crate) fn new(handler: &'a HandlerSpec) -> Self {
        Self {
            handler,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub(crate) fn build(mut self) -> HandlerIr {
        let entry = self.push_node(
            self.handler.span,
            "enter handler".to_string(),
            NodeSemantics::Start,
        );
        let exit = self.push_node(
            self.handler.span,
            "exit handler".to_string(),
            NodeSemantics::Exit,
        );
        let exits = self.build_block(&self.handler.body, vec![entry]);
        self.connect_all(&exits, exit);
        HandlerIr {
            name: self.handler.name.clone(),
            kind: self.handler.kind,
            span: self.handler.span,
            invariants: self.handler.invariants.clone(),
            entry,
            exit,
            nodes: self.nodes,
            edges: self.edges,
        }
    }

    pub(crate) fn push_node(
        &mut self,
        span: Span,
        label: String,
        semantics: NodeSemantics,
    ) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(IrNode {
            id,
            span,
            label,
            semantics,
        });
        id
    }

    pub(crate) fn connect(&mut self, from: NodeId, to: NodeId) {
        self.edges.push(IrEdge { from, to });
    }

    pub(crate) fn connect_all(&mut self, from: &[NodeId], to: NodeId) {
        for &edge_from in from {
            self.connect(edge_from, to);
        }
    }

    pub(crate) fn build_block(&mut self, nodes: &[SNode], incoming: Vec<NodeId>) -> Vec<NodeId> {
        let mut exits = incoming;
        for node in nodes {
            exits = self.build_stmt(node, exits);
        }
        exits
    }

    pub(crate) fn build_stmt(&mut self, node: &SNode, incoming: Vec<NodeId>) -> Vec<NodeId> {
        match &node.node {
            Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
                let exits = self.build_expr(value, incoming);
                if let BindingPattern::Identifier(name) = pattern {
                    let assignment = self.push_node(
                        node.span,
                        format!("assign {name}"),
                        NodeSemantics::Assignment(AssignmentSemantics {
                            target: Some(name.clone()),
                            op: None,
                            value: expr_summary(value),
                        }),
                    );
                    self.connect_all(&exits, assignment);
                    vec![assignment]
                } else {
                    exits
                }
            }
            Node::Assignment { target, value, op } => {
                let exits = self.build_expr(value, incoming);
                let assignment = self.push_node(
                    node.span,
                    format!(
                        "assign {}",
                        target_path(target).unwrap_or_else(|| "target".to_string())
                    ),
                    NodeSemantics::Assignment(AssignmentSemantics {
                        target: target_path(target),
                        op: op.clone(),
                        value: expr_summary(value),
                    }),
                );
                self.connect_all(&exits, assignment);
                vec![assignment]
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
                ..
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch =
                    self.push_node(node.span, "if condition".to_string(), NodeSemantics::Branch);
                self.connect_all(&cond_exits, branch);

                let then_entry =
                    self.push_node(node.span, "if true".to_string(), NodeSemantics::Marker);
                self.connect(branch, then_entry);
                let mut exits = self.build_block(then_body, vec![then_entry]);

                if let Some(else_body) = else_body {
                    let else_entry =
                        self.push_node(node.span, "if false".to_string(), NodeSemantics::Marker);
                    self.connect(branch, else_entry);
                    exits.extend(self.build_block(else_body, vec![else_entry]));
                } else {
                    let fallthrough =
                        self.push_node(node.span, "if false".to_string(), NodeSemantics::Marker);
                    self.connect(branch, fallthrough);
                    exits.push(fallthrough);
                }

                exits
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "guard condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);

                let success =
                    self.push_node(node.span, "guard passed".to_string(), NodeSemantics::Marker);
                self.connect(branch, success);

                let else_entry =
                    self.push_node(node.span, "guard failed".to_string(), NodeSemantics::Marker);
                self.connect(branch, else_entry);

                let mut exits = vec![success];
                exits.extend(self.build_block(else_body, vec![else_entry]));
                exits
            }
            Node::ForIn { iterable, body, .. } => {
                let iter_exits = self.build_expr(iterable, incoming);
                let branch = self.push_node(
                    node.span,
                    "for-in iteration".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&iter_exits, branch);

                let body_entry =
                    self.push_node(node.span, "for-in body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "for-in exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::WhileLoop { condition, body } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "while condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);

                let body_entry =
                    self.push_node(node.span, "while body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "while exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::Retry { count, body } => {
                let count_exits = self.build_expr(count, incoming);
                let branch = self.push_node(
                    node.span,
                    "retry iteration".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&count_exits, branch);

                let body_entry =
                    self.push_node(node.span, "retry body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "retry exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::Parallel { expr, body, .. } => {
                let expr_exits = self.build_expr(expr, incoming);
                let branch = self.push_node(
                    node.span,
                    "parallel dispatch".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&expr_exits, branch);
                let body_entry = self.push_node(
                    node.span,
                    "parallel body".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                let after = self.push_node(
                    node.span,
                    "parallel join".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect_all(&body_exits, after);
                self.connect(branch, after);
                vec![after]
            }
            Node::MatchExpr { value, arms } => {
                let value_exits = self.build_expr(value, incoming);
                let branch =
                    self.push_node(node.span, "match value".to_string(), NodeSemantics::Branch);
                self.connect_all(&value_exits, branch);
                let mut exits = Vec::new();
                for arm in arms {
                    let entry = self.push_node(
                        arm.pattern.span,
                        format!("match arm {}", pattern_label(&arm.pattern)),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, entry);
                    let arm_exits = if let Some(guard) = &arm.guard {
                        self.build_expr(guard, vec![entry])
                    } else {
                        vec![entry]
                    };
                    exits.extend(self.build_block(&arm.body, arm_exits));
                }
                exits
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                let branch =
                    self.push_node(node.span, "try dispatch".to_string(), NodeSemantics::Branch);
                self.connect_all(&incoming, branch);

                let try_entry =
                    self.push_node(node.span, "try body".to_string(), NodeSemantics::Marker);
                self.connect(branch, try_entry);
                let mut exits = self.build_block(body, vec![try_entry]);

                let catch_entry =
                    self.push_node(node.span, "catch body".to_string(), NodeSemantics::Marker);
                self.connect(branch, catch_entry);
                exits.extend(self.build_block(catch_body, vec![catch_entry]));

                if let Some(finally_body) = finally_body {
                    let finally_entry = self.push_node(
                        node.span,
                        "finally body".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect_all(&exits, finally_entry);
                    return self.build_block(finally_body, vec![finally_entry]);
                }

                exits
            }
            Node::TryExpr { body }
            | Node::SpawnExpr { body }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body, .. }
            | Node::Block(body) => self.build_block(body, incoming),
            Node::DeadlineBlock { duration, body } => {
                let duration_exits = self.build_expr(duration, incoming);
                self.build_block(body, duration_exits)
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let branch = self.push_node(node.span, "select".to_string(), NodeSemantics::Branch);
                self.connect_all(&incoming, branch);
                let mut exits = Vec::new();
                for case in cases {
                    let case_entry = self.push_node(
                        case.channel.span,
                        format!("select case {}", case.variable),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, case_entry);
                    let case_exits = self.build_expr(&case.channel, vec![case_entry]);
                    exits.extend(self.build_block(&case.body, case_exits));
                }
                if let Some((timeout_expr, timeout_body)) = timeout {
                    let timeout_entry = self.push_node(
                        timeout_expr.span,
                        "select timeout".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, timeout_entry);
                    let timeout_exits = self.build_expr(timeout_expr, vec![timeout_entry]);
                    exits.extend(self.build_block(timeout_body, timeout_exits));
                }
                if let Some(default_body) = default_body {
                    let default_entry = self.push_node(
                        node.span,
                        "select default".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, default_entry);
                    exits.extend(self.build_block(default_body, vec![default_entry]));
                }
                exits
            }
            Node::ReturnStmt { value } => {
                let exits = if let Some(value) = value.as_ref() {
                    self.build_expr(value, incoming)
                } else {
                    incoming
                };
                let ret = self.push_node(node.span, "return".to_string(), NodeSemantics::Return);
                self.connect_all(&exits, ret);
                Vec::new()
            }
            Node::ThrowStmt { value } => {
                let exits = self.build_expr(value, incoming);
                let throw = self.push_node(node.span, "throw".to_string(), NodeSemantics::Throw);
                self.connect_all(&exits, throw);
                Vec::new()
            }
            _ => self.build_expr(node, incoming),
        }
    }

    pub(crate) fn build_function_call(
        &mut self,
        node: &SNode,
        name: &str,
        args: &[SNode],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        if name == "dual_control" {
            let mut exits = incoming;
            for (index, arg) in args.iter().enumerate() {
                if index == 2 && matches!(arg.node, Node::Closure { .. }) {
                    continue;
                }
                exits = self.build_expr(arg, exits);
            }
            let enter = self.push_node(
                node.span,
                "dual_control approval gate".to_string(),
                NodeSemantics::ApprovalScopeEnter,
            );
            self.connect_all(&exits, enter);
            let closure_exits = match args.get(2) {
                Some(SNode {
                    node: Node::Closure { body, .. },
                    ..
                }) => self.build_block(body, vec![enter]),
                _ => vec![enter],
            };
            let exit = self.push_node(
                node.span,
                "end dual_control".to_string(),
                NodeSemantics::ApprovalScopeExit,
            );
            self.connect_all(&closure_exits, exit);
            return vec![exit];
        }

        if let Some(scope) = scoped_policy_call(name) {
            return self.build_policy_scope_call(node, args, incoming, scope);
        }

        let mut exits = incoming;
        for arg in args {
            exits = self.build_expr(arg, exits);
        }
        let call = classify_call(name, args);
        let call_id = self.push_node(
            node.span,
            format!("call {}", call.display_name),
            NodeSemantics::Call(call),
        );
        self.connect_all(&exits, call_id);
        vec![call_id]
    }

    /// Lower a method call. Harness methods are attributed directly from the
    /// typed builtin contract; arbitrary method calls remain pass-through.
    pub(crate) fn build_method_call(
        &mut self,
        node: &SNode,
        object: &SNode,
        method: &str,
        args: &[SNode],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        let mut exits = self.build_expr(object, incoming);
        for arg in args {
            exits = self.build_expr(arg, exits);
        }
        if let Some((sub_handle, entry)) = harness_sub_handle_for(object, method) {
            let display_name = format!("harness.{sub_handle}.{method}");
            let literal_args = literal_args(args);
            let call = CallSemantics {
                name: display_name.clone(),
                display_name,
                classification: classify_contract(entry.name, &entry.contract, &literal_args),
                literal_args,
            };
            let call_id = self.push_node(
                node.span,
                format!("call {}", call.display_name),
                NodeSemantics::Call(call),
            );
            self.connect_all(&exits, call_id);
            return vec![call_id];
        }
        exits
    }

    fn build_policy_scope_call(
        &mut self,
        node: &SNode,
        args: &[SNode],
        incoming: Vec<NodeId>,
        scope: PolicyScopeKind,
    ) -> Vec<NodeId> {
        let closure_index = 1;
        let mut exits = incoming;
        for (index, arg) in args.iter().enumerate() {
            if index == closure_index && matches!(arg.node, Node::Closure { .. }) {
                continue;
            }
            exits = self.build_expr(arg, exits);
        }
        let enter = self.push_node(
            node.span,
            format!("enter {}", scope.label()),
            NodeSemantics::PolicyScopeEnter(scope),
        );
        self.connect_all(&exits, enter);
        let closure_exits = match args.get(closure_index) {
            Some(SNode {
                node: Node::Closure { body, .. },
                ..
            }) => self.build_block(body, vec![enter]),
            _ => vec![enter],
        };
        let exit = self.push_node(
            node.span,
            format!("exit {}", scope.label()),
            NodeSemantics::PolicyScopeExit(scope),
        );
        self.connect_all(&closure_exits, exit);
        vec![exit]
    }

    pub(crate) fn build_hitl_expr(
        &mut self,
        node: &SNode,
        kind: HitlKind,
        args: &[HitlArg],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        match kind {
            HitlKind::RequestApproval => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(&arg.value, exits);
                }
                let call = CallSemantics {
                    name: kind.as_keyword().to_string(),
                    display_name: kind.as_keyword().to_string(),
                    classification: CallClassification::ApprovalGate,
                    literal_args: args
                        .iter()
                        .map(|arg| literal_value(&arg.value))
                        .collect::<Vec<_>>(),
                };
                let call_id = self.push_node(
                    node.span,
                    format!("call {}", kind.as_keyword()),
                    NodeSemantics::Call(call),
                );
                self.connect_all(&exits, call_id);
                vec![call_id]
            }
            HitlKind::DualControl => self.build_hitl_dual_control(node, args, incoming),
            HitlKind::AskUser | HitlKind::EscalateTo => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(&arg.value, exits);
                }
                exits
            }
        }
    }

    fn build_hitl_dual_control(
        &mut self,
        node: &SNode,
        args: &[HitlArg],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        let closure_index = args
            .iter()
            .position(|arg| arg.name.as_deref() == Some("action"))
            .or(Some(2));
        let mut exits = incoming;
        for (index, arg) in args.iter().enumerate() {
            if Some(index) == closure_index && matches!(arg.value.node, Node::Closure { .. }) {
                continue;
            }
            exits = self.build_expr(&arg.value, exits);
        }
        let enter = self.push_node(
            node.span,
            "dual_control approval gate".to_string(),
            NodeSemantics::ApprovalScopeEnter,
        );
        self.connect_all(&exits, enter);
        let closure_exits = closure_index
            .and_then(|index| args.get(index))
            .and_then(|arg| match &arg.value {
                SNode {
                    node: Node::Closure { body, .. },
                    ..
                } => Some(self.build_block(body, vec![enter])),
                _ => None,
            })
            .unwrap_or_else(|| vec![enter]);
        let exit = self.push_node(
            node.span,
            "end dual_control".to_string(),
            NodeSemantics::ApprovalScopeExit,
        );
        self.connect_all(&closure_exits, exit);
        vec![exit]
    }
}
