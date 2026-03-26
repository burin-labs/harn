use harn_lexer::StringSegment;
use harn_parser::{Node, TypedParam};

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

/// Compiles an AST into bytecode.
pub struct Compiler {
    chunk: Chunk,
    line: u32,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            chunk: Chunk::new(),
            line: 1,
        }
    }

    /// Compile a program (list of top-level nodes) into a Chunk.
    /// Finds the entry pipeline and compiles its body.
    pub fn compile(mut self, program: &[Node]) -> Result<Chunk, CompileError> {
        // Find entry pipeline
        let main = program
            .iter()
            .find(|n| matches!(n, Node::Pipeline { name, .. } if name == "default"))
            .or_else(|| program.iter().find(|n| matches!(n, Node::Pipeline { .. })));

        if let Some(Node::Pipeline { body, .. }) = main {
            self.compile_block(body)?;
        }

        self.chunk.emit(Op::Nil, self.line);
        self.chunk.emit(Op::Return, self.line);
        Ok(self.chunk)
    }

    fn compile_block(&mut self, stmts: &[Node]) -> Result<(), CompileError> {
        for (i, stmt) in stmts.iter().enumerate() {
            self.compile_node(stmt)?;
            // Pop intermediate expression results (keep last)
            if i < stmts.len() - 1 {
                // Only pop if the statement leaves a value on the stack
                if Self::produces_value(stmt) {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
        Ok(())
    }

    fn compile_node(&mut self, node: &Node) -> Result<(), CompileError> {
        match node {
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

            Node::Assignment { target, value } => {
                self.compile_node(value)?;
                if let Node::Identifier(name) = target.as_ref() {
                    let idx = self.chunk.add_constant(Constant::String(name.clone()));
                    self.chunk.emit_u16(Op::SetVar, idx, self.line);
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
                self.compile_node(object)?;
                for arg in args {
                    self.compile_node(arg)?;
                }
                let name_idx = self.chunk.add_constant(Constant::String(method.clone()));
                self.chunk
                    .emit_method_call(name_idx, args.len() as u8, self.line);
            }

            Node::PropertyAccess { object, property } => {
                self.compile_node(object)?;
                let idx = self.chunk.add_constant(Constant::String(property.clone()));
                self.chunk.emit_u16(Op::GetProperty, idx, self.line);
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
                self.compile_node(condition)?;
                let exit_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                                                     // Compile body statements, popping all results
                for stmt in body {
                    self.compile_node(stmt)?;
                    if Self::produces_value(stmt) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
                // Jump back to condition
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
                self.chunk.patch_jump(exit_jump);
                self.chunk.emit(Op::Pop, self.line); // pop condition
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
                // Try to get next item — jumps to end if exhausted
                let exit_jump_pos = self.chunk.emit_jump(Op::IterNext, self.line);
                // Define loop variable with current item (item is on stack from IterNext)
                self.chunk.emit_u16(Op::DefVar, var_idx, self.line);
                // Compile body
                self.compile_block(body)?;
                self.chunk.emit(Op::Pop, self.line);
                // Loop back
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
                self.chunk.patch_jump(exit_jump_pos);
                // Push nil as result (iterator state was consumed)
                self.chunk.emit(Op::Nil, self.line);
            }

            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    self.compile_node(val)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                self.chunk.emit(Op::Return, self.line);
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
                            // For interpolated expressions, we need to parse and compile them
                            // For the VM, we store the expression source as a constant
                            // and evaluate it at runtime (same approach as the tree-walker)
                            let idx = self.chunk.add_constant(Constant::String(expr_str.clone()));
                            self.chunk.emit_u16(Op::Constant, idx, self.line);
                            part_count += 1;
                            // Mark that this part needs expression evaluation
                            // For now, we do string concat of the parts
                            // TODO: proper interpolation in VM requires sub-compilation
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
                // For now, throw is handled by the VM as an error
                // We use a special constant to signal throw
                let idx = self
                    .chunk
                    .add_constant(Constant::String("__throw__".to_string()));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
            }

            Node::MatchExpr { value, arms } => {
                self.compile_node(value)?;
                let mut end_jumps = Vec::new();
                for arm in arms {
                    self.chunk.emit(Op::Dup, self.line);
                    self.compile_node(&arm.pattern)?;
                    self.chunk.emit(Op::Equal, self.line);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line); // pop bool
                    self.chunk.emit(Op::Pop, self.line); // pop match value
                    self.compile_block(&arm.body)?;
                    end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line); // pop bool
                }
                // No match — pop value, push nil
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.emit(Op::Nil, self.line);
                for j in end_jumps {
                    self.chunk.patch_jump(j);
                }
            }

            // Statements that don't produce values
            Node::Pipeline { .. }
            | Node::ImportDecl { .. }
            | Node::OverrideDecl { .. }
            | Node::TypeDecl { .. }
            | Node::EnumDecl { .. }
            | Node::StructDecl { .. }
            | Node::EnumConstruct { .. }
            | Node::StructConstruct { .. }
            | Node::DurationLiteral(_)
            | Node::RangeExpr { .. }
            | Node::GuardStmt { .. }
            | Node::AskExpr { .. }
            | Node::DeadlineBlock { .. }
            | Node::YieldExpr { .. }
            | Node::Block(_) => {
                self.chunk.emit(Op::Nil, self.line);
            }

            // Features that fall back to tree-walker (not compiled)
            Node::TryCatch { .. }
            | Node::Retry { .. }
            | Node::Parallel { .. }
            | Node::ParallelMap { .. }
            | Node::SpawnExpr { .. } => {
                // These are complex control flow that the VM delegates to the tree-walker
                self.chunk.emit(Op::Nil, self.line);
            }
        }
        Ok(())
    }

    /// Check if a node produces a value on the stack that needs to be popped.
    fn produces_value(node: &Node) -> bool {
        !matches!(
            node,
            Node::LetBinding { .. }
                | Node::VarBinding { .. }
                | Node::Assignment { .. }
                | Node::ReturnStmt { .. }
                | Node::FnDecl { .. }
        )
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
