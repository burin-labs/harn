use harn_parser::{Node, SNode, TypeExpr};

use crate::chunk::Op;

use super::error::CompileError;
use super::pipe::contains_pipe_placeholder;
use super::{Compiler, LoopContext};

impl Compiler {
    pub(super) fn compile_assignment(
        &mut self,
        target: &SNode,
        value: &SNode,
        op: &Option<String>,
    ) -> Result<(), CompileError> {
        if let Node::Identifier(name) = &target.node {
            if let Some(op) = op {
                let left_type = self.infer_expr_type(target);
                let right_type = self.infer_expr_type(value);
                let result_type =
                    self.infer_binary_result_type(op, left_type.as_ref(), right_type.as_ref());
                // `x += list` — in-place concat (see try_emit_inplace_concat_assign).
                if op == "+"
                    && self.try_emit_inplace_concat_assign(name, value, left_type.as_ref())?
                {
                    self.assign_type_fact(name, result_type);
                    return Ok(());
                }
                self.emit_get_binding(name);
                self.compile_node(value)?;
                if let Some(typed_op) = self
                    .options
                    .optimizations_enabled()
                    .then(|| {
                        self.specialized_binary_op(op, left_type.as_ref(), right_type.as_ref())
                    })
                    .flatten()
                {
                    self.chunk.emit(typed_op, self.line);
                } else {
                    self.emit_compound_op(op)?;
                }
                self.emit_set_binding(name);
                self.assign_type_fact(name, result_type);
            } else {
                // `x = x + list` — in-place concat (the common accumulator
                // idiom). Detect the `Binary(+, x, e)` shape with the same
                // target on the left and route to the in-place emitter.
                if let Node::BinaryOp {
                    op: bop,
                    left,
                    right,
                } = &value.node
                {
                    if bop == "+" {
                        if let Node::Identifier(lname) = &left.node {
                            if lname == name {
                                let left_type = self.infer_expr_type(target);
                                let value_type = self.infer_expr_type(value);
                                if self.try_emit_inplace_concat_assign(
                                    name,
                                    right,
                                    left_type.as_ref(),
                                )? {
                                    self.assign_type_fact(name, value_type);
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                // `x = x.push(v)` is the method spelling of the same list
                // accumulator append. When the receiver is statically known to
                // be a list and the target is a local slot, lower it through
                // the same fused concat opcode as `x = x + [v]`.
                if let Node::MethodCall {
                    object,
                    method,
                    args,
                } = &value.node
                {
                    if method == "push" && args.len() == 1 {
                        if let Node::Identifier(oname) = &object.node {
                            if oname == name {
                                let left_type = self.infer_expr_type(target);
                                let value_type = self.infer_expr_type(value);
                                if self.try_emit_list_push_assign(
                                    name,
                                    &args[0],
                                    left_type.as_ref(),
                                )? {
                                    self.assign_type_fact(name, value_type);
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                let value_type = self.infer_expr_type(value);
                self.compile_node(value)?;
                self.emit_set_binding(name);
                self.assign_type_fact(name, value_type);
            }
        } else if let Node::PropertyAccess { object, property } = &target.node {
            if let Node::Identifier(var_name) = &object.node {
                // Single-level `x.prop (op)= value` can store straight into
                // a resolved local slot; non-locals keep the env-aware opcode.
                let prop_idx = self.string_constant(property);
                if let Some(op) = op {
                    self.compile_node(target)?;
                    self.compile_node(value)?;
                    self.emit_compound_op(op)?;
                } else {
                    self.compile_node(value)?;
                }
                if let Some(slot) = self.resolve_local_slot(var_name) {
                    self.chunk
                        .emit_set_local_slot_property(prop_idx, slot, self.line);
                } else {
                    // The variable name index is encoded as a second u16.
                    let var_idx = self.string_constant(var_name);
                    self.chunk.emit_u16(Op::SetProperty, prop_idx, self.line);
                    let hi = (var_idx >> 8) as u8;
                    let lo = var_idx as u8;
                    self.chunk.code.push(hi);
                    self.chunk.code.push(lo);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                }
            } else {
                self.compile_path_assignment(target, value, op)?;
            }
        } else if let Node::SubscriptAccess { object, index } = &target.node {
            if let Node::Identifier(var_name) = &object.node {
                // Single-level `x[index] (op)= value`. A compound op reads the
                // target before writing it, and both the read and the write
                // need the index — so an index with potential side effects is
                // hoisted into a temp to keep it evaluated exactly once.
                if op.is_some() && !Self::is_effect_free_index(index) {
                    self.compile_path_assignment(target, value, op)?;
                } else {
                    if let Some(op) = op {
                        self.compile_node(target)?;
                        self.compile_node(value)?;
                        self.emit_compound_op(op)?;
                    } else {
                        self.compile_node(value)?;
                    }
                    self.compile_node(index)?;
                    if let Some(slot) = self.resolve_local_slot(var_name) {
                        self.chunk
                            .emit_u16(Op::SetLocalSlotSubscript, slot, self.line);
                    } else {
                        let var_idx = self.string_constant(var_name);
                        self.chunk.emit_u16(Op::SetSubscript, var_idx, self.line);
                    }
                }
            } else {
                self.compile_path_assignment(target, value, op)?;
            }
        } else {
            return Err(CompileError {
                message: format!(
                    "invalid assignment target: expected a variable, property path \
                     (`x.a.b`), or subscript path (`x[i][j]`), found {}",
                    Self::describe_assignment_target(&target.node)
                ),
                line: target.span.line as u32,
            });
        }
        Ok(())
    }

    /// True when re-evaluating `index` cannot run user code or change between
    /// the read and write halves of a compound assignment.
    fn is_effect_free_index(index: &SNode) -> bool {
        matches!(
            index.node,
            Node::Identifier(_)
                | Node::IntLiteral(_)
                | Node::FloatLiteral(_)
                | Node::StringLiteral(_)
                | Node::RawStringLiteral(_)
                | Node::BoolLiteral(_)
        )
    }

    /// Human description of a non-assignable target for the compile error.
    fn describe_assignment_target(node: &Node) -> &'static str {
        match node {
            Node::FunctionCall { .. } => "a function call result",
            Node::MethodCall { .. } | Node::OptionalMethodCall { .. } => "a method call result",
            Node::SliceAccess { .. } => "a slice",
            Node::OptionalPropertyAccess { .. } | Node::OptionalSubscriptAccess { .. } => {
                "an optional-chained access (`?.` / `?[]`)"
            }
            _ => "an expression",
        }
    }

    /// Lower an assignment through a nested access path (`a.b.c = v`,
    /// `a["b"]["c"] = v`, `xs[0][1] = v`, `m.list[0] = v`, …) or a
    /// single-level compound subscript whose index has side effects.
    ///
    /// `SetProperty`/`SetSubscript` only know how to write one level deep on
    /// a named binding, so deeper paths are desugared into a scope-contained
    /// chain of temporaries: hoist each computed index once, copy each path
    /// prefix into a temp, perform the single-level leaf assignment on the
    /// innermost temp, then write each temp back into its parent. (The naive
    /// lowering used to collapse the path to its root variable and final
    /// accessor, so `a.b.c = v` silently wrote `a.c`.)
    fn compile_path_assignment(
        &mut self,
        target: &SNode,
        value: &SNode,
        op: &Option<String>,
    ) -> Result<(), CompileError> {
        // `Index` carries a full `SNode`; the size gap to `Prop(String)` is
        // inherent to this short-lived path-desugaring helper, not worth an
        // allocation to box away.
        #[allow(clippy::large_enum_variant)]
        enum Seg {
            Prop(String),
            Index(SNode),
        }

        // Collect the access path, root-first. Anything that is not a plain
        // property/subscript chain rooted at an identifier is not assignable.
        let mut segs: Vec<Seg> = Vec::new();
        let mut cur = target;
        let root = loop {
            match &cur.node {
                Node::PropertyAccess { object, property } => {
                    segs.push(Seg::Prop(property.clone()));
                    cur = object;
                }
                Node::SubscriptAccess { object, index } => {
                    segs.push(Seg::Index((**index).clone()));
                    cur = object;
                }
                Node::Identifier(name) => break name.clone(),
                _ => {
                    return Err(CompileError {
                        message: format!(
                            "invalid assignment target: expected a variable, property path \
                             (`x.a.b`), or subscript path (`x[i][j]`), found {}",
                            Self::describe_assignment_target(&cur.node)
                        ),
                        line: cur.span.line as u32,
                    });
                }
            }
        };
        segs.reverse();
        let span = target.span;
        self.temp_counter += 1;
        let id = self.temp_counter;

        // The temps live in their own scope so they never leak into (or
        // shadow anything in) the surrounding code.
        self.begin_scope();

        // 1. Hoist computed indices into temps, in source order, so each
        //    index expression is evaluated exactly once.
        let seg_nodes: Vec<Seg> = segs
            .into_iter()
            .enumerate()
            .map(|(i, seg)| -> Result<Seg, CompileError> {
                match seg {
                    Seg::Prop(p) => Ok(Seg::Prop(p)),
                    Seg::Index(idx) if Self::is_effect_free_index(&idx) => Ok(Seg::Index(idx)),
                    Seg::Index(idx) => {
                        let name = format!("__asg{id}_k{i}__");
                        self.compile_node(&idx)?;
                        self.emit_define_binding(&name, false);
                        Ok(Seg::Index(SNode::new(Node::Identifier(name), idx.span)))
                    }
                }
            })
            .collect::<Result<_, _>>()?;

        let access = |obj_name: &str, seg: &Seg| -> SNode {
            let object = Box::new(SNode::new(Node::Identifier(obj_name.to_string()), span));
            match seg {
                Seg::Prop(p) => SNode::new(
                    Node::PropertyAccess {
                        object,
                        property: p.clone(),
                    },
                    span,
                ),
                Seg::Index(idx) => SNode::new(
                    Node::SubscriptAccess {
                        object,
                        index: Box::new(idx.clone()),
                    },
                    span,
                ),
            }
        };

        // 2. Copy each path prefix into a mutable temp, walking down.
        let n = seg_nodes.len();
        let mut prefix_names: Vec<String> = vec![root];
        for (i, seg) in seg_nodes.iter().enumerate().take(n - 1) {
            let get_expr = access(prefix_names.last().expect("non-empty"), seg);
            self.compile_node(&get_expr)?;
            let name = format!("__asg{id}_t{i}__");
            self.emit_define_binding(&name, true);
            prefix_names.push(name);
        }

        // 3. Single-level leaf assignment on the innermost temp (or the root
        //    itself when the only reason we are here is an effectful index).
        let leaf_target = access(prefix_names.last().expect("non-empty"), &seg_nodes[n - 1]);
        self.compile_assignment(&leaf_target, value, op)?;

        // 4. Write each temp back into its parent, walking up.
        for i in (0..n - 1).rev() {
            let wb_target = access(&prefix_names[i], &seg_nodes[i]);
            let wb_value = SNode::new(Node::Identifier(prefix_names[i + 1].clone()), span);
            self.compile_assignment(&wb_target, &wb_value, &None)?;
        }

        self.end_scope();
        Ok(())
    }

    /// Emit an in-place list/dict concat for `x = x + e` and `x += e`.
    ///
    /// The runtime `+` for two lists already extends the left operand's
    /// `Vec` in place when its `Arc` is uniquely held (`Arc::try_unwrap`).
    /// In the naive emission that uniqueness never holds: the accumulator's
    /// binding keeps one reference while the operand-stack copy holds the
    /// other, so every `+` clones the whole list — turning the ubiquitous
    /// `out = out + [item]` loop into O(n^2).
    ///
    /// Here we clear the binding's reference (`Nil; SetBinding`) *after* `e`
    /// is evaluated and *before* `Add`, so at concat time the value on the
    /// stack is the sole owner and `try_unwrap` extends in place — O(1)
    /// amortized. `e` is compiled while the binding is still live, so an
    /// aliasing right-hand side (e.g. `x = x + x`) still observes the real
    /// `x`; in that case the value is shared, `try_unwrap` fails, and the
    /// runtime safely falls back to a clone.
    ///
    /// Gated to list/dict-typed operands so the scalar `i += 1` / `sum += x`
    /// hot path keeps its specialized typed opcode and pays nothing for this.
    /// Returns `Ok(true)` when the optimized form was emitted (the caller is
    /// done) and `Ok(false)` when it does not apply.
    fn try_emit_inplace_concat_assign(
        &mut self,
        name: &str,
        rhs: &SNode,
        left_type: Option<&TypeExpr>,
    ) -> Result<bool, CompileError> {
        fn is_collection(t: Option<&TypeExpr>) -> bool {
            matches!(t, Some(TypeExpr::List(_)) | Some(TypeExpr::DictType(_, _)))
                || matches!(t, Some(TypeExpr::Named(n)) if n == "list" || n == "dict")
        }
        // A type whose `+` should keep its specialized scalar opcode
        // (`i += 1`, `total += x`) rather than route through the collection
        // concat path. `None` (unknown) is deliberately excluded so untyped
        // accumulators still reach the in-place opcode.
        fn is_known_scalar(t: Option<&TypeExpr>) -> bool {
            matches!(
                t,
                Some(TypeExpr::Named(n))
                    if matches!(
                        n.as_str(),
                        "int" | "float" | "bool" | "string" | "nil" | "decimal" | "duration"
                    )
            )
        }
        if !self.options.optimizations_enabled() {
            return Ok(false);
        }
        let rhs_type = self.infer_expr_type(rhs);

        // Local-slot accumulator: emit the fused `ConcatAssignLocal` opcode.
        // It reads the slot at runtime and only takes the value in place when
        // it is actually a List/Dict, so dynamically-typed accumulators get
        // the amortized-O(n) in-place extend too — not just statically-typed
        // ones — while a throwing add on a scalar leaves the binding intact.
        // Skip only when both operands are known scalars so the specialized
        // numeric opcode keeps its fast lane.
        if let Some(slot) = self.resolve_local_slot(name) {
            if is_known_scalar(left_type) && is_known_scalar(rhs_type.as_ref()) {
                return Ok(false);
            }
            self.compile_node(rhs)?; // [e]  (slot live: aliasing rhs sees real x)
            self.chunk.emit_u16(Op::ConcatAssignLocal, slot, self.line); // x = x + e
            return Ok(true);
        }

        // Non-local binding (module-level `let`, global): no slot to take, so
        // fall back to clearing the binding's reference between `e` and `Add`
        // so the runtime `try_unwrap` fast path can still fire. Gated to
        // statically-known collections to preserve existing behavior.
        if !is_collection(left_type) && !is_collection(rhs_type.as_ref()) {
            return Ok(false);
        }
        self.emit_get_binding(name); // [x]
        self.compile_node(rhs)?; // [x, e]  (binding live: aliasing rhs sees real x)
        self.chunk.emit(Op::Nil, self.line); // [x, e, nil]
        self.emit_set_binding(name); // [x, e]  binding <- nil; x uniquely held if unaliased
        self.chunk.emit(Op::Add, self.line); // [x + e]  in-place extend when unique
        self.emit_set_binding(name); // []
        Ok(true)
    }

    /// Emit the fused local concat form for `x = x.push(item)`.
    ///
    /// This deliberately stays narrower than the runtime method: it only fires
    /// for a local receiver that is statically list-like and for exactly one
    /// non-spread argument. The optimized bytecode evaluates `item` while the
    /// original slot is still live, builds `[item]`, then lets
    /// `ConcatAssignLocal` perform the same immutable list append as
    /// `x = x + [item]`, including alias-safe fallback cloning.
    fn try_emit_list_push_assign(
        &mut self,
        name: &str,
        item: &SNode,
        left_type: Option<&TypeExpr>,
    ) -> Result<bool, CompileError> {
        fn is_list(t: Option<&TypeExpr>) -> bool {
            matches!(t, Some(TypeExpr::List(_)))
                || matches!(t, Some(TypeExpr::Named(n)) if n == "list")
        }
        if !self.options.optimizations_enabled() || !is_list(left_type) {
            return Ok(false);
        }
        if matches!(item.node, Node::Spread(_)) {
            return Ok(false);
        }
        let Some(slot) = self.resolve_local_slot(name) else {
            return Ok(false);
        };
        self.compile_node(item)?;
        self.chunk.emit_u16(Op::BuildList, 1, self.line);
        self.chunk.emit_u16(Op::ConcatAssignLocal, slot, self.line);
        Ok(true)
    }

    pub(super) fn compile_if_else(
        &mut self,
        condition: &SNode,
        then_body: &[SNode],
        else_body: &Option<Vec<SNode>>,
    ) -> Result<(), CompileError> {
        self.compile_node(condition)?;
        let else_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
        self.chunk.emit(Op::Pop, self.line);
        self.compile_scoped_block(then_body)?;
        // The branch always leaves exactly one value on the stack, so
        // the truthy path must skip the falsy cleanup unconditionally.
        // Without the unconditional jump on the no-else path, control
        // fell through into the `Pop; Nil` scaffolding emitted for the
        // false branch, popping the then-body's value and replacing it
        // with `nil` — meaning `let x = if true { 42 }` produced `nil`
        // instead of `42`. The synthetic line 0 keeps the debugger
        // from reporting a phantom stop on the tail line of the
        // then-body when the VM jumps past the cleanup.
        let end_jump = self.chunk.emit_jump(Op::Jump, 0);
        self.chunk.patch_jump(else_jump);
        self.chunk.emit(Op::Pop, 0);
        if let Some(else_body) = else_body {
            self.compile_scoped_block(else_body)?;
        } else {
            self.chunk.emit(Op::Nil, 0);
        }
        self.chunk.patch_jump(end_jump);
        Ok(())
    }

    pub(super) fn compile_while_loop(
        &mut self,
        condition: &SNode,
        body: &[SNode],
    ) -> Result<(), CompileError> {
        let loop_start = self.chunk.current_offset();
        self.loop_stack.push(LoopContext {
            start_offset: loop_start,
            break_patches: Vec::new(),
            has_iterator: false,
            handler_depth: self.handler_depth,
            finally_depth: self.finally_bodies.len(),
            scope_depth: self.scope_depth,
        });
        self.compile_node(condition)?;
        let exit_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
        self.chunk.emit(Op::Pop, self.line);
        self.compile_scoped_statements(body)?;
        // Jump back to condition
        self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
        self.chunk.patch_jump(exit_jump);
        // Loop-exit cleanup is synthetic — line 0 keeps the debugger
        // from reporting a phantom stop on the tail body line when the
        // loop condition finally turns false.
        self.chunk.emit(Op::Pop, 0);
        let ctx = self.loop_stack.pop().unwrap();
        for patch_pos in ctx.break_patches {
            self.chunk.patch_jump(patch_pos);
        }
        self.chunk.emit(Op::Nil, 0);
        Ok(())
    }

    pub(super) fn compile_for_in(
        &mut self,
        pattern: &harn_parser::BindingPattern,
        iterable: &SNode,
        body: &[SNode],
        declaration: harn_lexer::Span,
    ) -> Result<(), CompileError> {
        let item_type = self.infer_for_item_type(iterable);
        self.compile_node(iterable)?;
        self.chunk.emit(Op::IterInit, self.line);
        let loop_start = self.chunk.current_offset();
        self.loop_stack.push(LoopContext {
            start_offset: loop_start,
            break_patches: Vec::new(),
            has_iterator: true,
            handler_depth: self.handler_depth,
            finally_depth: self.finally_bodies.len(),
            scope_depth: self.scope_depth,
        });
        // IterNext jumps to end if exhausted, else pushes the next item.
        let exit_jump_pos = self.chunk.emit_jump(Op::IterNext, self.line);
        self.begin_scope();
        let finally_floor = self.finally_bodies.len();
        self.compile_destructuring(pattern, true, declaration)?;
        // A `for`-item binding is reassignable per iteration, so — like a `let`
        // — its inferred primitive type may only feed typed-opcode
        // specialization when no reassignment in the loop body can change its
        // primitive kind. Otherwise drop the primitive fact and stay on the
        // generic adaptive path.
        let item_type = self.gate_for_item_type(pattern, item_type, body);
        self.record_binding_type(pattern, item_type);
        self.record_monomorphic_var_bindings(body);
        for sn in body {
            self.compile_discarded_stmt(sn)?;
        }
        self.drain_finallys_to_floor(finally_floor)?;
        self.end_scope();
        self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
        self.chunk.patch_jump(exit_jump_pos);
        let ctx = self.loop_stack.pop().unwrap();
        for patch_pos in ctx.break_patches {
            self.chunk.patch_jump(patch_pos);
        }
        // Synthetic Nil placeholder for the for-loop's expression value,
        // emitted after the iterator exit jump — tagged line 0 so the
        // debugger doesn't stop on it.
        self.chunk.emit(Op::Nil, 0);
        Ok(())
    }

    pub(super) fn compile_return_stmt(
        &mut self,
        value: &Option<Box<SNode>>,
    ) -> Result<(), CompileError> {
        if self.has_pending_finally() {
            // The operand must finish before a return begins. Protect its
            // evaluation so a dynamic throw still takes the throw-unwind path.
            if let Some(val) = value {
                self.compile_transfer_operand(val)?;
            } else {
                self.chunk.emit(Op::Nil, self.line);
            }
            self.temp_counter += 1;
            let temp_name = format!("__return_val_{}__", self.temp_counter);
            self.emit_define_binding(&temp_name, true);
            // Innermost finally first; skip catch barriers since
            // return transfers past local handlers. Each finally is masked
            // while it runs, so a `return` inside a finally doesn't re-run it.
            self.run_pending_finallys_for_transfer(0)?;
            self.emit_get_binding(&temp_name);
            self.chunk.emit(Op::Return, self.line);
        } else {
            // No pending finally — use tail-call optimization when possible.
            if let Some(val) = value {
                // Active handlers store catch offsets into this frame. Keep
                // the frame explicit until the return expression succeeds.
                let allow_tail_call = self.handler_depth == 0;
                if allow_tail_call {
                    if let Node::FunctionCall { name, args, .. } = &val.node {
                        if args.iter().any(|arg| matches!(&arg.node, Node::Spread(_))) {
                            self.compile_node(val)?;
                            self.chunk.emit(Op::Return, self.line);
                            return Ok(());
                        }
                        if self.has_local_binding(name) {
                            self.emit_get_binding(name);
                        } else {
                            let name_idx = self.string_constant(name);
                            self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                        }
                        for arg in args {
                            self.compile_node(arg)?;
                        }
                        self.chunk
                            .emit_u8(Op::TailCall, args.len() as u8, self.line);
                    } else if let Node::BinaryOp { op, left, right } = &val.node {
                        if op == "|>" && !contains_pipe_placeholder(right) {
                            self.compile_node(left)?;
                            self.compile_node(right)?;
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::TailCall, 1, self.line);
                        } else {
                            self.compile_node(val)?;
                        }
                    } else {
                        self.compile_node(val)?;
                    }
                } else {
                    self.compile_node(val)?;
                }
            } else {
                self.chunk.emit(Op::Nil, self.line);
            }
            self.chunk.emit(Op::Return, self.line);
        }
        Ok(())
    }

    pub(super) fn compile_cost_route(
        &mut self,
        options: &[(String, SNode)],
        body: &[SNode],
    ) -> Result<(), CompileError> {
        let route_idx = self.string_constant("__cost_route");
        self.chunk.emit_u16(Op::Constant, route_idx, self.line);

        for (key, value) in options {
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_node(value)?;
        }
        self.chunk
            .emit_u16(Op::BuildDict, options.len() as u16, self.line);

        self.compile_closure(&[], body)?;
        self.chunk.emit_u8(Op::Call, 2, self.line);
        Ok(())
    }

    pub(super) fn compile_break_stmt(&mut self) -> Result<(), CompileError> {
        if self.loop_stack.is_empty() {
            return Err(CompileError {
                message: "break outside of loop".to_string(),
                line: self.line,
            });
        }
        // Copy values out to avoid borrow conflict.
        let ctx = self.loop_stack.last().unwrap();
        let finally_depth = ctx.finally_depth;
        let handler_depth = ctx.handler_depth;
        let has_iterator = ctx.has_iterator;
        let scope_depth = ctx.scope_depth;
        for _ in handler_depth..self.handler_depth {
            self.chunk.emit(Op::PopHandler, self.line);
        }
        self.run_pending_finallys_for_transfer(finally_depth)?;
        self.emit_scope_unwind_to(scope_depth);
        if has_iterator {
            self.chunk.emit(Op::PopIterator, self.line);
        }
        let patch = self.chunk.emit_jump(Op::Jump, self.line);
        self.loop_stack
            .last_mut()
            .unwrap()
            .break_patches
            .push(patch);
        Ok(())
    }

    pub(super) fn compile_continue_stmt(&mut self) -> Result<(), CompileError> {
        if self.loop_stack.is_empty() {
            return Err(CompileError {
                message: "continue outside of loop".to_string(),
                line: self.line,
            });
        }
        let ctx = self.loop_stack.last().unwrap();
        let finally_depth = ctx.finally_depth;
        let handler_depth = ctx.handler_depth;
        let loop_start = ctx.start_offset;
        let scope_depth = ctx.scope_depth;
        for _ in handler_depth..self.handler_depth {
            self.chunk.emit(Op::PopHandler, self.line);
        }
        self.run_pending_finallys_for_transfer(finally_depth)?;
        self.emit_scope_unwind_to(scope_depth);
        self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
        Ok(())
    }

    pub(super) fn compile_guard_stmt(
        &mut self,
        condition: &SNode,
        else_body: &[SNode],
    ) -> Result<(), CompileError> {
        self.compile_node(condition)?;
        let skip_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
        self.chunk.emit(Op::Pop, self.line);
        self.compile_scoped_block(else_body)?;
        // Guard is a statement, not an expression: pop any trailing value.
        if !else_body.is_empty() && Self::produces_value(&else_body.last().unwrap().node) {
            self.chunk.emit(Op::Pop, self.line);
        }
        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
        self.chunk.patch_jump(skip_jump);
        self.chunk.emit(Op::Pop, self.line);
        self.chunk.patch_jump(end_jump);
        self.chunk.emit(Op::Nil, self.line);
        Ok(())
    }
}
