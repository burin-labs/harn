//! Lowering for Swift-style mutating method calls (`xs.push(v)`,
//! `stack.pop()`, `seen.add(k)`).
//!
//! Harn values stay immutable. A mutating call on a place is sugar for
//! computing the receiver's next value and writing it back to that place:
//!
//! ```text
//! recv = <place>; args = ...
//! r = __method_mutate(recv, "pop", args...)   // [next, result] or nil
//! if r { <place> = r[0]; r[1] } else { recv.pop(args...) }
//! ```
//!
//! `__method_mutate` answers `nil` when the receiver is not a builtin value
//! with that mutating method (a struct whose impl defines `push`, a dict
//! holding a callable `remove`), and the call falls back to an ordinary
//! method call without writing anything back.

use harn_parser::{Node, SNode, TypeExpr};

use crate::chunk::Op;

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    /// True when `object.method(...)` lowers as a mutating call: the method
    /// name is mutating on some builtin receiver, the receiver is a place,
    /// and its root is not a binding the compiler knows to be immutable.
    pub(super) fn is_mutating_method_call(&self, object: &SNode, method: &str) -> bool {
        if !harn_parser::builtin_methods::is_mutating_method_name(method) {
            return false;
        }
        let Some(root) = harn_parser::builtin_methods::place_root(object) else {
            return false;
        };
        self.local_binding_mutability(root) != Some(false)
    }

    pub(super) fn compile_mutating_method_call(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
    ) -> Result<(), CompileError> {
        let root_type = match &object.node {
            Node::Identifier(_) => self.infer_expr_type(object),
            _ => None,
        };
        // `xs.push(v)` on a local list appends in place, like `xs = xs + [v]`.
        if let (Node::Identifier(name), "push", [item]) = (&object.node, method, args) {
            if self.try_emit_list_append_assign(name, item, root_type.as_ref())? {
                self.chunk.emit(Op::Nil, self.line);
                self.keep_mutated_receiver_type(name, root_type, method, args);
                return Ok(());
            }
        }

        self.temp_counter += 1;
        let id = self.temp_counter;
        self.begin_scope();
        let place = self.hoist_place_indices(object, id)?;
        let span = object.span;
        let ident = |name: &str| SNode::new(Node::Identifier(name.to_string()), span);

        let receiver = format!("__mut{id}_recv__");
        self.compile_node(&place)?;
        self.emit_define_binding(&receiver, false);
        let mut arg_names = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let name = format!("__mut{id}_a{i}__");
            self.compile_node(arg)?;
            self.emit_define_binding(&name, false);
            arg_names.push(name);
        }

        let outcome = format!("__mut{id}_r__");
        self.emit_get_binding(&receiver);
        let method_idx = self.string_constant(method);
        self.chunk.emit_u16(Op::Constant, method_idx, self.line);
        for name in &arg_names {
            self.emit_get_binding(name);
        }
        self.emit_named_call("__method_mutate", arg_names.len() + 2);
        self.emit_define_binding(&outcome, false);

        let element = |index: i64| {
            SNode::new(
                Node::SubscriptAccess {
                    object: Box::new(ident(&outcome)),
                    index: Box::new(SNode::new(Node::IntLiteral(index), span)),
                },
                span,
            )
        };
        self.emit_get_binding(&outcome);
        let fallback = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
        self.chunk.emit(Op::Pop, self.line);
        // A bare binding is written directly so the assignment does not
        // forget the binding's type facts; the receiver kind is unchanged.
        if let Node::Identifier(name) = &place.node {
            self.compile_node(&element(0))?;
            self.emit_set_binding(name);
        } else {
            self.compile_assignment(&place, &element(0), &None)?;
        }
        self.compile_node(&element(1))?;
        let end = self.chunk.emit_jump(Op::Jump, self.line);
        self.chunk.patch_jump(fallback);
        self.chunk.emit(Op::Pop, self.line);
        let arg_nodes: Vec<SNode> = arg_names.iter().map(|name| ident(name)).collect();
        self.compile_plain_method_call(&ident(&receiver), method, &arg_nodes)?;
        self.chunk.patch_jump(end);
        self.end_scope();

        if let Node::Identifier(name) = &object.node {
            self.keep_mutated_receiver_type(name, root_type, method, args);
        }
        Ok(())
    }

    /// Rewrite each effectful index in a place to a temp so the place is read
    /// and written at the same location with every index evaluated once.
    fn hoist_place_indices(&mut self, place: &SNode, id: usize) -> Result<SNode, CompileError> {
        let node = match &place.node {
            Node::PropertyAccess { object, property } => Node::PropertyAccess {
                object: Box::new(self.hoist_place_indices(object, id)?),
                property: property.clone(),
            },
            Node::SubscriptAccess { object, index } => {
                let object = self.hoist_place_indices(object, id)?;
                let index = if Self::is_effect_free_index(index) {
                    (**index).clone()
                } else {
                    let name = format!("__mut{id}_k{}__", index.span.start);
                    self.compile_node(index)?;
                    self.emit_define_binding(&name, false);
                    SNode::new(Node::Identifier(name), index.span)
                };
                Node::SubscriptAccess {
                    object: Box::new(object),
                    index: Box::new(index),
                }
            }
            other => other.clone(),
        };
        Ok(SNode::new(node, place.span))
    }

    /// A mutation keeps a list a list. Element-specialized facts survive only
    /// when no element of another type can have been added.
    fn keep_mutated_receiver_type(
        &mut self,
        name: &str,
        before: Option<TypeExpr>,
        method: &str,
        args: &[SNode],
    ) {
        let Some(before) = before else {
            return;
        };
        if !Self::is_list_type(Some(&before)) {
            return;
        }
        let element = match &before {
            TypeExpr::List(element) => Some((**element).clone()),
            _ => None,
        };
        let added = match (method, args) {
            ("push", [item]) | ("insert", [_, item]) => Some(self.infer_expr_type(item)),
            ("extend", [items]) => Some(match self.infer_expr_type(items) {
                Some(TypeExpr::List(inner)) => Some(*inner),
                _ => None,
            }),
            _ => None,
        };
        let after = match (added, element) {
            (Some(added), Some(element)) if added.as_ref() != Some(&element) => {
                TypeExpr::Named("list".into())
            }
            _ => before,
        };
        self.assign_type_fact(name, Some(after));
    }
}
