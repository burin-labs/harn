use harn_parser::{Node, SNode, TypeExpr};

mod bindings;
mod catalogs;
mod closures;
mod concurrency;
mod decls;
mod error;
mod error_handling;
mod expressions;
mod hitl;
mod optimizer;
mod patterns;
mod pipe;
mod pipelines;
mod state;
mod statements;
#[cfg(test)]
mod tests;
mod type_facts;
mod yield_scan;

pub use error::CompileError;

use crate::chunk::{Chunk, Constant, Op};

/// Jump operands are 16-bit chunk offsets (`emit_jump`, `patch_jump`,
/// backward loop jumps), so a chunk whose code grows past `u16::MAX`
/// bytes would silently truncate jump targets and land somewhere wild at
/// runtime. Every finalized chunk (the program chunk and each compiled
/// function's chunk) must pass through this guard so oversized bodies
/// fail compilation instead of miscompiling.
pub(crate) fn ensure_chunk_addressable(
    chunk: &Chunk,
    what: &str,
    line: u32,
) -> Result<(), CompileError> {
    if chunk.code.len() > u16::MAX as usize {
        return Err(CompileError {
            message: format!(
                "{what} compiled to {} bytes of bytecode, more than the 64 KiB a jump \
                 operand can address; split it into smaller functions",
                chunk.code.len()
            ),
            line,
        });
    }
    Ok(())
}

/// Environment variable that disables optional compiler optimizations.
///
/// The VM still emits structurally required bytecode, such as parameter
/// slots, but skips semantic-preserving optimizer passes. This gives tests
/// and benchmarks a stable optimized-vs-unoptimized comparison switch.
pub const HARN_DISABLE_OPTIMIZATIONS_ENV: &str = "HARN_DISABLE_OPTIMIZATIONS";

/// Controls semantic-preserving compiler optimizations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompilerOptions {
    optimize: bool,
}

impl CompilerOptions {
    pub fn optimized() -> Self {
        Self { optimize: true }
    }

    pub fn without_optimizations() -> Self {
        Self { optimize: false }
    }

    pub fn from_env() -> Self {
        if std::env::var_os(HARN_DISABLE_OPTIMIZATIONS_ENV).is_some() {
            Self::without_optimizations()
        } else {
            Self::optimized()
        }
    }

    pub fn optimizations_enabled(self) -> bool {
        self.optimize
    }
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self::optimized()
    }
}

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
enum LocalStorage {
    Slot(u16),
    /// An environment-backed cell that still participates in lexical
    /// shadowing. Captured mutable bindings use cells so closures see later
    /// writes, but a later same-named declaration must not retroactively
    /// redirect earlier references into a new local slot.
    Environment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalBindingKind {
    Value,
    Callable,
}

#[derive(Clone, Copy, Debug)]
struct LocalBinding {
    storage: LocalStorage,
    kind: LocalBindingKind,
    mutable: bool,
}

struct EnumCatalogSnapshot {
    names: std::collections::HashSet<String>,
    variant_owners: std::collections::HashMap<String, Vec<String>>,
}

/// Compiles an AST into bytecode.
pub struct Compiler {
    options: CompilerOptions,
    chunk: Chunk,
    line: u32,
    column: u32,
    /// Track enum type names so PropertyAccess on them can produce EnumVariant.
    enum_names: std::collections::HashSet<String>,
    /// Variant name → owning enum names. Lets a bare call-shaped match
    /// pattern (`Ok(v)`, `Some(x)`) resolve to its enum without
    /// qualification when the variant name is unambiguous.
    enum_variant_owners: std::collections::HashMap<String, Vec<String>>,
    /// Source spans of enums predeclared into the module catalog. Re-visiting
    /// those AST nodes during bytecode emission must not replace the final
    /// prepass view with an earlier duplicate declaration.
    predeclared_enum_declarations: std::collections::HashSet<(usize, usize)>,
    /// Catalog snapshots paired with lexical bytecode scopes. Enum
    /// declarations update the active catalog in source order; restoring the
    /// snapshot on scope exit prevents a block-local enum from leaking into
    /// later outer match patterns.
    enum_catalog_scopes: Vec<EnumCatalogSnapshot>,
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
    /// `(span.start, span.end)` of every mutable binding (`let` / `for`-item)
    /// proven *monomorphic*: its value keeps a single primitive type across its
    /// initializer and every reassignment in scope. Only these bindings may
    /// carry an initializer-inferred primitive type fact into typed-opcode
    /// specialization (`AddInt`, `LessInt`, …), which hard-errors on a runtime
    /// operand-type mismatch. A mutable binding that is reassigned through an
    /// `any`-typed (or otherwise non-matching) value is *not* recorded here, so
    /// the compiler keeps it on the generic adaptive path that re-checks operand
    /// shapes at runtime — see [`Compiler::record_monomorphic_var_bindings`].
    /// Populated per lexical scope before that scope's statements are compiled;
    /// keyed by byte span because `Span` is not `Hash`.
    monomorphic_bindings: std::collections::HashSet<(usize, usize)>,
    /// Current-chunk string constant index. This avoids repeatedly scanning the
    /// constant pool while compiling name-heavy scripts.
    string_constants: std::collections::HashMap<String, u16>,
    /// Lexical bindings for the current compiled frame. Ordinary locals use
    /// indexed slots; mutable values captured by nested callables retain an
    /// environment-backed marker so lexical shadowing and dynamic cell access
    /// agree on the same declaration.
    local_scopes: Vec<std::collections::HashMap<String, LocalBinding>>,
    /// True when this compiler is emitting code outside any function-like
    /// scope (module top-level statements). `try*` is rejected here
    /// because the rethrow has no enclosing function to live in.
    /// Pipeline bodies and nested `Compiler::new()` instances (fn,
    /// closure, tool, etc.) flip this to false before compiling.
    module_level: bool,
    /// Source bindings captured by a nested callable in the body this compiler
    /// emits. Identity includes the declaration span, so a shadowing parameter
    /// or block-local never boxes an unrelated same-named `let`.
    captured_bindings: std::collections::HashSet<harn_parser::lexical::BindingId>,
}

impl Compiler {
    /// Compile a single AST node. Most arm bodies live in per-category
    /// submodules (expressions, statements, closures, decls, patterns,
    /// error_handling, concurrency); this function is a thin dispatcher.
    fn compile_node(&mut self, snode: &SNode) -> Result<(), CompileError> {
        self.line = snode.span.line as u32;
        self.column = snode.span.column as u32;
        self.chunk.set_column(self.column);
        if self.options.optimizations_enabled() {
            if let Some(folded) = optimizer::fold_constant_expr(snode) {
                if folded.node != snode.node {
                    return self.compile_node(&folded);
                }
            }
        }
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
                let idx = self.string_constant(s);
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::BoolLiteral(true) => self.chunk.emit(Op::True, self.line),
            Node::BoolLiteral(false) => self.chunk.emit(Op::False, self.line),
            Node::NilLiteral => self.chunk.emit(Op::Nil, self.line),
            Node::DurationLiteral(ms) => {
                let ms = i64::try_from(*ms).map_err(|_| CompileError {
                    message: "duration literal is too large".to_string(),
                    line: self.line,
                })?;
                let idx = self.chunk.add_constant(Constant::Duration(ms));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::Identifier(name) => {
                if let Some(schema) = self.schema_value_for_alias(name) {
                    self.emit_vm_value_literal(&schema);
                    return Ok(());
                }
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
                self.compile_destructuring(pattern, true, snode.span)?;
                // A `let` is reassignable, so its initializer-inferred primitive
                // type is only safe for typed-opcode specialization when the
                // binding is provably monomorphic (proven by
                // `record_monomorphic_var_bindings`, run before this scope's
                // statements). Otherwise drop the primitive fact so arithmetic
                // stays on the generic adaptive path, which re-checks operand
                // shapes at runtime instead of hard-committing to `AddInt` etc.
                let binding_type = self.gate_mutable_primitive_type(snode.span, binding_type);
                self.record_binding_type(pattern, binding_type.clone());
                self.maybe_register_owned_drop(pattern, binding_type.as_ref(), snode.span);
            }
            Node::ConstBinding { pattern, value, .. } => {
                // `const` is an immutable binding. When its initializer is in
                // the pure const-eval subset over a plain identifier, the
                // typechecker has already folded it; either way the VM
                // re-evaluates the same expression, producing the folded value
                // byte-for-byte. Lowered immutable (destructuring allowed).
                let binding_type = match &snode.node {
                    Node::ConstBinding {
                        type_ann: Some(type_ann),
                        ..
                    } => Some(type_ann.clone()),
                    _ => self.infer_expr_type(value),
                };
                self.compile_node(value)?;
                self.compile_destructuring(pattern, false, snode.span)?;
                self.record_binding_type(pattern, binding_type.clone());
                self.maybe_register_owned_drop(pattern, binding_type.as_ref(), snode.span);
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
            Node::NonNullAssert { operand } => {
                // `expr!` — identity when present, throws when `nil`. Leaves the
                // (non-nil) value on the stack. `JumpIfFalse` peeks, so the
                // `is_nil` bool is popped on both paths.
                self.compile_node(operand)?; // [value]
                self.chunk.emit(Op::Dup, self.line); // [value, value]
                self.chunk.emit(Op::Nil, self.line); // [value, value, nil]
                self.chunk.emit(Op::Equal, self.line); // [value, is_nil]
                let present_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                // nil path: drop the bool, throw a structured message.
                self.chunk.emit(Op::Pop, self.line); // [value]
                let idx =
                    self.string_constant("non-null assertion failed: value was nil (unwrap_nil)");
                self.chunk.emit_u16(Op::Constant, idx, self.line);
                self.chunk.emit(Op::Throw, self.line);
                // present path: drop the bool, leaving the value.
                self.chunk.patch_jump(present_jump);
                self.chunk.emit(Op::Pop, self.line); // [value]
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
            Node::FunctionCall { name, args, .. } => {
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
                let name_idx = self.string_constant(method);
                self.chunk
                    .emit_method_call_opt(name_idx, args.len() as u8, self.line);
            }
            Node::PropertyAccess { object, property } => {
                self.compile_property_access(object, property)?;
            }
            Node::OptionalPropertyAccess { object, property } => {
                self.compile_node(object)?;
                let idx = self.string_constant(property);
                self.chunk.emit_u16(Op::GetPropertyOpt, idx, self.line);
            }
            Node::SubscriptAccess { object, index } => {
                self.compile_node(object)?;
                self.compile_node(index)?;
                self.chunk.emit(Op::Subscript, self.line);
            }
            Node::OptionalSubscriptAccess { object, index } => {
                self.compile_node(object)?;
                self.compile_node(index)?;
                self.chunk.emit(Op::SubscriptOpt, self.line);
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
                ..
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
                self.compile_for_in(pattern, iterable, body, snode.span)?;
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
                name,
                type_params,
                params,
                body,
                is_stream,
                ..
            } => {
                self.compile_fn_decl(name, type_params, params, body, *is_stream)?;
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
            Node::EvalPackDecl {
                binding_name,
                pack_id,
                fields,
                body,
                summarize,
                ..
            } => {
                self.compile_eval_pack_decl(binding_name, pack_id, fields, body, summarize, true)?;
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
                let name_idx = self.string_constant("__range__");
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
                    let idx = self.string_constant("require condition failed");
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
            Node::MutexBlock { key, body } => {
                self.begin_scope();
                let finally_floor = self.finally_bodies.len();
                match key {
                    // `mutex(resource) { ... }`: evaluate the resource and key
                    // the lock on its structural value at runtime.
                    Some(key_expr) => {
                        self.compile_node(key_expr)?;
                        self.chunk.emit(Op::SyncMutexEnterKeyed, self.line);
                    }
                    // `mutex { ... }`: key on the lexical call-site (computed in
                    // the VM from the chunk + instruction pointer) so distinct
                    // blocks don't contend on one global lock.
                    None => {
                        self.chunk.emit(Op::SyncMutexEnter, self.line);
                    }
                }
                for sn in body {
                    self.compile_discarded_stmt(sn)?;
                }
                self.drain_finallys_to_floor(finally_floor)?;
                self.chunk.emit(Op::Nil, self.line);
                self.end_scope();
            }
            Node::ScopeBlock { body } => {
                // Structured-concurrency nursery. `TaskScopeEnter` pushes a task
                // scope; tasks spawned inside register to it. `TaskScopeExit`
                // joins them (propagating the first error, cancelling the rest).
                // On `throw`/early exit the scope is unwound and its tasks
                // cancelled by the frame/handler teardown, mirroring
                // `held_sync_guards`.
                self.begin_scope();
                let finally_floor = self.finally_bodies.len();
                self.chunk.emit(Op::TaskScopeEnter, self.line);
                for sn in body {
                    self.compile_discarded_stmt(sn)?;
                }
                self.drain_finallys_to_floor(finally_floor)?;
                self.chunk.emit(Op::TaskScopeExit, self.line);
                self.chunk.emit(Op::Nil, self.line);
                self.end_scope();
            }
            Node::DeferStmt { body } => {
                // Register the body to run on return/throw/scope-exit. The
                // statement emits no bytecode of its own — the deferred body
                // is inlined later by the finally-draining machinery — so it
                // leaves the operand stack untouched, matching
                // `produces_value` == false. Emitting a `Nil` here instead
                // leaked an unpopped slot per execution, which in a loop body
                // grew the operand stack without bound (surfaced by the
                // #2622 balance assertion).
                self.finally_bodies
                    .push(FinallyEntry::Finally(body.clone()));
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    self.compile_node(val)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                self.chunk.emit(Op::Yield, self.line);
            }
            Node::EmitExpr { value } => {
                self.compile_node(value)?;
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
            Node::ImportDecl { path, .. } => {
                let idx = self.string_constant(path);
                self.chunk.emit_u16(Op::Import, idx, self.line);
            }
            Node::SelectiveImport { names, path, .. } => {
                let path_idx = self.string_constant(path);
                let names_str = names.join(",");
                let names_idx = self.owned_string_constant(names_str);
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
            // Metadata-only declarations: enum names, struct/interface
            // layouts, and type aliases are pre-scanned, so they emit no
            // bytecode and leave the operand stack untouched. Type-alias names
            // in expression position lower directly to schema constants in the
            // `Identifier` arm above; eagerly binding every alias at top level
            // bloats large module init chunks past the VM's 64 KiB jump limit.
            // `produces_value` classifies them as non-value-producing to match;
            // contexts that require a block to yield a value (last statement of
            // a block, match-arm body) emit their own `Nil` placeholder.
            // Emitting one here instead left an unpopped `Nil` on the stack in
            // every value-discarding context (`compile_top_level_declarations`
            // pops nothing) — a latent imbalance surfaced by the #2622 balance
            // assertion.
            Node::EnumDecl { name, variants, .. } => {
                let declaration = (snode.span.start, snode.span.end);
                if !self.predeclared_enum_declarations.contains(&declaration) {
                    self.register_enum_decl(name, variants);
                }
            }
            Node::Pipeline { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. }
            | Node::InterfaceDecl { .. } => {}
            Node::TryCatch {
                has_catch: _,
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
                ..
            } => {
                self.compile_try_catch(body, error_var, error_type, catch_body, finally_body)?;
            }
            Node::TryExpr { body } => {
                self.compile_try_expr(body)?;
            }
            Node::Retry { count, body } => {
                self.compile_retry(count, body)?;
            }
            Node::CostRoute { options, body } => {
                self.compile_cost_route(options, body)?;
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
            Node::HitlExpr { kind, args } => {
                self.compile_hitl_expr(*kind, args)?;
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
