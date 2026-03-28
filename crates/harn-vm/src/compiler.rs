use harn_lexer::StringSegment;
use harn_parser::{Node, SNode, TypedParam};

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};

/// Compile error.
#[derive(Debug)]
pub struct CompileError {
    pub message: String,
    pub line: u32,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compile error at line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for CompileError {}

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
}

/// Compiles an AST into bytecode.
pub struct Compiler {
    chunk: Chunk,
    line: u32,
    column: u32,
    /// Track enum type names so PropertyAccess on them can produce EnumVariant.
    enum_names: std::collections::HashSet<String>,
    /// Stack of active loop contexts for break/continue.
    loop_stack: Vec<LoopContext>,
    /// Current depth of exception handlers (for cleanup on break/continue).
    handler_depth: usize,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            chunk: Chunk::new(),
            line: 1,
            column: 1,
            enum_names: std::collections::HashSet::new(),
            loop_stack: Vec::new(),
            handler_depth: 0,
        }
    }

    /// Compile a program (list of top-level nodes) into a Chunk.
    /// Finds the entry pipeline and compiles its body, including inherited bodies.
    pub fn compile(mut self, program: &[SNode]) -> Result<Chunk, CompileError> {
        // Pre-scan the entire program for enum declarations (including inside pipelines)
        // so we can recognize EnumName.Variant as enum construction.
        Self::collect_enum_names(program, &mut self.enum_names);

        // Compile all top-level non-pipeline declarations first (fn, enum, etc.)
        for sn in program {
            match &sn.node {
                Node::ImportDecl { .. } | Node::SelectiveImport { .. } => {
                    self.compile_node(sn)?;
                }
                _ => {}
            }
        }

        // Find entry pipeline
        let main = program
            .iter()
            .find(|sn| matches!(&sn.node, Node::Pipeline { name, .. } if name == "default"))
            .or_else(|| {
                program
                    .iter()
                    .find(|sn| matches!(&sn.node, Node::Pipeline { .. }))
            });

        if let Some(sn) = main {
            if let Node::Pipeline { body, extends, .. } = &sn.node {
                // If this pipeline extends another, compile the parent chain first
                if let Some(parent_name) = extends {
                    self.compile_parent_pipeline(program, parent_name)?;
                }
                self.compile_block(body)?;
            }
        }

        self.chunk.emit(Op::Nil, self.line);
        self.chunk.emit(Op::Return, self.line);
        Ok(self.chunk)
    }

    /// Compile a specific named pipeline (for test runners).
    pub fn compile_named(
        mut self,
        program: &[SNode],
        pipeline_name: &str,
    ) -> Result<Chunk, CompileError> {
        Self::collect_enum_names(program, &mut self.enum_names);

        for sn in program {
            if matches!(
                &sn.node,
                Node::ImportDecl { .. } | Node::SelectiveImport { .. }
            ) {
                self.compile_node(sn)?;
            }
        }

        let target = program
            .iter()
            .find(|sn| matches!(&sn.node, Node::Pipeline { name, .. } if name == pipeline_name));

        if let Some(sn) = target {
            if let Node::Pipeline { body, extends, .. } = &sn.node {
                if let Some(parent_name) = extends {
                    self.compile_parent_pipeline(program, parent_name)?;
                }
                self.compile_block(body)?;
            }
        }

        self.chunk.emit(Op::Nil, self.line);
        self.chunk.emit(Op::Return, self.line);
        Ok(self.chunk)
    }

    /// Recursively compile parent pipeline bodies (for extends).
    fn compile_parent_pipeline(
        &mut self,
        program: &[SNode],
        parent_name: &str,
    ) -> Result<(), CompileError> {
        let parent = program
            .iter()
            .find(|sn| matches!(&sn.node, Node::Pipeline { name, .. } if name == parent_name));
        if let Some(sn) = parent {
            if let Node::Pipeline { body, extends, .. } = &sn.node {
                // Recurse if this parent also extends another
                if let Some(grandparent) = extends {
                    self.compile_parent_pipeline(program, grandparent)?;
                }
                // Compile parent body - pop all statement values
                for stmt in body {
                    self.compile_node(stmt)?;
                    if Self::produces_value(&stmt.node) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        for (i, snode) in stmts.iter().enumerate() {
            self.compile_node(snode)?;
            // Pop intermediate expression results (keep last)
            if i < stmts.len() - 1 {
                // Only pop if the statement leaves a value on the stack
                if Self::produces_value(&snode.node) {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
        Ok(())
    }

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
            Node::StringLiteral(s) => {
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
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::GetVar, idx, self.line);
            }

            Node::LetBinding { name, value, .. } => {
                self.compile_node(value)?;
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefLet, idx, self.line);
            }

            Node::VarBinding { name, value, .. } => {
                self.compile_node(value)?;
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefVar, idx, self.line);
            }

            Node::Assignment {
                target, value, op, ..
            } => {
                if let Node::Identifier(name) = &target.node {
                    let idx = self.chunk.add_constant(Constant::String(name.clone()));
                    if let Some(op) = op {
                        self.chunk.emit_u16(Op::GetVar, idx, self.line);
                        self.compile_node(value)?;
                        self.emit_compound_op(op)?;
                        self.chunk.emit_u16(Op::SetVar, idx, self.line);
                    } else {
                        self.compile_node(value)?;
                        self.chunk.emit_u16(Op::SetVar, idx, self.line);
                    }
                } else if let Node::PropertyAccess { object, property } = &target.node {
                    // obj.field = value → SetProperty
                    if let Some(var_name) = self.root_var_name(object) {
                        let var_idx = self.chunk.add_constant(Constant::String(var_name.clone()));
                        let prop_idx = self.chunk.add_constant(Constant::String(property.clone()));
                        if let Some(op) = op {
                            // compound: obj.field += value
                            self.compile_node(target)?; // push current obj.field
                            self.compile_node(value)?;
                            self.emit_compound_op(op)?;
                        } else {
                            self.compile_node(value)?;
                        }
                        // Stack: [new_value]
                        // SetProperty reads var_idx from env, sets prop, writes back
                        self.chunk.emit_u16(Op::SetProperty, prop_idx, self.line);
                        // Encode the variable name index as a second u16
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
                    // obj[idx] = value → SetSubscript
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
            }

            Node::BinaryOp { op, left, right } => {
                // Short-circuit operators
                match op.as_str() {
                    "&&" => {
                        self.compile_node(left)?;
                        let jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_node(right)?;
                        self.chunk.patch_jump(jump);
                        // Normalize to bool
                        self.chunk.emit(Op::Not, self.line);
                        self.chunk.emit(Op::Not, self.line);
                        return Ok(());
                    }
                    "||" => {
                        self.compile_node(left)?;
                        let jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_node(right)?;
                        self.chunk.patch_jump(jump);
                        self.chunk.emit(Op::Not, self.line);
                        self.chunk.emit(Op::Not, self.line);
                        return Ok(());
                    }
                    "??" => {
                        self.compile_node(left)?;
                        self.chunk.emit(Op::Dup, self.line);
                        // Check if nil: push nil, compare
                        self.chunk.emit(Op::Nil, self.line);
                        self.chunk.emit(Op::NotEqual, self.line);
                        let jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                        self.chunk.emit(Op::Pop, self.line); // pop the not-equal result
                        self.chunk.emit(Op::Pop, self.line); // pop the nil value
                        self.compile_node(right)?;
                        let end = self.chunk.emit_jump(Op::Jump, self.line);
                        self.chunk.patch_jump(jump);
                        self.chunk.emit(Op::Pop, self.line); // pop the not-equal result
                        self.chunk.patch_jump(end);
                        return Ok(());
                    }
                    "|>" => {
                        self.compile_node(left)?;
                        self.compile_node(right)?;
                        self.chunk.emit(Op::Pipe, self.line);
                        return Ok(());
                    }
                    _ => {}
                }

                self.compile_node(left)?;
                self.compile_node(right)?;
                match op.as_str() {
                    "+" => self.chunk.emit(Op::Add, self.line),
                    "-" => self.chunk.emit(Op::Sub, self.line),
                    "*" => self.chunk.emit(Op::Mul, self.line),
                    "/" => self.chunk.emit(Op::Div, self.line),
                    "%" => self.chunk.emit(Op::Mod, self.line),
                    "==" => self.chunk.emit(Op::Equal, self.line),
                    "!=" => self.chunk.emit(Op::NotEqual, self.line),
                    "<" => self.chunk.emit(Op::Less, self.line),
                    ">" => self.chunk.emit(Op::Greater, self.line),
                    "<=" => self.chunk.emit(Op::LessEqual, self.line),
                    ">=" => self.chunk.emit(Op::GreaterEqual, self.line),
                    _ => {
                        return Err(CompileError {
                            message: format!("Unknown operator: {op}"),
                            line: self.line,
                        })
                    }
                }
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
                // Push function name as string constant
                let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                // Push arguments
                for arg in args {
                    self.compile_node(arg)?;
                }
                self.chunk.emit_u8(Op::Call, args.len() as u8, self.line);
            }

            Node::MethodCall {
                object,
                method,
                args,
            } => {
                // Check if this is an enum variant construction with args: EnumName.Variant(args)
                if let Node::Identifier(name) = &object.node {
                    if self.enum_names.contains(name) {
                        // Compile args, then BuildEnum
                        for arg in args {
                            self.compile_node(arg)?;
                        }
                        let enum_idx = self.chunk.add_constant(Constant::String(name.clone()));
                        let var_idx = self.chunk.add_constant(Constant::String(method.clone()));
                        self.chunk.emit_u16(Op::BuildEnum, enum_idx, self.line);
                        let hi = (var_idx >> 8) as u8;
                        let lo = var_idx as u8;
                        self.chunk.code.push(hi);
                        self.chunk.code.push(lo);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        let fc = args.len() as u16;
                        let fhi = (fc >> 8) as u8;
                        let flo = fc as u8;
                        self.chunk.code.push(fhi);
                        self.chunk.code.push(flo);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        return Ok(());
                    }
                }
                self.compile_node(object)?;
                for arg in args {
                    self.compile_node(arg)?;
                }
                let name_idx = self.chunk.add_constant(Constant::String(method.clone()));
                self.chunk
                    .emit_method_call(name_idx, args.len() as u8, self.line);
            }

            Node::PropertyAccess { object, property } => {
                // Check if this is an enum variant construction: EnumName.Variant
                if let Node::Identifier(name) = &object.node {
                    if self.enum_names.contains(name) {
                        // Emit BuildEnum with 0 fields
                        let enum_idx = self.chunk.add_constant(Constant::String(name.clone()));
                        let var_idx = self.chunk.add_constant(Constant::String(property.clone()));
                        self.chunk.emit_u16(Op::BuildEnum, enum_idx, self.line);
                        let hi = (var_idx >> 8) as u8;
                        let lo = var_idx as u8;
                        self.chunk.code.push(hi);
                        self.chunk.code.push(lo);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        // 0 fields
                        self.chunk.code.push(0);
                        self.chunk.code.push(0);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        self.chunk.lines.push(self.line);
                        self.chunk.columns.push(self.column);
                        return Ok(());
                    }
                }
                self.compile_node(object)?;
                let idx = self.chunk.add_constant(Constant::String(property.clone()));
                self.chunk.emit_u16(Op::GetProperty, idx, self.line);
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

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.compile_node(condition)?;
                let else_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_block(then_body)?;
                if let Some(else_body) = else_body {
                    let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                    self.chunk.patch_jump(else_jump);
                    self.chunk.emit(Op::Pop, self.line);
                    self.compile_block(else_body)?;
                    self.chunk.patch_jump(end_jump);
                } else {
                    self.chunk.patch_jump(else_jump);
                    self.chunk.emit(Op::Pop, self.line);
                    self.chunk.emit(Op::Nil, self.line);
                }
            }

            Node::WhileLoop { condition, body } => {
                let loop_start = self.chunk.current_offset();
                self.loop_stack.push(LoopContext {
                    start_offset: loop_start,
                    break_patches: Vec::new(),
                    has_iterator: false,
                    handler_depth: self.handler_depth,
                });
                self.compile_node(condition)?;
                let exit_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                                                     // Compile body statements, popping all results
                for sn in body {
                    self.compile_node(sn)?;
                    if Self::produces_value(&sn.node) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
                // Jump back to condition
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
                self.chunk.patch_jump(exit_jump);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                                                     // Patch all break jumps to here
                let ctx = self.loop_stack.pop().unwrap();
                for patch_pos in ctx.break_patches {
                    self.chunk.patch_jump(patch_pos);
                }
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::ForIn {
                variable,
                iterable,
                body,
            } => {
                // Compile iterable
                self.compile_node(iterable)?;
                // Variable name
                let var_idx = self.chunk.add_constant(Constant::String(variable.clone()));
                // Initialize iterator
                self.chunk.emit(Op::IterInit, self.line);
                let loop_start = self.chunk.current_offset();
                self.loop_stack.push(LoopContext {
                    start_offset: loop_start,
                    break_patches: Vec::new(),
                    has_iterator: true,
                    handler_depth: self.handler_depth,
                });
                // Try to get next item — jumps to end if exhausted
                let exit_jump_pos = self.chunk.emit_jump(Op::IterNext, self.line);
                // Define loop variable with current item (item is on stack from IterNext)
                self.chunk.emit_u16(Op::DefVar, var_idx, self.line);
                // Compile body statements, popping all results
                for sn in body {
                    self.compile_node(sn)?;
                    if Self::produces_value(&sn.node) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
                // Loop back
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
                self.chunk.patch_jump(exit_jump_pos);
                // Patch all break jumps to here
                let ctx = self.loop_stack.pop().unwrap();
                for patch_pos in ctx.break_patches {
                    self.chunk.patch_jump(patch_pos);
                }
                // Push nil as result (iterator state was consumed)
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    // Tail call optimization: if returning a direct function call,
                    // emit TailCall instead of Call to reuse the current frame.
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
                            // Tail pipe optimization: `return x |> f` becomes a tail call.
                            // Compile left side (value) — inner pipes compile normally.
                            self.compile_node(left)?;
                            // Compile right side (callable reference).
                            self.compile_node(right)?;
                            // Stack is now [value, callable]. TailCall expects [callable, args...],
                            // so swap to get [callable, value] then tail-call with 1 arg.
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

            Node::BreakStmt => {
                if self.loop_stack.is_empty() {
                    return Err(CompileError {
                        message: "break outside of loop".to_string(),
                        line: self.line,
                    });
                }
                let ctx = self.loop_stack.last().unwrap();
                // Pop exception handlers that were pushed inside the loop
                for _ in ctx.handler_depth..self.handler_depth {
                    self.chunk.emit(Op::PopHandler, self.line);
                }
                // Pop iterator if breaking from a for-in loop
                if ctx.has_iterator {
                    self.chunk.emit(Op::PopIterator, self.line);
                }
                let patch = self.chunk.emit_jump(Op::Jump, self.line);
                self.loop_stack
                    .last_mut()
                    .unwrap()
                    .break_patches
                    .push(patch);
            }

            Node::ContinueStmt => {
                if self.loop_stack.is_empty() {
                    return Err(CompileError {
                        message: "continue outside of loop".to_string(),
                        line: self.line,
                    });
                }
                let ctx = self.loop_stack.last().unwrap();
                // Pop exception handlers that were pushed inside the loop
                for _ in ctx.handler_depth..self.handler_depth {
                    self.chunk.emit(Op::PopHandler, self.line);
                }
                let loop_start = ctx.start_offset;
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
            }

            Node::ListLiteral(elements) => {
                for el in elements {
                    self.compile_node(el)?;
                }
                self.chunk
                    .emit_u16(Op::BuildList, elements.len() as u16, self.line);
            }

            Node::DictLiteral(entries) => {
                for entry in entries {
                    self.compile_node(&entry.key)?;
                    self.compile_node(&entry.value)?;
                }
                self.chunk
                    .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
            }

            Node::InterpolatedString(segments) => {
                let mut part_count = 0u16;
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => {
                            let idx = self.chunk.add_constant(Constant::String(s.clone()));
                            self.chunk.emit_u16(Op::Constant, idx, self.line);
                            part_count += 1;
                        }
                        StringSegment::Expression(expr_str) => {
                            // Parse and compile the embedded expression
                            let mut lexer = harn_lexer::Lexer::new(expr_str);
                            if let Ok(tokens) = lexer.tokenize() {
                                let mut parser = harn_parser::Parser::new(tokens);
                                if let Ok(snode) = parser.parse_single_expression() {
                                    self.compile_node(&snode)?;
                                    // Convert result to string for concatenation
                                    let to_str = self
                                        .chunk
                                        .add_constant(Constant::String("to_string".into()));
                                    self.chunk.emit_u16(Op::Constant, to_str, self.line);
                                    self.chunk.emit(Op::Swap, self.line);
                                    self.chunk.emit_u8(Op::Call, 1, self.line);
                                    part_count += 1;
                                } else {
                                    // Fallback: treat as literal string
                                    let idx =
                                        self.chunk.add_constant(Constant::String(expr_str.clone()));
                                    self.chunk.emit_u16(Op::Constant, idx, self.line);
                                    part_count += 1;
                                }
                            }
                        }
                    }
                }
                if part_count > 1 {
                    self.chunk.emit_u16(Op::Concat, part_count, self.line);
                }
            }

            Node::FnDecl {
                name, params, body, ..
            } => {
                // Compile function body into a separate chunk
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Nil, self.line);
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: name.clone(),
                    params: TypedParam::names(params),
                    chunk: fn_compiler.chunk,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);

                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
            }

            Node::Closure { params, body } => {
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                // If block didn't end with return, the last value is on the stack
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: "<closure>".to_string(),
                    params: TypedParam::names(params),
                    chunk: fn_compiler.chunk,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);

                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
            }

            Node::ThrowStmt { value } => {
                self.compile_node(value)?;
                self.chunk.emit(Op::Throw, self.line);
            }

            Node::MatchExpr { value, arms } => {
                self.compile_node(value)?;
                let mut end_jumps = Vec::new();
                for arm in arms {
                    match &arm.pattern.node {
                        // Wildcard `_` — always matches
                        Node::Identifier(name) if name == "_" => {
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        }
                        // Enum destructuring: EnumConstruct pattern
                        Node::EnumConstruct {
                            enum_name,
                            variant,
                            args: pat_args,
                        } => {
                            // Check if the match value is this enum variant
                            self.chunk.emit(Op::Dup, self.line);
                            let en_idx =
                                self.chunk.add_constant(Constant::String(enum_name.clone()));
                            let vn_idx = self.chunk.add_constant(Constant::String(variant.clone()));
                            self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                            let hi = (vn_idx >> 8) as u8;
                            let lo = vn_idx as u8;
                            self.chunk.code.push(hi);
                            self.chunk.code.push(lo);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            // Stack: [match_value, bool]
                            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool

                            // Destructure: bind field variables from the enum's fields
                            // The match value is still on the stack; we need to extract fields
                            for (i, pat_arg) in pat_args.iter().enumerate() {
                                if let Node::Identifier(binding_name) = &pat_arg.node {
                                    // Dup the match value, get .fields, subscript [i]
                                    self.chunk.emit(Op::Dup, self.line);
                                    let fields_idx = self
                                        .chunk
                                        .add_constant(Constant::String("fields".to_string()));
                                    self.chunk.emit_u16(Op::GetProperty, fields_idx, self.line);
                                    let idx_const =
                                        self.chunk.add_constant(Constant::Int(i as i64));
                                    self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                                    self.chunk.emit(Op::Subscript, self.line);
                                    let name_idx = self
                                        .chunk
                                        .add_constant(Constant::String(binding_name.clone()));
                                    self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                                }
                            }

                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                            self.chunk.patch_jump(skip);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                        }
                        // Enum variant without args: PropertyAccess(EnumName, Variant)
                        Node::PropertyAccess { object, property } if matches!(&object.node, Node::Identifier(n) if self.enum_names.contains(n)) =>
                        {
                            let enum_name = if let Node::Identifier(n) = &object.node {
                                n.clone()
                            } else {
                                unreachable!()
                            };
                            self.chunk.emit(Op::Dup, self.line);
                            let en_idx = self.chunk.add_constant(Constant::String(enum_name));
                            let vn_idx =
                                self.chunk.add_constant(Constant::String(property.clone()));
                            self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                            let hi = (vn_idx >> 8) as u8;
                            let lo = vn_idx as u8;
                            self.chunk.code.push(hi);
                            self.chunk.code.push(lo);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                            self.chunk.patch_jump(skip);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                        }
                        // Enum destructuring via MethodCall: EnumName.Variant(bindings...)
                        // Parser produces MethodCall for EnumName.Variant(x) patterns
                        Node::MethodCall {
                            object,
                            method,
                            args: pat_args,
                        } if matches!(&object.node, Node::Identifier(n) if self.enum_names.contains(n)) =>
                        {
                            let enum_name = if let Node::Identifier(n) = &object.node {
                                n.clone()
                            } else {
                                unreachable!()
                            };
                            // Check if the match value is this enum variant
                            self.chunk.emit(Op::Dup, self.line);
                            let en_idx = self.chunk.add_constant(Constant::String(enum_name));
                            let vn_idx = self.chunk.add_constant(Constant::String(method.clone()));
                            self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                            let hi = (vn_idx >> 8) as u8;
                            let lo = vn_idx as u8;
                            self.chunk.code.push(hi);
                            self.chunk.code.push(lo);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            self.chunk.lines.push(self.line);
                            self.chunk.columns.push(self.column);
                            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool

                            // Destructure: bind field variables
                            for (i, pat_arg) in pat_args.iter().enumerate() {
                                if let Node::Identifier(binding_name) = &pat_arg.node {
                                    self.chunk.emit(Op::Dup, self.line);
                                    let fields_idx = self
                                        .chunk
                                        .add_constant(Constant::String("fields".to_string()));
                                    self.chunk.emit_u16(Op::GetProperty, fields_idx, self.line);
                                    let idx_const =
                                        self.chunk.add_constant(Constant::Int(i as i64));
                                    self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                                    self.chunk.emit(Op::Subscript, self.line);
                                    let name_idx = self
                                        .chunk
                                        .add_constant(Constant::String(binding_name.clone()));
                                    self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                                }
                            }

                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                            self.chunk.patch_jump(skip);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                        }
                        // Binding pattern: bare identifier (not a literal)
                        Node::Identifier(name) => {
                            // Bind the match value to this name, always matches
                            self.chunk.emit(Op::Dup, self.line); // dup for binding
                            let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                            self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        }
                        // Literal/expression pattern — compare with Equal
                        _ => {
                            self.chunk.emit(Op::Dup, self.line);
                            self.compile_node(&arm.pattern)?;
                            self.chunk.emit(Op::Equal, self.line);
                            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                            self.chunk.patch_jump(skip);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                        }
                    }
                }
                // No match — pop value, push nil
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.emit(Op::Nil, self.line);
                for j in end_jumps {
                    self.chunk.patch_jump(j);
                }
            }

            Node::RangeExpr {
                start,
                end,
                inclusive,
            } => {
                // Compile as __range__(start, end, inclusive_bool) builtin call
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
                // guard condition else { body }
                // Compile condition; if truthy, skip else_body
                self.compile_node(condition)?;
                let skip_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                                                     // Compile else_body
                self.compile_block(else_body)?;
                // Pop result of else_body (guard is a statement, not expression)
                if !else_body.is_empty() && Self::produces_value(&else_body.last().unwrap().node) {
                    self.chunk.emit(Op::Pop, self.line);
                }
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(skip_jump);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                self.chunk.patch_jump(end_jump);
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::Block(stmts) => {
                if stmts.is_empty() {
                    self.chunk.emit(Op::Nil, self.line);
                } else {
                    self.compile_block(stmts)?;
                }
            }

            Node::DeadlineBlock { duration, body } => {
                self.compile_node(duration)?;
                self.chunk.emit(Op::DeadlineSetup, self.line);
                if body.is_empty() {
                    self.chunk.emit(Op::Nil, self.line);
                } else {
                    self.compile_block(body)?;
                }
                self.chunk.emit(Op::DeadlineEnd, self.line);
            }

            Node::MutexBlock { body } => {
                // v1: single-threaded, just compile the body
                if body.is_empty() {
                    self.chunk.emit(Op::Nil, self.line);
                } else {
                    // Compile body, but pop intermediate values and push nil at the end.
                    // The body typically contains statements (assignments) that don't produce values.
                    for sn in body {
                        self.compile_node(sn)?;
                        if Self::produces_value(&sn.node) {
                            self.chunk.emit(Op::Pop, self.line);
                        }
                    }
                    self.chunk.emit(Op::Nil, self.line);
                }
            }

            Node::YieldExpr { .. } => {
                // v1: yield is host-integration only, emit nil
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::AskExpr { fields } => {
                // Compile as a dict literal and call llm_call builtin
                // For v1, just build the dict (llm_call requires async)
                for entry in fields {
                    self.compile_node(&entry.key)?;
                    self.compile_node(&entry.value)?;
                }
                self.chunk
                    .emit_u16(Op::BuildDict, fields.len() as u16, self.line);
            }

            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                // Push field values onto the stack, then BuildEnum
                for arg in args {
                    self.compile_node(arg)?;
                }
                let enum_idx = self.chunk.add_constant(Constant::String(enum_name.clone()));
                let var_idx = self.chunk.add_constant(Constant::String(variant.clone()));
                // BuildEnum: enum_name_idx, variant_idx, field_count
                self.chunk.emit_u16(Op::BuildEnum, enum_idx, self.line);
                let hi = (var_idx >> 8) as u8;
                let lo = var_idx as u8;
                self.chunk.code.push(hi);
                self.chunk.code.push(lo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                let fc = args.len() as u16;
                let fhi = (fc >> 8) as u8;
                let flo = fc as u8;
                self.chunk.code.push(fhi);
                self.chunk.code.push(flo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
            }

            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                // Build as a dict with a __struct__ key for metadata
                let struct_key = self
                    .chunk
                    .add_constant(Constant::String("__struct__".to_string()));
                let struct_val = self
                    .chunk
                    .add_constant(Constant::String(struct_name.clone()));
                self.chunk.emit_u16(Op::Constant, struct_key, self.line);
                self.chunk.emit_u16(Op::Constant, struct_val, self.line);

                for entry in fields {
                    self.compile_node(&entry.key)?;
                    self.compile_node(&entry.value)?;
                }
                self.chunk
                    .emit_u16(Op::BuildDict, (fields.len() + 1) as u16, self.line);
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

            // Declarations that only register metadata (no runtime effect needed for v1)
            Node::Pipeline { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. }
            | Node::EnumDecl { .. }
            | Node::StructDecl { .. }
            | Node::InterfaceDecl { .. } => {
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
            } => {
                // Extract the type name for typed catch (e.g., "AppError")
                let type_name = error_type.as_ref().and_then(|te| {
                    // TypeExpr is a Named(String) for simple type names
                    if let harn_parser::TypeExpr::Named(name) = te {
                        Some(name.clone())
                    } else {
                        None
                    }
                });

                // Store the error type name as a constant (or empty string for untyped)
                let type_name_idx = if let Some(ref tn) = type_name {
                    self.chunk.add_constant(Constant::String(tn.clone()))
                } else {
                    self.chunk.add_constant(Constant::String(String::new()))
                };

                // 1. Emit TryCatchSetup with placeholder offset to catch handler
                self.handler_depth += 1;
                let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                // Emit the type name index as extra u16 after the jump offset
                let hi = (type_name_idx >> 8) as u8;
                let lo = type_name_idx as u8;
                self.chunk.code.push(hi);
                self.chunk.code.push(lo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);

                // 2. Compile try body
                if body.is_empty() {
                    self.chunk.emit(Op::Nil, self.line);
                } else {
                    self.compile_block(body)?;
                    // If last statement doesn't produce a value, push nil
                    if !Self::produces_value(&body.last().unwrap().node) {
                        self.chunk.emit(Op::Nil, self.line);
                    }
                }

                // 3. Emit PopHandler (successful try body completion)
                self.handler_depth -= 1;
                self.chunk.emit(Op::PopHandler, self.line);

                // 4. Emit Jump past catch body
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                // 5. Patch the catch offset to point here
                self.chunk.patch_jump(catch_jump);

                // 6. Error value is on the stack from the handler.
                //    If error_var exists, bind it; otherwise pop the error value.
                if let Some(var_name) = error_var {
                    let idx = self.chunk.add_constant(Constant::String(var_name.clone()));
                    self.chunk.emit_u16(Op::DefLet, idx, self.line);
                } else {
                    self.chunk.emit(Op::Pop, self.line);
                }

                // 7. Compile catch body
                if catch_body.is_empty() {
                    self.chunk.emit(Op::Nil, self.line);
                } else {
                    self.compile_block(catch_body)?;
                    if !Self::produces_value(&catch_body.last().unwrap().node) {
                        self.chunk.emit(Op::Nil, self.line);
                    }
                }

                // 8. Patch the end jump
                self.chunk.patch_jump(end_jump);
            }

            Node::Retry { count, body } => {
                // Compile count expression into a mutable counter variable
                self.compile_node(count)?;
                let counter_name = "__retry_counter__";
                let counter_idx = self
                    .chunk
                    .add_constant(Constant::String(counter_name.to_string()));
                self.chunk.emit_u16(Op::DefVar, counter_idx, self.line);

                // Also store the last error for re-throwing
                self.chunk.emit(Op::Nil, self.line);
                let err_name = "__retry_last_error__";
                let err_idx = self
                    .chunk
                    .add_constant(Constant::String(err_name.to_string()));
                self.chunk.emit_u16(Op::DefVar, err_idx, self.line);

                // Loop start
                let loop_start = self.chunk.current_offset();

                // Set up try/catch (untyped - empty type name)
                let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                // Emit empty type name for untyped catch
                let empty_type = self.chunk.add_constant(Constant::String(String::new()));
                let hi = (empty_type >> 8) as u8;
                let lo = empty_type as u8;
                self.chunk.code.push(hi);
                self.chunk.code.push(lo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);

                // Compile body
                self.compile_block(body)?;

                // Success: pop handler, jump to end
                self.chunk.emit(Op::PopHandler, self.line);
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                // Catch handler
                self.chunk.patch_jump(catch_jump);
                // Save the error value for potential re-throw
                self.chunk.emit(Op::Dup, self.line);
                self.chunk.emit_u16(Op::SetVar, err_idx, self.line);
                // Pop the error value
                self.chunk.emit(Op::Pop, self.line);

                // Decrement counter
                self.chunk.emit_u16(Op::GetVar, counter_idx, self.line);
                let one_idx = self.chunk.add_constant(Constant::Int(1));
                self.chunk.emit_u16(Op::Constant, one_idx, self.line);
                self.chunk.emit(Op::Sub, self.line);
                self.chunk.emit(Op::Dup, self.line);
                self.chunk.emit_u16(Op::SetVar, counter_idx, self.line);

                // If counter > 0, jump to loop start
                let zero_idx = self.chunk.add_constant(Constant::Int(0));
                self.chunk.emit_u16(Op::Constant, zero_idx, self.line);
                self.chunk.emit(Op::Greater, self.line);
                let retry_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);

                // No more retries — re-throw the last error
                self.chunk.patch_jump(retry_jump);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                self.chunk.emit_u16(Op::GetVar, err_idx, self.line);
                self.chunk.emit(Op::Throw, self.line);

                self.chunk.patch_jump(end_jump);
                // Push nil as the result of a successful retry block
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::Parallel {
                count,
                variable,
                body,
            } => {
                self.compile_node(count)?;
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Return, self.line);
                let params = vec![variable.clone().unwrap_or_else(|| "__i__".to_string())];
                let func = CompiledFunction {
                    name: "<parallel>".to_string(),
                    params,
                    chunk: fn_compiler.chunk,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::Parallel, self.line);
            }

            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                self.compile_node(list)?;
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Return, self.line);
                let func = CompiledFunction {
                    name: "<parallel_map>".to_string(),
                    params: vec![variable.clone()],
                    chunk: fn_compiler.chunk,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::ParallelMap, self.line);
            }

            Node::SpawnExpr { body } => {
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Return, self.line);
                let func = CompiledFunction {
                    name: "<spawn>".to_string(),
                    params: vec![],
                    chunk: fn_compiler.chunk,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::Spawn, self.line);
            }
        }
        Ok(())
    }

    /// Check if a node produces a value on the stack that needs to be popped.
    fn produces_value(node: &Node) -> bool {
        match node {
            // These nodes do NOT produce a value on the stack
            Node::LetBinding { .. }
            | Node::VarBinding { .. }
            | Node::Assignment { .. }
            | Node::ReturnStmt { .. }
            | Node::FnDecl { .. }
            | Node::ThrowStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt => false,
            // These compound nodes explicitly produce a value
            Node::TryCatch { .. }
            | Node::Retry { .. }
            | Node::GuardStmt { .. }
            | Node::DeadlineBlock { .. }
            | Node::MutexBlock { .. } => true,
            // All other expressions produce values
            _ => true,
        }
    }
}

impl Compiler {
    /// Compile a function body into a CompiledFunction (for import support).
    pub fn compile_fn_body(
        &mut self,
        params: &[TypedParam],
        body: &[SNode],
    ) -> Result<CompiledFunction, CompileError> {
        let mut fn_compiler = Compiler::new();
        fn_compiler.compile_block(body)?;
        fn_compiler.chunk.emit(Op::Nil, 0);
        fn_compiler.chunk.emit(Op::Return, 0);
        Ok(CompiledFunction {
            name: String::new(),
            params: TypedParam::names(params),
            chunk: fn_compiler.chunk,
        })
    }

    /// Compile a match arm body, ensuring it always pushes exactly one value.
    fn compile_match_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        if body.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(body)?;
            // If the last statement doesn't produce a value, push nil
            if !Self::produces_value(&body.last().unwrap().node) {
                self.chunk.emit(Op::Nil, self.line);
            }
        }
        Ok(())
    }

    /// Emit the binary op instruction for a compound assignment operator.
    fn emit_compound_op(&mut self, op: &str) -> Result<(), CompileError> {
        match op {
            "+" => self.chunk.emit(Op::Add, self.line),
            "-" => self.chunk.emit(Op::Sub, self.line),
            "*" => self.chunk.emit(Op::Mul, self.line),
            "/" => self.chunk.emit(Op::Div, self.line),
            "%" => self.chunk.emit(Op::Mod, self.line),
            _ => {
                return Err(CompileError {
                    message: format!("Unknown compound operator: {op}"),
                    line: self.line,
                })
            }
        }
        Ok(())
    }

    /// Extract the root variable name from a (possibly nested) access expression.
    fn root_var_name(&self, node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.root_var_name(object)
            }
            Node::SubscriptAccess { object, .. } => self.root_var_name(object),
            _ => None,
        }
    }
}

impl Compiler {
    /// Recursively collect all enum type names from the AST.
    fn collect_enum_names(nodes: &[SNode], names: &mut std::collections::HashSet<String>) {
        for sn in nodes {
            match &sn.node {
                Node::EnumDecl { name, .. } => {
                    names.insert(name.clone());
                }
                Node::Pipeline { body, .. } => {
                    Self::collect_enum_names(body, names);
                }
                Node::FnDecl { body, .. } => {
                    Self::collect_enum_names(body, names);
                }
                Node::Block(stmts) => {
                    Self::collect_enum_names(stmts, names);
                }
                _ => {}
            }
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn compile_source(source: &str) -> Chunk {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        Compiler::new().compile(&program).unwrap()
    }

    #[test]
    fn test_compile_arithmetic() {
        let chunk = compile_source("pipeline test(task) { let x = 2 + 3 }");
        assert!(!chunk.code.is_empty());
        // Should have constants: 2, 3, "x"
        assert!(chunk.constants.contains(&Constant::Int(2)));
        assert!(chunk.constants.contains(&Constant::Int(3)));
    }

    #[test]
    fn test_compile_function_call() {
        let chunk = compile_source("pipeline test(task) { log(42) }");
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("CALL"));
    }

    #[test]
    fn test_compile_if_else() {
        let chunk =
            compile_source(r#"pipeline test(task) { if true { log("yes") } else { log("no") } }"#);
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("JUMP_IF_FALSE"));
        assert!(disasm.contains("JUMP"));
    }

    #[test]
    fn test_compile_while() {
        let chunk = compile_source("pipeline test(task) { var i = 0\n while i < 5 { i = i + 1 } }");
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("JUMP_IF_FALSE"));
        // Should have a backward jump
        assert!(disasm.contains("JUMP"));
    }

    #[test]
    fn test_compile_closure() {
        let chunk = compile_source("pipeline test(task) { let f = { x -> x * 2 } }");
        assert!(!chunk.functions.is_empty());
        assert_eq!(chunk.functions[0].params, vec!["x"]);
    }

    #[test]
    fn test_compile_list() {
        let chunk = compile_source("pipeline test(task) { let a = [1, 2, 3] }");
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("BUILD_LIST"));
    }

    #[test]
    fn test_compile_dict() {
        let chunk = compile_source(r#"pipeline test(task) { let d = {name: "test"} }"#);
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("BUILD_DICT"));
    }

    #[test]
    fn test_disassemble() {
        let chunk = compile_source("pipeline test(task) { log(2 + 3) }");
        let disasm = chunk.disassemble("test");
        // Should be readable
        assert!(disasm.contains("CONSTANT"));
        assert!(disasm.contains("ADD"));
        assert!(disasm.contains("CALL"));
    }
}
