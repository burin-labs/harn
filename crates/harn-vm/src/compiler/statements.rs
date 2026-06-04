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
                let value_type = self.infer_expr_type(value);
                self.compile_node(value)?;
                self.emit_set_binding(name);
                self.assign_type_fact(name, value_type);
            }
        } else if let Node::PropertyAccess { object, property } = &target.node {
            if let Some(var_name) = self.root_var_name(object) {
                let var_idx = self.string_constant(&var_name);
                let prop_idx = self.string_constant(property);
                if let Some(op) = op {
                    self.compile_node(target)?;
                    self.compile_node(value)?;
                    self.emit_compound_op(op)?;
                } else {
                    self.compile_node(value)?;
                }
                // SetProperty reads var_idx from env, sets prop, writes back.
                // The variable name index is encoded as a second u16.
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
        } else if let Node::SubscriptAccess { object, index } = &target.node {
            if let Some(var_name) = self.root_var_name(object) {
                let var_idx = self.string_constant(&var_name);
                if let Some(op) = op {
                    self.compile_node(target)?;
                    self.compile_node(value)?;
                    self.emit_compound_op(op)?;
                } else {
                    self.compile_node(value)?;
                }
                self.compile_node(index)?;
                self.chunk.emit_u16(Op::SetSubscript, var_idx, self.line);
            }
        }
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
        }
        if !self.options.optimizations_enabled() {
            return Ok(false);
        }
        let rhs_type = self.infer_expr_type(rhs);
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
        self.compile_destructuring(pattern, true)?;
        self.record_binding_type(pattern, item_type);
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
            // Inside try-finally: save value to a temp, run pending
            // finallys, then restore and return.
            if let Some(val) = value {
                self.compile_node(val)?;
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
                        let name_idx = self.string_constant(name);
                        self.chunk.emit_u16(Op::Constant, name_idx, self.line);
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
            if matches!(
                key.as_str(),
                "fallback_strategy" | "strategy" | "quality" | "min_quality"
            ) {
                if let Node::Identifier(identifier) = &value.node {
                    let value_idx = self.string_constant(identifier);
                    self.chunk.emit_u16(Op::Constant, value_idx, self.line);
                    continue;
                }
            }
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
