use harn_parser::SNode;

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_throw_stmt(&mut self, value: &SNode) -> Result<(), CompileError> {
        // Pending cleanups run from their own exception handlers, exactly as
        // they do for errors raised by callees and failing operations.
        self.compile_node(value)?;
        self.chunk.emit(Op::Throw, self.line);
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
        let empty_type = self.string_constant("");
        self.emit_type_name_extra(empty_type);

        self.compile_node(operand)?;

        self.handler_depth -= 1;
        self.chunk.emit(Op::PopHandler, self.line);
        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

        // Catch path: thrown value is on the stack. Rethrow it; pending
        // cleanups run from their own handlers.
        self.chunk.patch_jump(catch_jump);
        self.chunk.emit(Op::Throw, self.line);

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
                Some(name.as_str())
            } else {
                None
            }
        });

        let type_name_idx = if let Some(tn) = type_name {
            self.string_constant(tn)
        } else {
            self.string_constant("")
        };

        let has_catch = !catch_body.is_empty() || error_var.is_some();

        if let Some(finally_body) = finally_body {
            // The cleanup handler sits outside the catch handler, so it sees
            // errors from the body that the catch does not match, errors from
            // the catch body, and errors raised anywhere below this frame.
            let finally_floor = self.finally_bodies.len();
            self.push_cleanup(finally_body.clone());
            if has_catch {
                self.compile_try_catch(body, error_var, error_type, catch_body, &None)?;
            } else {
                self.compile_try_body(body)?;
            }
            self.drain_finallys_to_floor(finally_floor)?;
        } else {
            self.handler_depth += 1;
            let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
            self.emit_type_name_extra(type_name_idx);

            self.compile_try_body(body)?;

            self.handler_depth -= 1;
            self.chunk.emit(Op::PopHandler, self.line);
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
        let empty_type = self.string_constant("");
        self.emit_type_name_extra(empty_type);

        self.compile_try_body(body)?;

        self.handler_depth -= 1;
        self.chunk.emit(Op::PopHandler, self.line);

        // Wrap non-Result successes in Result.Ok while avoiding Ok(Ok(...))
        // when the try body already returns a Result.
        self.chunk.emit(Op::TryWrapOk, self.line);

        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

        // Error path: wrap in Result.Err.
        self.chunk.patch_jump(catch_jump);

        let err_idx = self.string_constant("Err");
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

        self.handler_depth += 1;
        let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
        // Empty type name → untyped catch.
        let empty_type = self.string_constant("");
        let hi = (empty_type >> 8) as u8;
        let lo = empty_type as u8;
        self.chunk.code.push(hi);
        self.chunk.code.push(lo);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);

        self.compile_block(body)?;

        self.handler_depth -= 1;
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

        // Body-success path lands here with the body's value on the stack —
        // that value IS the value of the `retry` expression (mirroring `if`,
        // `match` and `try`). The exhaustion path above always throws, so it
        // never falls through; a trailing `Op::Nil` here would only shadow the
        // body value with nil and leave it orphaned on the stack.
        self.chunk.patch_jump(end_jump);
        Ok(())
    }
}
