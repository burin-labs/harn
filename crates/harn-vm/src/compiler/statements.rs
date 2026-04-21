use harn_parser::{Node, SNode};

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::{Compiler, LoopContext};

impl Compiler {
    pub(super) fn compile_assignment(
        &mut self,
        target: &SNode,
        value: &SNode,
        op: &Option<String>,
    ) -> Result<(), CompileError> {
        if let Node::Identifier(name) = &target.node {
            let idx = self.chunk.add_constant(Constant::String(name.clone()));
            if let Some(op) = op {
                let left_type = self.infer_expr_type(target);
                let right_type = self.infer_expr_type(value);
                let result_type =
                    self.infer_binary_result_type(op, left_type.as_ref(), right_type.as_ref());
                self.chunk.emit_u16(Op::GetVar, idx, self.line);
                self.compile_node(value)?;
                if let Some(typed_op) =
                    self.specialized_binary_op(op, left_type.as_ref(), right_type.as_ref())
                {
                    self.chunk.emit(typed_op, self.line);
                } else {
                    self.emit_compound_op(op)?;
                }
                self.chunk.emit_u16(Op::SetVar, idx, self.line);
                self.assign_type_fact(name, result_type);
            } else {
                let value_type = self.infer_expr_type(value);
                self.compile_node(value)?;
                self.chunk.emit_u16(Op::SetVar, idx, self.line);
                self.assign_type_fact(name, value_type);
            }
        } else if let Node::PropertyAccess { object, property } = &target.node {
            if let Some(var_name) = self.root_var_name(object) {
                let var_idx = self.chunk.add_constant(Constant::String(var_name.clone()));
                let prop_idx = self.chunk.add_constant(Constant::String(property.clone()));
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
                let var_idx = self.chunk.add_constant(Constant::String(var_name.clone()));
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
        if let Some(else_body) = else_body {
            // Cleanup jump + else-branch Pop share the synthetic line 0
            // so the debugger doesn't report a phantom stop on the tail
            // line of the then-body when the VM jumps past it.
            let end_jump = self.chunk.emit_jump(Op::Jump, 0);
            self.chunk.patch_jump(else_jump);
            self.chunk.emit(Op::Pop, 0);
            self.compile_scoped_block(else_body)?;
            self.chunk.patch_jump(end_jump);
        } else {
            self.chunk.patch_jump(else_jump);
            // Same rationale: the Pop/Nil cleanup emitted after the
            // JumpIfFalse target is part of the compiler's expression
            // scaffolding, not source code. Tagging these with line 0
            // keeps step-over from stopping on a line that wasn't
            // actually executed (see step_execute's upcoming_line()).
            self.chunk.emit(Op::Pop, 0);
            self.chunk.emit(Op::Nil, 0);
        }
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
        self.compile_destructuring(pattern, true)?;
        self.record_binding_type(pattern, item_type);
        for sn in body {
            self.compile_node(sn)?;
            if Self::produces_value(&sn.node) {
                self.chunk.emit(Op::Pop, self.line);
            }
        }
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
            let save_idx = self.chunk.add_constant(Constant::String(temp_name.clone()));
            self.chunk.emit_u16(Op::DefVar, save_idx, self.line);
            // Innermost finally first; skip catch barriers since
            // return transfers past local handlers.
            for fb in self.all_pending_finallys() {
                self.compile_finally_inline(&fb)?;
            }
            let restore_idx = self.chunk.add_constant(Constant::String(temp_name));
            self.chunk.emit_u16(Op::GetVar, restore_idx, self.line);
            self.chunk.emit(Op::Return, self.line);
        } else {
            // No pending finally — use tail-call optimization when possible.
            if let Some(val) = value {
                if let Node::FunctionCall { name, args } = &val.node {
                    let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                    self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                    for arg in args {
                        self.compile_node(arg)?;
                    }
                    self.chunk
                        .emit_u8(Op::TailCall, args.len() as u8, self.line);
                } else if let Node::BinaryOp { op, left, right } = &val.node {
                    if op == "|>" {
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
                self.chunk.emit(Op::Nil, self.line);
            }
            self.chunk.emit(Op::Return, self.line);
        }
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
        for fb in self.pending_finallys_down_to(finally_depth) {
            self.compile_finally_inline(&fb)?;
        }
        self.unwind_scopes_to(scope_depth);
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
        for fb in self.pending_finallys_down_to(finally_depth) {
            self.compile_finally_inline(&fb)?;
        }
        self.unwind_scopes_to(scope_depth);
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
