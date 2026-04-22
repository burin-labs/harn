use harn_parser::SNode;

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::{Compiler, FinallyEntry};

impl Compiler {
    pub(super) fn compile_throw_stmt(&mut self, value: &SNode) -> Result<(), CompileError> {
        // Only run finallys the unwind will actually cross — i.e.,
        // those between this throw and the innermost `CatchBarrier`.
        // Finallys beyond the nearest local `catch` aren't on the
        // throw's escape path (the catch halts unwinding there), so
        // pre-running them wrongly fires outer side effects.
        let pending = self.pending_finallys_until_barrier();
        if !pending.is_empty() {
            self.compile_node(value)?;
            self.temp_counter += 1;
            let temp_name = format!("__throw_val_{}__", self.temp_counter);
            self.emit_define_binding(&temp_name, true);
            for fb in &pending {
                self.compile_finally_inline(fb)?;
            }
            self.emit_get_binding(&temp_name);
            self.chunk.emit(Op::Throw, self.line);
        } else {
            self.compile_node(value)?;
            self.chunk.emit(Op::Throw, self.line);
        }
        Ok(())
    }

    pub(super) fn compile_try_star(&mut self, operand: &SNode) -> Result<(), CompileError> {
        if self.module_level {
            return Err(CompileError {
                message: "try* requires an enclosing function (fn, tool, or pipeline) so the rethrow has a target".into(),
                line: self.line,
            });
        }
        self.handler_depth += 1;
        let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
        let empty_type = self.chunk.add_constant(Constant::String(String::new()));
        self.emit_type_name_extra(empty_type);

        self.compile_node(operand)?;

        self.handler_depth -= 1;
        self.chunk.emit(Op::PopHandler, self.line);
        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

        // Catch path: thrown value is on the stack. Pre-run any
        // finallys between us and the innermost catch barrier
        // (mirrors `Node::ThrowStmt` lowering), then rethrow.
        self.chunk.patch_jump(catch_jump);
        let pending = self.pending_finallys_until_barrier();
        if pending.is_empty() {
            self.chunk.emit(Op::Throw, self.line);
        } else {
            self.temp_counter += 1;
            let temp_name = format!("__try_star_err_{}__", self.temp_counter);
            self.emit_define_binding(&temp_name, true);
            for fb in &pending {
                self.compile_finally_inline(fb)?;
            }
            self.emit_get_binding(&temp_name);
            self.chunk.emit(Op::Throw, self.line);
        }

        self.chunk.patch_jump(end_jump);
        Ok(())
    }

    pub(super) fn compile_try_catch(
        &mut self,
        body: &[SNode],
        error_var: &Option<String>,
        error_type: &Option<harn_parser::TypeExpr>,
        catch_body: &[SNode],
        finally_body: &Option<Vec<SNode>>,
    ) -> Result<(), CompileError> {
        // Extract the type name for typed catch (e.g., "AppError")
        let type_name = error_type.as_ref().and_then(|te| {
            if let harn_parser::TypeExpr::Named(name) = te {
                Some(name.clone())
            } else {
                None
            }
        });

        let type_name_idx = if let Some(ref tn) = type_name {
            self.chunk.add_constant(Constant::String(tn.clone()))
        } else {
            self.chunk.add_constant(Constant::String(String::new()))
        };

        let has_catch = !catch_body.is_empty() || error_var.is_some();
        let has_finally = finally_body.is_some();

        if has_catch && has_finally {
            let finally_body = finally_body.as_ref().unwrap();
            // During the try body: install both the catch barrier
            // (so throws don't pre-run finallys beyond our catch)
            // and our finally (so return/break/continue in the
            // body still run it). Order matters — barrier is below
            // our finally so pre-running stops *at* the barrier.
            self.finally_bodies.push(FinallyEntry::CatchBarrier);
            self.finally_bodies
                .push(FinallyEntry::Finally(finally_body.clone()));

            self.handler_depth += 1;
            let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
            self.emit_type_name_extra(type_name_idx);

            self.compile_try_body(body)?;

            self.handler_depth -= 1;
            self.chunk.emit(Op::PopHandler, self.line);
            // Body-success path: throw never fired, so pre-run did
            // not happen. Run finally now.
            self.compile_finally_inline(finally_body)?;
            // Drop both finally and barrier — we're leaving the
            // try body; the catch handler compiles without them.
            self.finally_bodies.pop(); // Finally
            self.finally_bodies.pop(); // CatchBarrier
            let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

            self.chunk.patch_jump(catch_jump);
            self.begin_scope();
            self.compile_catch_binding(error_var)?;

            // Inner try around catch body so a catch-body throw
            // lands in our `rethrow_jump` and we emit a plain
            // rethrow (finally already fired via the body's throw).
            self.handler_depth += 1;
            let rethrow_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
            let empty_type = self.chunk.add_constant(Constant::String(String::new()));
            self.emit_type_name_extra(empty_type);

            self.compile_try_body(catch_body)?;

            self.handler_depth -= 1;
            self.chunk.emit(Op::PopHandler, self.line);
            self.end_scope();
            let end_jump2 = self.chunk.emit_jump(Op::Jump, self.line);

            // Rethrow handler: plain rethrow; finally already pre-ran
            // via the body's Throw lowering before the outer handler
            // delivered control into catch.
            self.chunk.patch_jump(rethrow_jump);
            self.compile_plain_rethrow()?;
            self.end_scope();

            self.chunk.patch_jump(end_jump);
            self.chunk.patch_jump(end_jump2);
        } else if has_finally {
            let finally_body = finally_body.as_ref().unwrap();
            // No catch: throws in the body unwind through us, so
            // we don't install a barrier — our finally and any
            // outer finallys are on the throw's escape path.
            self.finally_bodies
                .push(FinallyEntry::Finally(finally_body.clone()));

            self.handler_depth += 1;
            let error_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
            let empty_type = self.chunk.add_constant(Constant::String(String::new()));
            self.emit_type_name_extra(empty_type);

            self.compile_try_body(body)?;

            self.handler_depth -= 1;
            self.chunk.emit(Op::PopHandler, self.line);
            self.compile_finally_inline(finally_body)?;
            let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

            // Error path: save error, re-throw. Finally already
            // pre-ran via the body's Throw lowering.
            self.chunk.patch_jump(error_jump);
            self.compile_plain_rethrow()?;

            self.chunk.patch_jump(end_jump);

            self.finally_bodies.pop(); // Finally
        } else {
            // try-catch without finally: install a barrier so
            // throws in the body don't pre-run outer finallys
            // (the throw is caught here and won't unwind past).
            self.finally_bodies.push(FinallyEntry::CatchBarrier);

            self.handler_depth += 1;
            let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
            self.emit_type_name_extra(type_name_idx);

            self.compile_try_body(body)?;

            self.handler_depth -= 1;
            self.chunk.emit(Op::PopHandler, self.line);
            self.finally_bodies.pop(); // CatchBarrier
            let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

            self.chunk.patch_jump(catch_jump);
            self.begin_scope();
            self.compile_catch_binding(error_var)?;

            self.compile_try_body(catch_body)?;
            self.end_scope();

            self.chunk.patch_jump(end_jump);
        }
        Ok(())
    }

    pub(super) fn compile_try_expr(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        // `try { body }` returns Result.Ok(value) or Result.Err(error).
        self.handler_depth += 1;
        let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
        let empty_type = self.chunk.add_constant(Constant::String(String::new()));
        self.emit_type_name_extra(empty_type);

        self.compile_try_body(body)?;

        self.handler_depth -= 1;
        self.chunk.emit(Op::PopHandler, self.line);

        // Wrap success in Result.Ok.
        let ok_idx = self.chunk.add_constant(Constant::String("Ok".to_string()));
        self.chunk.emit_u16(Op::Constant, ok_idx, self.line);
        self.chunk.emit(Op::Swap, self.line);
        self.chunk.emit_u8(Op::Call, 1, self.line);

        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

        // Error path: wrap in Result.Err.
        self.chunk.patch_jump(catch_jump);

        let err_idx = self.chunk.add_constant(Constant::String("Err".to_string()));
        self.chunk.emit_u16(Op::Constant, err_idx, self.line);
        self.chunk.emit(Op::Swap, self.line);
        self.chunk.emit_u8(Op::Call, 1, self.line);

        self.chunk.patch_jump(end_jump);
        Ok(())
    }

    pub(super) fn compile_retry(
        &mut self,
        count: &SNode,
        body: &[SNode],
    ) -> Result<(), CompileError> {
        self.compile_node(count)?;
        let counter_name = "__retry_counter__";
        self.emit_define_binding(counter_name, true);

        // Store last error for re-throwing after retries are exhausted.
        self.chunk.emit(Op::Nil, self.line);
        let err_name = "__retry_last_error__";
        self.emit_define_binding(err_name, true);

        let loop_start = self.chunk.current_offset();

        let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
        // Empty type name → untyped catch.
        let empty_type = self.chunk.add_constant(Constant::String(String::new()));
        let hi = (empty_type >> 8) as u8;
        let lo = empty_type as u8;
        self.chunk.code.push(hi);
        self.chunk.code.push(lo);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);

        self.compile_block(body)?;

        self.chunk.emit(Op::PopHandler, self.line);
        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

        self.chunk.patch_jump(catch_jump);
        self.chunk.emit(Op::Dup, self.line);
        self.emit_set_binding(err_name);
        self.chunk.emit(Op::Pop, self.line);

        self.emit_get_binding(counter_name);
        let one_idx = self.chunk.add_constant(Constant::Int(1));
        self.chunk.emit_u16(Op::Constant, one_idx, self.line);
        self.chunk.emit(Op::Sub, self.line);
        self.chunk.emit(Op::Dup, self.line);
        self.emit_set_binding(counter_name);

        let zero_idx = self.chunk.add_constant(Constant::Int(0));
        self.chunk.emit_u16(Op::Constant, zero_idx, self.line);
        self.chunk.emit(Op::Greater, self.line);
        let retry_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
        self.chunk.emit(Op::Pop, self.line);
        self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);

        // Retries exhausted — re-throw the last error.
        self.chunk.patch_jump(retry_jump);
        self.chunk.emit(Op::Pop, self.line);
        self.emit_get_binding(err_name);
        self.chunk.emit(Op::Throw, self.line);

        self.chunk.patch_jump(end_jump);
        self.chunk.emit(Op::Nil, self.line);
        Ok(())
    }
}
