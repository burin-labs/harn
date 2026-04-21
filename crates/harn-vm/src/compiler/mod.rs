use harn_parser::{Node, SNode, TypeExpr};

mod closures;
mod concurrency;
mod decls;
mod error;
mod error_handling;
mod expressions;
mod patterns;
mod pipe;
mod state;
mod statements;
#[cfg(test)]
mod tests;
mod type_facts;
mod yield_scan;

pub use error::CompileError;

use crate::chunk::{Chunk, Constant, Op};

/// Look through an `AttributedDecl` wrapper to the inner declaration.
/// `compile_named` / `compile` use this so attributed declarations like
/// `@test pipeline foo(...)` are still discoverable by name.
fn peel_node(sn: &SNode) -> &Node {
    match &sn.node {
        Node::AttributedDecl { inner, .. } => &inner.node,
        other => other,
    }
}

/// Entry in the compiler's pending-finally stack. See the field-level doc on
/// `Compiler::finally_bodies` for the unwind semantics each variant encodes.
#[derive(Clone, Debug)]
enum FinallyEntry {
    Finally(Vec<SNode>),
    CatchBarrier,
}

/// Tracks loop context for break/continue compilation.
struct LoopContext {
    /// Offset of the loop start (for continue).
    start_offset: usize,
    /// Positions of break jumps that need patching to the loop end.
    break_patches: Vec<usize>,
    /// True if this is a for-in loop (has an iterator to clean up on break).
    has_iterator: bool,
    /// Number of exception handlers active at loop entry.
    handler_depth: usize,
    /// Number of pending finally bodies at loop entry.
    finally_depth: usize,
    /// Lexical scope depth at loop entry.
    scope_depth: usize,
}

#[derive(Clone, Copy, Debug)]
struct LocalBinding {
    slot: u16,
    mutable: bool,
}

/// Compiles an AST into bytecode.
pub struct Compiler {
    chunk: Chunk,
    line: u32,
    column: u32,
    /// Track enum type names so PropertyAccess on them can produce EnumVariant.
    enum_names: std::collections::HashSet<String>,
    /// Track struct type names to declared field order for indexed instances.
    struct_layouts: std::collections::HashMap<String, Vec<String>>,
    /// Track interface names → method names for runtime enforcement.
    interface_methods: std::collections::HashMap<String, Vec<String>>,
    /// Stack of active loop contexts for break/continue.
    loop_stack: Vec<LoopContext>,
    /// Current depth of exception handlers (for cleanup on break/continue).
    handler_depth: usize,
    /// Stack of pending finally bodies plus catch-handler barriers for
    /// unwind-aware lowering of `throw`, `return`, `break`, and `continue`.
    ///
    /// A `Finally` entry is a pending finally body that must execute when
    /// control exits its enclosing try block. A `CatchBarrier` marks the
    /// boundary of an active `try/catch` handler: throws emitted inside
    /// the try body are caught locally, so pre-running finallys *beyond*
    /// the barrier would wrongly fire side effects for outer blocks the
    /// throw never actually escapes. Throw lowering stops at the innermost
    /// barrier; `return`/`break`/`continue`, which do transfer past local
    /// handlers, still run every pending `Finally` up to their target.
    finally_bodies: Vec<FinallyEntry>,
    /// Counter for unique temp variable names.
    temp_counter: usize,
    /// Number of lexical block scopes currently active in this compiled frame.
    scope_depth: usize,
    /// Top-level `type` aliases, used to lower `schema_of(T)` and
    /// `output_schema: T` into constant JSON-Schema dicts at compile time.
    type_aliases: std::collections::HashMap<String, TypeExpr>,
    /// Lightweight compiler-side type facts used only for conservative
    /// bytecode specialization. This mirrors lexical scopes and is separate
    /// from the parser's diagnostic type checker so compile-only callers keep
    /// working without a required type-check pass.
    type_scopes: Vec<std::collections::HashMap<String, TypeExpr>>,
    /// Lexical variable slots for the current compiled frame. The compiler
    /// only consults this for names declared inside the current function-like
    /// body; all unresolved names stay on the existing dynamic/name path.
    local_scopes: Vec<std::collections::HashMap<String, LocalBinding>>,
    /// True when this compiler is emitting code outside any function-like
    /// scope (module top-level statements). `try*` is rejected here
    /// because the rethrow has no enclosing function to live in.
    /// Pipeline bodies and nested `Compiler::new()` instances (fn,
    /// closure, tool, etc.) flip this to false before compiling.
    module_level: bool,
}

impl Compiler {
    /// Compile a single AST node. Most arm bodies live in per-category
    /// submodules (expressions, statements, closures, decls, patterns,
    /// error_handling, concurrency); this function is a thin dispatcher.
    fn compile_node(&mut self, snode: &SNode) -> Result<(), CompileError> {
        self.line = snode.span.line as u32;
        self.column = snode.span.column as u32;
        self.chunk.set_column(self.column);
        match &snode.node {
            Node::IntLiteral(n) => {
                let idx = self.chunk.add_constant(Constant::Int(*n));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::FloatLiteral(n) => {
                let idx = self.chunk.add_constant(Constant::Float(*n));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::StringLiteral(s) | Node::RawStringLiteral(s) => {
                let idx = self.chunk.add_constant(Constant::String(s.clone()));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::BoolLiteral(true) => self.chunk.emit(Op::True, self.line),
            Node::BoolLiteral(false) => self.chunk.emit(Op::False, self.line),
            Node::NilLiteral => self.chunk.emit(Op::Nil, self.line),
            Node::DurationLiteral(ms) => {
                let idx = self.chunk.add_constant(Constant::Duration(*ms));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::Identifier(name) => {
                self.emit_get_binding(name);
            }
            Node::LetBinding { pattern, value, .. } => {
                let binding_type = match &snode.node {
                    Node::LetBinding {
                        type_ann: Some(type_ann),
                        ..
                    } => Some(type_ann.clone()),
                    _ => self.infer_expr_type(value),
                };
                self.compile_node(value)?;
                self.compile_destructuring(pattern, false)?;
                self.record_binding_type(pattern, binding_type);
            }
            Node::VarBinding { pattern, value, .. } => {
                let binding_type = match &snode.node {
                    Node::VarBinding {
                        type_ann: Some(type_ann),
                        ..
                    } => Some(type_ann.clone()),
                    _ => self.infer_expr_type(value),
                };
                self.compile_node(value)?;
                self.compile_destructuring(pattern, true)?;
                self.record_binding_type(pattern, binding_type);
            }
            Node::Assignment {
                target, value, op, ..
            } => {
                self.compile_assignment(target, value, op)?;
            }
            Node::BinaryOp { op, left, right } => {
                self.compile_binary_op(op, left, right)?;
            }
            Node::UnaryOp { op, operand } => {
                self.compile_node(operand)?;
                match op.as_str() {
                    "-" => self.chunk.emit(Op::Negate, self.line),
                    "!" => self.chunk.emit(Op::Not, self.line),
                    _ => {}
                }
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.compile_node(condition)?;
                let else_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_node(true_expr)?;
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(else_jump);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_node(false_expr)?;
                self.chunk.patch_jump(end_jump);
            }
            Node::FunctionCall { name, args } => {
                self.compile_function_call(name, args)?;
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                self.compile_method_call(object, method, args)?;
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                self.compile_node(object)?;
                for arg in args {
                    self.compile_node(arg)?;
                }
                let name_idx = self.chunk.add_constant(Constant::String(method.clone()));
                self.chunk
                    .emit_method_call_opt(name_idx, args.len() as u8, self.line);
            }
            Node::PropertyAccess { object, property } => {
                self.compile_property_access(object, property)?;
            }
            Node::OptionalPropertyAccess { object, property } => {
                self.compile_node(object)?;
                let idx = self.chunk.add_constant(Constant::String(property.clone()));
                self.chunk.emit_u16(Op::GetPropertyOpt, idx, self.line);
            }
            Node::SubscriptAccess { object, index } => {
                self.compile_node(object)?;
                self.compile_node(index)?;
                self.chunk.emit(Op::Subscript, self.line);
            }
            Node::SliceAccess { object, start, end } => {
                self.compile_node(object)?;
                if let Some(s) = start {
                    self.compile_node(s)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                if let Some(e) = end {
                    self.compile_node(e)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                self.chunk.emit(Op::Slice, self.line);
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.compile_if_else(condition, then_body, else_body)?;
            }
            Node::WhileLoop { condition, body } => {
                self.compile_while_loop(condition, body)?;
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.compile_for_in(pattern, iterable, body)?;
            }
            Node::ReturnStmt { value } => {
                self.compile_return_stmt(value)?;
            }
            Node::BreakStmt => {
                self.compile_break_stmt()?;
            }
            Node::ContinueStmt => {
                self.compile_continue_stmt()?;
            }
            Node::ListLiteral(elements) => {
                self.compile_list_literal(elements)?;
            }
            Node::DictLiteral(entries) => {
                self.compile_dict_literal(entries)?;
            }
            Node::InterpolatedString(segments) => {
                self.compile_interpolated_string(segments)?;
            }
            Node::FnDecl {
                name, params, body, ..
            } => {
                self.compile_fn_decl(name, params, body)?;
            }
            Node::ToolDecl {
                name,
                description,
                params,
                return_type,
                body,
                ..
            } => {
                self.compile_tool_decl(name, description, params, return_type, body)?;
            }
            Node::SkillDecl { name, fields, .. } => {
                self.compile_skill_decl(name, fields)?;
            }
            Node::Closure { params, body, .. } => {
                self.compile_closure(params, body)?;
            }
            Node::ThrowStmt { value } => {
                self.compile_throw_stmt(value)?;
            }
            Node::MatchExpr { value, arms } => {
                self.compile_match_expr(value, arms)?;
            }
            Node::RangeExpr {
                start,
                end,
                inclusive,
            } => {
                let name_idx = self
                    .chunk
                    .add_constant(Constant::String("__range__".to_string()));
                self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                self.compile_node(start)?;
                self.compile_node(end)?;
                if *inclusive {
                    self.chunk.emit(Op::True, self.line);
                } else {
                    self.chunk.emit(Op::False, self.line);
                }
                self.chunk.emit_u8(Op::Call, 3, self.line);
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.compile_guard_stmt(condition, else_body)?;
            }
            Node::RequireStmt { condition, message } => {
                self.compile_node(condition)?;
                let ok_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                self.chunk.emit(Op::Pop, self.line);
                if let Some(message) = message {
                    self.compile_node(message)?;
                } else {
                    let idx = self
                        .chunk
                        .add_constant(Constant::String("require condition failed".to_string()));
                    self.chunk.emit_u16(Op::Constant, idx, self.line);
                }
                self.chunk.emit(Op::Throw, self.line);
                self.chunk.patch_jump(ok_jump);
                self.chunk.emit(Op::Pop, self.line);
            }
            Node::Block(stmts) => {
                self.compile_scoped_block(stmts)?;
            }
            Node::DeadlineBlock { duration, body } => {
                self.compile_node(duration)?;
                self.chunk.emit(Op::DeadlineSetup, self.line);
                self.compile_scoped_block(body)?;
                self.chunk.emit(Op::DeadlineEnd, self.line);
            }
            Node::MutexBlock { body } => {
                // v1: single-threaded, but still uses a lexical block scope.
                self.begin_scope();
                for sn in body {
                    self.compile_node(sn)?;
                    if Self::produces_value(&sn.node) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
                self.chunk.emit(Op::Nil, self.line);
                self.end_scope();
            }
            Node::DeferStmt { body } => {
                // Push onto the finally stack so it runs on return/throw/scope-exit.
                self.finally_bodies
                    .push(FinallyEntry::Finally(body.clone()));
                self.chunk.emit(Op::Nil, self.line);
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    self.compile_node(val)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                self.chunk.emit(Op::Yield, self.line);
            }
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                self.compile_enum_construct(enum_name, variant, args)?;
            }
            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                self.compile_struct_construct(struct_name, fields)?;
            }
            Node::ImportDecl { path } => {
                let idx = self.chunk.add_constant(Constant::String(path.clone()));
                self.chunk.emit_u16(Op::Import, idx, self.line);
            }
            Node::SelectiveImport { names, path } => {
                let path_idx = self.chunk.add_constant(Constant::String(path.clone()));
                let names_str = names.join(",");
                let names_idx = self.chunk.add_constant(Constant::String(names_str));
                self.chunk
                    .emit_u16(Op::SelectiveImport, path_idx, self.line);
                let hi = (names_idx >> 8) as u8;
                let lo = names_idx as u8;
                self.chunk.code.push(hi);
                self.chunk.code.push(lo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
            }
            Node::TryOperator { operand } => {
                self.compile_node(operand)?;
                self.chunk.emit(Op::TryUnwrap, self.line);
            }
            // `try* EXPR`: evaluate EXPR; on throw, run pending finally
            // blocks up to the innermost catch barrier and rethrow the
            // original value. On success, leave EXPR's value on the stack.
            //
            // Per the issue-#26 desugaring:
            //   { let _r = try { EXPR }
            //     guard is_ok(_r) else { throw unwrap_err(_r) }
            //     unwrap(_r) }
            //
            // The bytecode realizes this directly: install a try handler
            // around EXPR so a throw lands in our catch path, where we
            // pre-run pending finallys and re-emit `Throw`. Skipping the
            // intermediate Result.Ok/Err wrapping that `TryExpr` does
            // keeps the success path a no-op (operand value passes through
            // as-is).
            Node::TryStar { operand } => {
                self.compile_try_star(operand)?;
            }
            Node::ImplBlock { type_name, methods } => {
                self.compile_impl_block(type_name, methods)?;
            }
            Node::StructDecl { name, fields, .. } => {
                self.compile_struct_decl(name, fields)?;
            }
            // Metadata-only declarations (no runtime effect).
            Node::Pipeline { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. } => {
                self.chunk.emit(Op::Nil, self.line);
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                self.compile_try_catch(body, error_var, error_type, catch_body, finally_body)?;
            }
            Node::TryExpr { body } => {
                self.compile_try_expr(body)?;
            }
            Node::Retry { count, body } => {
                self.compile_retry(count, body)?;
            }
            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                self.compile_parallel(mode, expr, variable, body, options)?;
            }
            Node::SpawnExpr { body } => {
                self.compile_spawn_expr(body)?;
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                self.compile_select_expr(cases, timeout, default_body)?;
            }
            Node::Spread(_) => {
                return Err(CompileError {
                    message: "spread (...) can only be used inside list literals, dict literals, or function call arguments".into(),
                    line: self.line,
                });
            }
            Node::AttributedDecl { attributes, inner } => {
                self.compile_attributed_decl(attributes, inner)?;
            }
            Node::OrPattern(_) => {
                return Err(CompileError {
                    message: "or-pattern (|) can only appear as a match arm pattern".into(),
                    line: self.line,
                });
            }
        }
        Ok(())
    }
}
