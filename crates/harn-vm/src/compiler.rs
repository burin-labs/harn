use harn_lexer::StringSegment;
use harn_parser::{BindingPattern, Node, SNode, TypedParam};

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
    /// Number of pending finally bodies at loop entry.
    finally_depth: usize,
    /// Lexical scope depth at loop entry.
    scope_depth: usize,
}

/// Compiles an AST into bytecode.
pub struct Compiler {
    chunk: Chunk,
    line: u32,
    column: u32,
    /// Track enum type names so PropertyAccess on them can produce EnumVariant.
    enum_names: std::collections::HashSet<String>,
    /// Track interface names → method names for runtime enforcement.
    interface_methods: std::collections::HashMap<String, Vec<String>>,
    /// Stack of active loop contexts for break/continue.
    loop_stack: Vec<LoopContext>,
    /// Current depth of exception handlers (for cleanup on break/continue).
    handler_depth: usize,
    /// Stack of pending finally bodies for return/break/continue handling.
    finally_bodies: Vec<Vec<SNode>>,
    /// Counter for unique temp variable names.
    temp_counter: usize,
    /// Number of lexical block scopes currently active in this compiled frame.
    scope_depth: usize,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            chunk: Chunk::new(),
            line: 1,
            column: 1,
            enum_names: std::collections::HashSet::new(),
            interface_methods: std::collections::HashMap::new(),
            loop_stack: Vec::new(),
            handler_depth: 0,
            finally_bodies: Vec::new(),
            temp_counter: 0,
            scope_depth: 0,
        }
    }

    /// Compile a program (list of top-level nodes) into a Chunk.
    /// Finds the entry pipeline and compiles its body, including inherited bodies.
    pub fn compile(mut self, program: &[SNode]) -> Result<Chunk, CompileError> {
        // Pre-scan the entire program for enum declarations (including inside pipelines)
        // so we can recognize EnumName.Variant as enum construction.
        Self::collect_enum_names(program, &mut self.enum_names);
        // Built-in Result enum is always available
        self.enum_names.insert("Result".to_string());
        Self::collect_interface_methods(program, &mut self.interface_methods);

        // Compile all top-level imports first
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
            self.compile_top_level_declarations(program)?;
            if let Node::Pipeline { body, extends, .. } = &sn.node {
                // If this pipeline extends another, compile the parent chain first
                if let Some(parent_name) = extends {
                    self.compile_parent_pipeline(program, parent_name)?;
                }
                self.compile_block(body)?;
            }
        } else {
            // No pipeline found — compile all top-level statements as an
            // implicit entry point (script mode).
            let top_level: Vec<&SNode> = program
                .iter()
                .filter(|sn| {
                    !matches!(
                        &sn.node,
                        Node::ImportDecl { .. } | Node::SelectiveImport { .. }
                    )
                })
                .collect();
            for sn in &top_level {
                self.compile_node(sn)?;
                if Self::produces_value(&sn.node) {
                    self.chunk.emit(Op::Pop, self.line);
                }
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
        Self::collect_interface_methods(program, &mut self.interface_methods);

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
            self.compile_top_level_declarations(program)?;
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

    /// Emit bytecode preamble for default parameter values.
    /// For each param with a default at index i, emits:
    ///   GetArgc; PushInt (i+1); GreaterEqual; JumpIfTrue <skip>;
    ///   [compile default expr]; DefLet param_name; <skip>:
    fn emit_default_preamble(&mut self, params: &[TypedParam]) -> Result<(), CompileError> {
        for (i, param) in params.iter().enumerate() {
            if let Some(default_expr) = &param.default_value {
                self.chunk.emit(Op::GetArgc, self.line);
                let threshold_idx = self.chunk.add_constant(Constant::Int((i + 1) as i64));
                self.chunk.emit_u16(Op::Constant, threshold_idx, self.line);
                // argc >= (i+1) means arg was provided
                self.chunk.emit(Op::GreaterEqual, self.line);
                let skip_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                // Pop the boolean from JumpIfTrue (it doesn't pop)
                self.chunk.emit(Op::Pop, self.line);
                // Compile the default expression
                self.compile_node(default_expr)?;
                let name_idx = self
                    .chunk
                    .add_constant(Constant::String(param.name.clone()));
                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(skip_jump);
                // Pop the boolean left by JumpIfTrue on the true path
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.patch_jump(end_jump);
            }
        }
        Ok(())
    }

    /// Emit runtime type checks for parameters with type annotations.
    /// For each param with a type annotation, emits CheckType(var_name, type_name)
    /// or calls __assert_shape for shape types.
    fn emit_type_checks(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                // Handle shape types via __assert_shape builtin call
                if let harn_parser::TypeExpr::Shape(fields) = type_expr {
                    let spec = Self::shape_to_spec_string(fields);
                    // Emit: __assert_shape(param_value, param_name, spec)
                    let fn_idx = self
                        .chunk
                        .add_constant(Constant::String("__assert_shape".into()));
                    self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                    let var_idx = self
                        .chunk
                        .add_constant(Constant::String(param.name.clone()));
                    self.chunk.emit_u16(Op::GetVar, var_idx, self.line);
                    let name_idx = self
                        .chunk
                        .add_constant(Constant::String(param.name.clone()));
                    self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                    let spec_idx = self.chunk.add_constant(Constant::String(spec));
                    self.chunk.emit_u16(Op::Constant, spec_idx, self.line);
                    self.chunk.emit_u8(Op::Call, 3, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    continue;
                }

                // Check if this is an interface type — emit __assert_interface
                if let harn_parser::TypeExpr::Named(name) = type_expr {
                    if let Some(methods) = self.interface_methods.get(name) {
                        let fn_idx = self
                            .chunk
                            .add_constant(Constant::String("__assert_interface".into()));
                        self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                        let var_idx = self
                            .chunk
                            .add_constant(Constant::String(param.name.clone()));
                        self.chunk.emit_u16(Op::GetVar, var_idx, self.line);
                        let name_idx = self
                            .chunk
                            .add_constant(Constant::String(param.name.clone()));
                        self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                        let iface_idx = self.chunk.add_constant(Constant::String(name.clone()));
                        self.chunk.emit_u16(Op::Constant, iface_idx, self.line);
                        let methods_str = methods.join(",");
                        let methods_idx = self.chunk.add_constant(Constant::String(methods_str));
                        self.chunk.emit_u16(Op::Constant, methods_idx, self.line);
                        self.chunk.emit_u8(Op::Call, 4, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        continue;
                    }
                }

                let type_name = Self::type_expr_to_runtime_name(type_expr);
                if let Some(type_name) = type_name {
                    let var_idx = self
                        .chunk
                        .add_constant(Constant::String(param.name.clone()));
                    let type_idx = self.chunk.add_constant(Constant::String(type_name));
                    self.chunk.emit_u16(Op::CheckType, var_idx, self.line);
                    // Emit the type name index as two extra bytes
                    let hi = (type_idx >> 8) as u8;
                    let lo = type_idx as u8;
                    self.chunk.code.push(hi);
                    self.chunk.code.push(lo);
                }
            }
        }
    }

    /// Serialize a list of ShapeFields into a spec string for __assert_shape.
    /// Format: `name:string,age:int,active:?bool,addr:{city:string,zip:string}`
    fn shape_to_spec_string(fields: &[harn_parser::ShapeField]) -> String {
        fields
            .iter()
            .map(|f| {
                let opt = if f.optional { "?" } else { "" };
                let type_str = Self::type_expr_to_spec(&f.type_expr);
                format!("{}:{}{}", f.name, opt, type_str)
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Convert a TypeExpr into a spec string fragment for shape validation.
    fn type_expr_to_spec(type_expr: &harn_parser::TypeExpr) -> String {
        match type_expr {
            harn_parser::TypeExpr::Named(name) => name.clone(),
            harn_parser::TypeExpr::Shape(fields) => {
                let inner = Self::shape_to_spec_string(fields);
                format!("{{{}}}", inner)
            }
            harn_parser::TypeExpr::List(_) => "list".to_string(),
            harn_parser::TypeExpr::DictType(_, _) => "dict".to_string(),
            harn_parser::TypeExpr::Union(members) => {
                // Serialize union as "type1|type2|type3" for runtime validation
                members
                    .iter()
                    .map(Self::type_expr_to_spec)
                    .collect::<Vec<_>>()
                    .join("|")
            }
            harn_parser::TypeExpr::FnType { .. } => "closure".to_string(),
        }
    }

    /// Convert a TypeExpr to a runtime type name string for CheckType.
    fn type_expr_to_runtime_name(type_expr: &harn_parser::TypeExpr) -> Option<String> {
        match type_expr {
            harn_parser::TypeExpr::Named(name) => match name.as_str() {
                "int" | "float" | "string" | "bool" | "list" | "dict" | "set" | "nil"
                | "closure" => Some(name.clone()),
                _ => None, // Unknown types are not checked at runtime
            },
            _ => None, // Union types, shapes, etc. are not checked at runtime
        }
    }

    /// Emit the extra u16 type name index after a TryCatchSetup jump.
    fn emit_type_name_extra(&mut self, type_name_idx: u16) {
        let hi = (type_name_idx >> 8) as u8;
        let lo = type_name_idx as u8;
        self.chunk.code.push(hi);
        self.chunk.code.push(lo);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
    }

    /// Compile a try/catch body block (produces a value on the stack).
    fn compile_try_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        if body.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_scoped_block(body)?;
        }
        Ok(())
    }

    /// Compile catch error binding (error value is on stack from handler).
    fn compile_catch_binding(&mut self, error_var: &Option<String>) -> Result<(), CompileError> {
        if let Some(var_name) = error_var {
            let idx = self.chunk.add_constant(Constant::String(var_name.clone()));
            self.chunk.emit_u16(Op::DefLet, idx, self.line);
        } else {
            self.chunk.emit(Op::Pop, self.line);
        }
        Ok(())
    }

    /// Compile finally body inline, discarding its result value.
    fn compile_finally_inline(&mut self, finally_body: &[SNode]) -> Result<(), CompileError> {
        if !finally_body.is_empty() {
            self.compile_scoped_block(finally_body)?;
            // Finally body's value is discarded — only the try/catch value matters
            if Self::produces_value(&finally_body.last().unwrap().node) {
                self.chunk.emit(Op::Pop, self.line);
            }
        }
        Ok(())
    }

    /// Compile rethrow pattern: save error to temp var, run finally, re-throw.
    fn compile_rethrow_with_finally(&mut self, finally_body: &[SNode]) -> Result<(), CompileError> {
        // Error is on the stack from the handler
        self.temp_counter += 1;
        let temp_name = format!("__finally_err_{}__", self.temp_counter);
        let err_idx = self.chunk.add_constant(Constant::String(temp_name.clone()));
        self.chunk.emit_u16(Op::DefVar, err_idx, self.line);
        self.compile_finally_inline(finally_body)?;
        let get_idx = self.chunk.add_constant(Constant::String(temp_name));
        self.chunk.emit_u16(Op::GetVar, get_idx, self.line);
        self.chunk.emit(Op::Throw, self.line);
        Ok(())
    }

    fn begin_scope(&mut self) {
        self.chunk.emit(Op::PushScope, self.line);
        self.scope_depth += 1;
    }

    fn end_scope(&mut self) {
        if self.scope_depth > 0 {
            self.chunk.emit(Op::PopScope, self.line);
            self.scope_depth -= 1;
        }
    }

    fn unwind_scopes_to(&mut self, target_depth: usize) {
        while self.scope_depth > target_depth {
            self.chunk.emit(Op::PopScope, self.line);
            self.scope_depth -= 1;
        }
    }

    fn compile_scoped_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        if stmts.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(stmts)?;
        }
        self.end_scope();
        Ok(())
    }

    fn compile_scoped_statements(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        for sn in stmts {
            self.compile_node(sn)?;
            if Self::produces_value(&sn.node) {
                self.chunk.emit(Op::Pop, self.line);
            }
        }
        self.end_scope();
        Ok(())
    }

    fn compile_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        for (i, snode) in stmts.iter().enumerate() {
            self.compile_node(snode)?;
            let is_last = i == stmts.len() - 1;
            if is_last {
                // If the last statement doesn't produce a value, push nil
                // so the block always leaves exactly one value on the stack.
                if !Self::produces_value(&snode.node) {
                    self.chunk.emit(Op::Nil, self.line);
                }
            } else {
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
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::GetVar, idx, self.line);
            }

            Node::LetBinding { pattern, value, .. } => {
                self.compile_node(value)?;
                self.compile_destructuring(pattern, false)?;
            }

            Node::VarBinding { pattern, value, .. } => {
                self.compile_node(value)?;
                self.compile_destructuring(pattern, true)?;
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
                        // If the RHS contains `_` placeholders, desugar into a closure:
                        //   value |> func(_, arg)  =>  value |> { __pipe -> func(__pipe, arg) }
                        if contains_pipe_placeholder(right) {
                            let replaced = replace_pipe_placeholder(right);
                            let closure_node = SNode::dummy(Node::Closure {
                                params: vec![TypedParam {
                                    name: "__pipe".into(),
                                    type_expr: None,
                                    default_value: None,
                                    rest: false,
                                }],
                                body: vec![replaced],
                                fn_syntax: false,
                            });
                            self.compile_node(&closure_node)?;
                        } else {
                            self.compile_node(right)?;
                        }
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
                    "in" => self.chunk.emit(Op::Contains, self.line),
                    "not_in" => {
                        self.chunk.emit(Op::Contains, self.line);
                        self.chunk.emit(Op::Not, self.line);
                    }
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
                let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
                // Push function name as string constant
                let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::Constant, name_idx, self.line);

                if has_spread {
                    // Build the args into a single list using the flush-and-concat
                    // pattern (same as ListLiteral with spreads).
                    self.chunk.emit_u16(Op::BuildList, 0, self.line);
                    let mut pending = 0u16;
                    for arg in args {
                        if let Node::Spread(inner) = &arg.node {
                            if pending > 0 {
                                self.chunk.emit_u16(Op::BuildList, pending, self.line);
                                self.chunk.emit(Op::Add, self.line);
                                pending = 0;
                            }
                            self.compile_node(inner)?;
                            self.chunk.emit(Op::Dup, self.line);
                            let assert_idx = self
                                .chunk
                                .add_constant(Constant::String("__assert_list".into()));
                            self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            self.chunk.emit(Op::Pop, self.line);
                            self.chunk.emit(Op::Add, self.line);
                        } else {
                            self.compile_node(arg)?;
                            pending += 1;
                        }
                    }
                    if pending > 0 {
                        self.chunk.emit_u16(Op::BuildList, pending, self.line);
                        self.chunk.emit(Op::Add, self.line);
                    }
                    self.chunk.emit(Op::CallSpread, self.line);
                } else {
                    // Push arguments normally
                    for arg in args {
                        self.compile_node(arg)?;
                    }
                    self.chunk.emit_u8(Op::Call, args.len() as u8, self.line);
                }
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
                let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
                self.compile_node(object)?;
                let name_idx = self.chunk.add_constant(Constant::String(method.clone()));
                if has_spread {
                    // Build args into a single list (same pattern as FunctionCall spread)
                    self.chunk.emit_u16(Op::BuildList, 0, self.line);
                    let mut pending = 0u16;
                    for arg in args {
                        if let Node::Spread(inner) = &arg.node {
                            if pending > 0 {
                                self.chunk.emit_u16(Op::BuildList, pending, self.line);
                                self.chunk.emit(Op::Add, self.line);
                                pending = 0;
                            }
                            self.compile_node(inner)?;
                            self.chunk.emit(Op::Dup, self.line);
                            let assert_idx = self
                                .chunk
                                .add_constant(Constant::String("__assert_list".into()));
                            self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            self.chunk.emit(Op::Pop, self.line);
                            self.chunk.emit(Op::Add, self.line);
                        } else {
                            self.compile_node(arg)?;
                            pending += 1;
                        }
                    }
                    if pending > 0 {
                        self.chunk.emit_u16(Op::BuildList, pending, self.line);
                        self.chunk.emit(Op::Add, self.line);
                    }
                    self.chunk
                        .emit_u16(Op::MethodCallSpread, name_idx, self.line);
                } else {
                    for arg in args {
                        self.compile_node(arg)?;
                    }
                    self.chunk
                        .emit_method_call(name_idx, args.len() as u8, self.line);
                }
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
                self.compile_node(condition)?;
                let else_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_scoped_block(then_body)?;
                if let Some(else_body) = else_body {
                    let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                    self.chunk.patch_jump(else_jump);
                    self.chunk.emit(Op::Pop, self.line);
                    self.compile_scoped_block(else_body)?;
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
                    finally_depth: self.finally_bodies.len(),
                    scope_depth: self.scope_depth,
                });
                self.compile_node(condition)?;
                let exit_jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line); // pop condition
                self.compile_scoped_statements(body)?;
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
                pattern,
                iterable,
                body,
            } => {
                // Compile iterable
                self.compile_node(iterable)?;
                // Initialize iterator
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
                // Try to get next item — jumps to end if exhausted
                let exit_jump_pos = self.chunk.emit_jump(Op::IterNext, self.line);
                self.begin_scope();
                // Define loop variable(s) with current item (item is on stack from IterNext)
                self.compile_destructuring(pattern, true)?;
                for sn in body {
                    self.compile_node(sn)?;
                    if Self::produces_value(&sn.node) {
                        self.chunk.emit(Op::Pop, self.line);
                    }
                }
                self.end_scope();
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
                let has_pending_finally = !self.finally_bodies.is_empty();

                if has_pending_finally {
                    // Inside try-finally: compile value, save to temp,
                    // run pending finallys, restore value, then return.
                    if let Some(val) = value {
                        self.compile_node(val)?;
                    } else {
                        self.chunk.emit(Op::Nil, self.line);
                    }
                    self.temp_counter += 1;
                    let temp_name = format!("__return_val_{}__", self.temp_counter);
                    let save_idx = self.chunk.add_constant(Constant::String(temp_name.clone()));
                    self.chunk.emit_u16(Op::DefVar, save_idx, self.line);
                    // Emit all pending finallys (innermost first = reverse order)
                    let finallys: Vec<_> = self.finally_bodies.iter().rev().cloned().collect();
                    for fb in &finallys {
                        self.compile_finally_inline(fb)?;
                    }
                    let restore_idx = self.chunk.add_constant(Constant::String(temp_name));
                    self.chunk.emit_u16(Op::GetVar, restore_idx, self.line);
                    self.chunk.emit(Op::Return, self.line);
                } else {
                    // No pending finally — original behavior with tail call optimization
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
            }

            Node::BreakStmt => {
                if self.loop_stack.is_empty() {
                    return Err(CompileError {
                        message: "break outside of loop".to_string(),
                        line: self.line,
                    });
                }
                // Copy values out to avoid borrow conflict
                let ctx = self.loop_stack.last().unwrap();
                let finally_depth = ctx.finally_depth;
                let handler_depth = ctx.handler_depth;
                let has_iterator = ctx.has_iterator;
                let scope_depth = ctx.scope_depth;
                // Pop exception handlers that were pushed inside the loop
                for _ in handler_depth..self.handler_depth {
                    self.chunk.emit(Op::PopHandler, self.line);
                }
                // Emit pending finallys that are inside the loop
                if self.finally_bodies.len() > finally_depth {
                    let finallys: Vec<_> = self.finally_bodies[finally_depth..]
                        .iter()
                        .rev()
                        .cloned()
                        .collect();
                    for fb in &finallys {
                        self.compile_finally_inline(fb)?;
                    }
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
            }

            Node::ContinueStmt => {
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
                if self.finally_bodies.len() > finally_depth {
                    let finallys: Vec<_> = self.finally_bodies[finally_depth..]
                        .iter()
                        .rev()
                        .cloned()
                        .collect();
                    for fb in &finallys {
                        self.compile_finally_inline(fb)?;
                    }
                }
                self.unwind_scopes_to(scope_depth);
                self.chunk.emit_u16(Op::Jump, loop_start as u16, self.line);
            }

            Node::ListLiteral(elements) => {
                let has_spread = elements.iter().any(|e| matches!(&e.node, Node::Spread(_)));
                if !has_spread {
                    for el in elements {
                        self.compile_node(el)?;
                    }
                    self.chunk
                        .emit_u16(Op::BuildList, elements.len() as u16, self.line);
                } else {
                    // Build with spreads: accumulate segments into lists and concat
                    // Start with empty list
                    self.chunk.emit_u16(Op::BuildList, 0, self.line);
                    let mut pending = 0u16;
                    for el in elements {
                        if let Node::Spread(inner) = &el.node {
                            // First, build list from pending non-spread elements
                            if pending > 0 {
                                self.chunk.emit_u16(Op::BuildList, pending, self.line);
                                // Concat accumulated + pending segment
                                self.chunk.emit(Op::Add, self.line);
                                pending = 0;
                            }
                            // Concat with the spread expression (with type check)
                            self.compile_node(inner)?;
                            self.chunk.emit(Op::Dup, self.line);
                            let assert_idx = self
                                .chunk
                                .add_constant(Constant::String("__assert_list".into()));
                            self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            self.chunk.emit(Op::Pop, self.line);
                            self.chunk.emit(Op::Add, self.line);
                        } else {
                            self.compile_node(el)?;
                            pending += 1;
                        }
                    }
                    if pending > 0 {
                        self.chunk.emit_u16(Op::BuildList, pending, self.line);
                        self.chunk.emit(Op::Add, self.line);
                    }
                }
            }

            Node::DictLiteral(entries) => {
                let has_spread = entries
                    .iter()
                    .any(|e| matches!(&e.value.node, Node::Spread(_)));
                if !has_spread {
                    for entry in entries {
                        self.compile_node(&entry.key)?;
                        self.compile_node(&entry.value)?;
                    }
                    self.chunk
                        .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
                } else {
                    // Build with spreads: use empty dict + Add for merging
                    self.chunk.emit_u16(Op::BuildDict, 0, self.line);
                    let mut pending = 0u16;
                    for entry in entries {
                        if let Node::Spread(inner) = &entry.value.node {
                            // Flush pending entries
                            if pending > 0 {
                                self.chunk.emit_u16(Op::BuildDict, pending, self.line);
                                self.chunk.emit(Op::Add, self.line);
                                pending = 0;
                            }
                            // Merge spread dict via Add (with type check)
                            self.compile_node(inner)?;
                            self.chunk.emit(Op::Dup, self.line);
                            let assert_idx = self
                                .chunk
                                .add_constant(Constant::String("__assert_dict".into()));
                            self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            self.chunk.emit(Op::Pop, self.line);
                            self.chunk.emit(Op::Add, self.line);
                        } else {
                            self.compile_node(&entry.key)?;
                            self.compile_node(&entry.value)?;
                            pending += 1;
                        }
                    }
                    if pending > 0 {
                        self.chunk.emit_u16(Op::BuildDict, pending, self.line);
                        self.chunk.emit(Op::Add, self.line);
                    }
                }
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
                        StringSegment::Expression(expr_str, expr_line, expr_col) => {
                            // Parse and compile the embedded expression
                            let mut lexer =
                                harn_lexer::Lexer::with_position(expr_str, *expr_line, *expr_col);
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
                fn_compiler.emit_default_preamble(params)?;
                fn_compiler.emit_type_checks(params);
                let is_gen = body_contains_yield(body);
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Nil, self.line);
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: name.clone(),
                    params: TypedParam::names(params),
                    default_start: TypedParam::default_start(params),
                    chunk: fn_compiler.chunk,
                    is_generator: is_gen,
                    has_rest_param: params.last().is_some_and(|p| p.rest),
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);

                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
            }

            Node::ToolDecl {
                name,
                description,
                params,
                return_type: _,
                body,
                ..
            } => {
                // 1. Compile the body as a closure (same as FnDecl)
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.emit_default_preamble(params)?;
                fn_compiler.emit_type_checks(params);
                fn_compiler.compile_block(body)?;
                // Use closure-like return: the last expression value is the return value
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: name.clone(),
                    params: TypedParam::names(params),
                    default_start: TypedParam::default_start(params),
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: params.last().is_some_and(|p| p.rest),
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);

                // 2. Push "tool_define" function name (Call convention: fn first, then args)
                let define_name = self
                    .chunk
                    .add_constant(Constant::String("tool_define".into()));
                self.chunk.emit_u16(Op::Constant, define_name, self.line);

                // 3. Arg 1: tool_registry()
                let reg_name = self
                    .chunk
                    .add_constant(Constant::String("tool_registry".into()));
                self.chunk.emit_u16(Op::Constant, reg_name, self.line);
                self.chunk.emit_u8(Op::Call, 0, self.line);

                // 4. Arg 2: tool name string
                let tool_name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::Constant, tool_name_idx, self.line);

                // 5. Arg 3: description string
                let desc = description.as_deref().unwrap_or("");
                let desc_idx = self.chunk.add_constant(Constant::String(desc.to_string()));
                self.chunk.emit_u16(Op::Constant, desc_idx, self.line);

                // 6. Arg 4: config dict { parameters: {...}, handler: <closure> }
                // Build parameters dict: { param_name: { type: "<json_type>" }, ... }
                let mut param_count: u16 = 0;
                for p in params {
                    let pn_idx = self.chunk.add_constant(Constant::String(p.name.clone()));
                    self.chunk.emit_u16(Op::Constant, pn_idx, self.line);

                    let type_str = match &p.type_expr {
                        Some(harn_parser::TypeExpr::Named(n)) => match n.as_str() {
                            "string" => "string",
                            "int" => "integer",
                            "float" => "number",
                            "bool" => "boolean",
                            _ => "string",
                        },
                        _ => "string",
                    };
                    let type_key = self.chunk.add_constant(Constant::String("type".into()));
                    self.chunk.emit_u16(Op::Constant, type_key, self.line);
                    let type_val = self.chunk.add_constant(Constant::String(type_str.into()));
                    self.chunk.emit_u16(Op::Constant, type_val, self.line);
                    let mut param_schema_entries: u16 = 1;
                    if let Some(default_expr) = &p.default_value {
                        let required_key =
                            self.chunk.add_constant(Constant::String("required".into()));
                        self.chunk.emit_u16(Op::Constant, required_key, self.line);
                        let required_val = self.chunk.add_constant(Constant::Bool(false));
                        self.chunk.emit_u16(Op::Constant, required_val, self.line);
                        let default_key =
                            self.chunk.add_constant(Constant::String("default".into()));
                        self.chunk.emit_u16(Op::Constant, default_key, self.line);
                        self.compile_node(default_expr)?;
                        param_schema_entries += 2;
                    }
                    self.chunk
                        .emit_u16(Op::BuildDict, param_schema_entries, self.line);
                    param_count += 1;
                }
                self.chunk.emit_u16(Op::BuildDict, param_count, self.line);

                // Build config dict: { "parameters": <params_dict>, "handler": <closure> }
                let params_key = self
                    .chunk
                    .add_constant(Constant::String("parameters".into()));
                self.chunk.emit_u16(Op::Constant, params_key, self.line);
                self.chunk.emit(Op::Swap, self.line);

                let handler_key = self.chunk.add_constant(Constant::String("handler".into()));
                self.chunk.emit_u16(Op::Constant, handler_key, self.line);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);

                self.chunk.emit_u16(Op::BuildDict, 2, self.line);

                // 7. Call tool_define(registry, name, description, config)
                self.chunk.emit_u8(Op::Call, 4, self.line);

                // 8. Bind the result (the registry with the tool) as a let variable
                let bind_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefLet, bind_idx, self.line);
            }

            Node::Closure { params, body, .. } => {
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.emit_default_preamble(params)?;
                fn_compiler.emit_type_checks(params);
                let is_gen = body_contains_yield(body);
                fn_compiler.compile_block(body)?;
                // If block didn't end with return, the last value is on the stack
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: "<closure>".to_string(),
                    params: TypedParam::names(params),
                    default_start: TypedParam::default_start(params),
                    chunk: fn_compiler.chunk,
                    is_generator: is_gen,
                    has_rest_param: false,
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
                            self.begin_scope();
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
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
                            self.begin_scope();

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
                            self.end_scope();
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
                            self.begin_scope();
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
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
                            self.begin_scope();

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
                            self.end_scope();
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                            self.chunk.patch_jump(skip);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                        }
                        // Binding pattern: bare identifier (not a literal)
                        Node::Identifier(name) => {
                            self.begin_scope();
                            // Bind the match value to this name, always matches
                            self.chunk.emit(Op::Dup, self.line); // dup for binding
                            let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                            self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        }
                        // Dict pattern: {key: literal, key: binding, ...}
                        Node::DictLiteral(entries)
                            if entries
                                .iter()
                                .all(|e| matches!(&e.key.node, Node::StringLiteral(_))) =>
                        {
                            // Check type is dict: dup, call type_of, compare "dict"
                            self.chunk.emit(Op::Dup, self.line);
                            let typeof_idx =
                                self.chunk.add_constant(Constant::String("type_of".into()));
                            self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            let dict_str = self.chunk.add_constant(Constant::String("dict".into()));
                            self.chunk.emit_u16(Op::Constant, dict_str, self.line);
                            self.chunk.emit(Op::Equal, self.line);
                            let skip_type = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool

                            // Check literal constraints
                            let mut constraint_skips = Vec::new();
                            let mut bindings = Vec::new();
                            self.begin_scope();
                            for entry in entries {
                                if let Node::StringLiteral(key) = &entry.key.node {
                                    match &entry.value.node {
                                        // Literal value → constraint: dict[key] == value
                                        Node::StringLiteral(_)
                                        | Node::IntLiteral(_)
                                        | Node::FloatLiteral(_)
                                        | Node::BoolLiteral(_)
                                        | Node::NilLiteral => {
                                            self.chunk.emit(Op::Dup, self.line);
                                            let key_idx = self
                                                .chunk
                                                .add_constant(Constant::String(key.clone()));
                                            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                                            self.chunk.emit(Op::Subscript, self.line);
                                            self.compile_node(&entry.value)?;
                                            self.chunk.emit(Op::Equal, self.line);
                                            let skip =
                                                self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                            self.chunk.emit(Op::Pop, self.line); // pop bool
                                            constraint_skips.push(skip);
                                        }
                                        // Identifier → binding: bind dict[key] to variable
                                        Node::Identifier(binding) => {
                                            bindings.push((key.clone(), binding.clone()));
                                        }
                                        _ => {
                                            // Complex expression constraint
                                            self.chunk.emit(Op::Dup, self.line);
                                            let key_idx = self
                                                .chunk
                                                .add_constant(Constant::String(key.clone()));
                                            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                                            self.chunk.emit(Op::Subscript, self.line);
                                            self.compile_node(&entry.value)?;
                                            self.chunk.emit(Op::Equal, self.line);
                                            let skip =
                                                self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                            self.chunk.emit(Op::Pop, self.line);
                                            constraint_skips.push(skip);
                                        }
                                    }
                                }
                            }

                            // All constraints passed — emit bindings
                            for (key, binding) in &bindings {
                                self.chunk.emit(Op::Dup, self.line);
                                let key_idx =
                                    self.chunk.add_constant(Constant::String(key.clone()));
                                self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                                self.chunk.emit(Op::Subscript, self.line);
                                let name_idx =
                                    self.chunk.add_constant(Constant::String(binding.clone()));
                                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                            }

                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));

                            let type_fail_target = self.chunk.code.len();
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            let next_arm_jump = self.chunk.emit_jump(Op::Jump, self.line);
                            let scoped_fail_target = self.chunk.code.len();
                            self.chunk.emit(Op::PopScope, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            let next_arm_target = self.chunk.code.len();

                            for skip in constraint_skips {
                                self.chunk.patch_jump_to(skip, scoped_fail_target);
                            }
                            self.chunk.patch_jump_to(skip_type, type_fail_target);
                            self.chunk.patch_jump_to(next_arm_jump, next_arm_target);
                        }
                        // List pattern: [literal, binding, ...]
                        Node::ListLiteral(elements) => {
                            // Check type is list: dup, call type_of, compare "list"
                            self.chunk.emit(Op::Dup, self.line);
                            let typeof_idx =
                                self.chunk.add_constant(Constant::String("type_of".into()));
                            self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            let list_str = self.chunk.add_constant(Constant::String("list".into()));
                            self.chunk.emit_u16(Op::Constant, list_str, self.line);
                            self.chunk.emit(Op::Equal, self.line);
                            let skip_type = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool

                            // Check length: dup, call len, compare >= elements.len()
                            self.chunk.emit(Op::Dup, self.line);
                            let len_idx = self.chunk.add_constant(Constant::String("len".into()));
                            self.chunk.emit_u16(Op::Constant, len_idx, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            let count = self
                                .chunk
                                .add_constant(Constant::Int(elements.len() as i64));
                            self.chunk.emit_u16(Op::Constant, count, self.line);
                            self.chunk.emit(Op::GreaterEqual, self.line);
                            let skip_len = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool

                            // Check literal constraints and collect bindings
                            let mut constraint_skips = Vec::new();
                            let mut bindings = Vec::new();
                            self.begin_scope();
                            for (i, elem) in elements.iter().enumerate() {
                                match &elem.node {
                                    Node::Identifier(name) if name != "_" => {
                                        bindings.push((i, name.clone()));
                                    }
                                    Node::Identifier(_) => {} // wildcard _
                                    // Literal constraint
                                    _ => {
                                        self.chunk.emit(Op::Dup, self.line);
                                        let idx_const =
                                            self.chunk.add_constant(Constant::Int(i as i64));
                                        self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                                        self.chunk.emit(Op::Subscript, self.line);
                                        self.compile_node(elem)?;
                                        self.chunk.emit(Op::Equal, self.line);
                                        let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                        self.chunk.emit(Op::Pop, self.line);
                                        constraint_skips.push(skip);
                                    }
                                }
                            }

                            // Emit bindings
                            for (i, name) in &bindings {
                                self.chunk.emit(Op::Dup, self.line);
                                let idx_const = self.chunk.add_constant(Constant::Int(*i as i64));
                                self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                                self.chunk.emit(Op::Subscript, self.line);
                                let name_idx =
                                    self.chunk.add_constant(Constant::String(name.clone()));
                                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                            }

                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
                            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));

                            let pre_scope_fail_target = self.chunk.code.len();
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            let next_arm_jump = self.chunk.emit_jump(Op::Jump, self.line);
                            let scoped_fail_target = self.chunk.code.len();
                            self.chunk.emit(Op::PopScope, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            let next_arm_target = self.chunk.code.len();
                            for skip in constraint_skips {
                                self.chunk.patch_jump_to(skip, scoped_fail_target);
                            }
                            self.chunk.patch_jump_to(skip_len, pre_scope_fail_target);
                            self.chunk.patch_jump_to(skip_type, pre_scope_fail_target);
                            self.chunk.patch_jump_to(next_arm_jump, next_arm_target);
                        }
                        // Literal/expression pattern — compare with Equal
                        _ => {
                            self.chunk.emit(Op::Dup, self.line);
                            self.compile_node(&arm.pattern)?;
                            self.chunk.emit(Op::Equal, self.line);
                            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                            self.chunk.emit(Op::Pop, self.line); // pop bool
                            self.begin_scope();
                            self.chunk.emit(Op::Pop, self.line); // pop match value
                            self.compile_match_body(&arm.body)?;
                            self.end_scope();
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
                self.compile_scoped_block(else_body)?;
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

            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    self.compile_node(val)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }
                self.chunk.emit(Op::Yield, self.line);
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

            Node::TryOperator { operand } => {
                self.compile_node(operand)?;
                self.chunk.emit(Op::TryUnwrap, self.line);
            }

            Node::ImplBlock { type_name, methods } => {
                // Compile each method as a closure and store in __impl_TypeName dict.
                // Build key-value pairs on stack, then BuildDict.
                for method_sn in methods {
                    if let Node::FnDecl {
                        name, params, body, ..
                    } = &method_sn.node
                    {
                        // Method name key
                        let key_idx = self.chunk.add_constant(Constant::String(name.clone()));
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);

                        // Compile method body as closure
                        let mut fn_compiler = Compiler::new();
                        fn_compiler.enum_names = self.enum_names.clone();
                        fn_compiler.emit_default_preamble(params)?;
                        fn_compiler.emit_type_checks(params);
                        fn_compiler.compile_block(body)?;
                        fn_compiler.chunk.emit(Op::Nil, self.line);
                        fn_compiler.chunk.emit(Op::Return, self.line);

                        let func = CompiledFunction {
                            name: format!("{}.{}", type_name, name),
                            params: TypedParam::names(params),
                            default_start: TypedParam::default_start(params),
                            chunk: fn_compiler.chunk,
                            is_generator: false,
                            has_rest_param: false,
                        };
                        let fn_idx = self.chunk.functions.len();
                        self.chunk.functions.push(func);
                        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                    }
                }
                let method_count = methods
                    .iter()
                    .filter(|m| matches!(m.node, Node::FnDecl { .. }))
                    .count();
                self.chunk
                    .emit_u16(Op::BuildDict, method_count as u16, self.line);
                let impl_name = format!("__impl_{}", type_name);
                let name_idx = self.chunk.add_constant(Constant::String(impl_name));
                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
            }

            Node::StructDecl { name, .. } => {
                // Compile a constructor function: StructName({field: val, ...}) -> StructInstance
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                let params = vec![TypedParam::untyped("__fields")];
                fn_compiler.emit_default_preamble(&params)?;

                // Call __make_struct(struct_name, fields_dict) to tag the dict
                let make_idx = fn_compiler
                    .chunk
                    .add_constant(Constant::String("__make_struct".into()));
                fn_compiler
                    .chunk
                    .emit_u16(Op::Constant, make_idx, self.line);
                let sname_idx = fn_compiler
                    .chunk
                    .add_constant(Constant::String(name.clone()));
                fn_compiler
                    .chunk
                    .emit_u16(Op::Constant, sname_idx, self.line);
                let fields_idx = fn_compiler
                    .chunk
                    .add_constant(Constant::String("__fields".into()));
                fn_compiler
                    .chunk
                    .emit_u16(Op::GetVar, fields_idx, self.line);
                fn_compiler.chunk.emit_u8(Op::Call, 2, self.line);
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: name.clone(),
                    params: TypedParam::names(&params),
                    default_start: None,
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: false,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
            }

            // Declarations that only register metadata (no runtime effect needed for v1)
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
                    // === try-catch-finally ===
                    let finally_body = finally_body.as_ref().unwrap();

                    // Push finally body onto pending stack for return/break handling
                    self.finally_bodies.push(finally_body.clone());

                    // 1. TryCatchSetup for try body
                    self.handler_depth += 1;
                    let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                    self.emit_type_name_extra(type_name_idx);

                    // 2. Compile try body
                    self.compile_try_body(body)?;

                    // 3. PopHandler + inline finally (success path)
                    self.handler_depth -= 1;
                    self.chunk.emit(Op::PopHandler, self.line);
                    self.compile_finally_inline(finally_body)?;
                    let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                    // 4. Catch entry
                    self.chunk.patch_jump(catch_jump);
                    self.begin_scope();
                    self.compile_catch_binding(error_var)?;

                    // 5. Inner try around catch body (so finally runs if catch throws)
                    self.handler_depth += 1;
                    let rethrow_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                    let empty_type = self.chunk.add_constant(Constant::String(String::new()));
                    self.emit_type_name_extra(empty_type);

                    // 6. Compile catch body
                    self.compile_try_body(catch_body)?;

                    // 7. PopHandler + inline finally (catch success path)
                    self.handler_depth -= 1;
                    self.chunk.emit(Op::PopHandler, self.line);
                    self.compile_finally_inline(finally_body)?;
                    self.end_scope();
                    let end_jump2 = self.chunk.emit_jump(Op::Jump, self.line);

                    // 8. Rethrow handler: save error, run finally, re-throw
                    self.chunk.patch_jump(rethrow_jump);
                    self.compile_rethrow_with_finally(finally_body)?;
                    self.end_scope();

                    self.chunk.patch_jump(end_jump);
                    self.chunk.patch_jump(end_jump2);

                    self.finally_bodies.pop();
                } else if has_finally {
                    // === try-finally (no catch) ===
                    let finally_body = finally_body.as_ref().unwrap();

                    self.finally_bodies.push(finally_body.clone());

                    // 1. TryCatchSetup to error path
                    self.handler_depth += 1;
                    let error_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                    let empty_type = self.chunk.add_constant(Constant::String(String::new()));
                    self.emit_type_name_extra(empty_type);

                    // 2. Compile try body
                    self.compile_try_body(body)?;

                    // 3. PopHandler + inline finally (success path)
                    self.handler_depth -= 1;
                    self.chunk.emit(Op::PopHandler, self.line);
                    self.compile_finally_inline(finally_body)?;
                    let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                    // 4. Error path: save error, run finally, re-throw
                    self.chunk.patch_jump(error_jump);
                    self.compile_rethrow_with_finally(finally_body)?;

                    self.chunk.patch_jump(end_jump);

                    self.finally_bodies.pop();
                } else {
                    // === try-catch (no finally) — original behavior ===

                    // 1. TryCatchSetup
                    self.handler_depth += 1;
                    let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                    self.emit_type_name_extra(type_name_idx);

                    // 2. Compile try body
                    self.compile_try_body(body)?;

                    // 3. PopHandler + jump past catch
                    self.handler_depth -= 1;
                    self.chunk.emit(Op::PopHandler, self.line);
                    let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                    // 4. Catch entry
                    self.chunk.patch_jump(catch_jump);
                    self.begin_scope();
                    self.compile_catch_binding(error_var)?;

                    // 5. Compile catch body
                    self.compile_try_body(catch_body)?;
                    self.end_scope();

                    // 6. Patch end
                    self.chunk.patch_jump(end_jump);
                }
            }

            Node::TryExpr { body } => {
                // try { body } — returns Result.Ok(value) or Result.Err(error)

                // 1. Set up try-catch handler (untyped)
                self.handler_depth += 1;
                let catch_jump = self.chunk.emit_jump(Op::TryCatchSetup, self.line);
                let empty_type = self.chunk.add_constant(Constant::String(String::new()));
                self.emit_type_name_extra(empty_type);

                // 2. Compile try body (leaves value on stack)
                self.compile_try_body(body)?;

                // 3. PopHandler (success path)
                self.handler_depth -= 1;
                self.chunk.emit(Op::PopHandler, self.line);

                // 4. Wrap in Result.Ok: push "Ok", swap, call Ok(value)
                let ok_idx = self.chunk.add_constant(Constant::String("Ok".to_string()));
                self.chunk.emit_u16(Op::Constant, ok_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);

                // 5. Jump past error handler
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);

                // 6. Error handler: error value is on stack
                self.chunk.patch_jump(catch_jump);

                // 7. Wrap in Result.Err: push "Err", swap, call Err(error)
                let err_idx = self.chunk.add_constant(Constant::String("Err".to_string()));
                self.chunk.emit_u16(Op::Constant, err_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);

                // 8. Patch end
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
                    default_start: None,
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: false,
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
                    default_start: None,
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: false,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::ParallelMap, self.line);
            }

            Node::ParallelSettle {
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
                    name: "<parallel_settle>".to_string(),
                    params: vec![variable.clone()],
                    default_start: None,
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: false,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::ParallelSettle, self.line);
            }

            Node::SpawnExpr { body } => {
                let mut fn_compiler = Compiler::new();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Return, self.line);
                let func = CompiledFunction {
                    name: "<spawn>".to_string(),
                    params: vec![],
                    default_start: None,
                    chunk: fn_compiler.chunk,
                    is_generator: false,
                    has_rest_param: false,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(func);
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
                self.chunk.emit(Op::Spawn, self.line);
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                // Desugar select into: builtin call + index-based dispatch.
                //
                // Step 1: Push builtin name, compile channel list, optionally
                //         compile timeout duration, then Call.
                // Step 2: Store result dict in temp, dispatch on result.index.

                let builtin_name = if timeout.is_some() {
                    "__select_timeout"
                } else if default_body.is_some() {
                    "__select_try"
                } else {
                    "__select_list"
                };

                // Push builtin name (callee goes below args on stack)
                let name_idx = self
                    .chunk
                    .add_constant(Constant::String(builtin_name.into()));
                self.chunk.emit_u16(Op::Constant, name_idx, self.line);

                // Build channel list (arg 1)
                for case in cases {
                    self.compile_node(&case.channel)?;
                }
                self.chunk
                    .emit_u16(Op::BuildList, cases.len() as u16, self.line);

                // If timeout, compile duration (arg 2)
                if let Some((duration_expr, _)) = timeout {
                    self.compile_node(duration_expr)?;
                    self.chunk.emit_u8(Op::Call, 2, self.line);
                } else {
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                }

                // Store result in temp var
                self.temp_counter += 1;
                let result_name = format!("__sel_result_{}__", self.temp_counter);
                let result_idx = self
                    .chunk
                    .add_constant(Constant::String(result_name.clone()));
                self.chunk.emit_u16(Op::DefVar, result_idx, self.line);

                // Dispatch on result.index
                let mut end_jumps = Vec::new();

                for (i, case) in cases.iter().enumerate() {
                    let get_r = self
                        .chunk
                        .add_constant(Constant::String(result_name.clone()));
                    self.chunk.emit_u16(Op::GetVar, get_r, self.line);
                    let idx_prop = self.chunk.add_constant(Constant::String("index".into()));
                    self.chunk.emit_u16(Op::GetProperty, idx_prop, self.line);
                    let case_i = self.chunk.add_constant(Constant::Int(i as i64));
                    self.chunk.emit_u16(Op::Constant, case_i, self.line);
                    self.chunk.emit(Op::Equal, self.line);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    self.begin_scope();

                    // Bind variable = result.value
                    let get_r2 = self
                        .chunk
                        .add_constant(Constant::String(result_name.clone()));
                    self.chunk.emit_u16(Op::GetVar, get_r2, self.line);
                    let val_prop = self.chunk.add_constant(Constant::String("value".into()));
                    self.chunk.emit_u16(Op::GetProperty, val_prop, self.line);
                    let var_idx = self
                        .chunk
                        .add_constant(Constant::String(case.variable.clone()));
                    self.chunk.emit_u16(Op::DefLet, var_idx, self.line);

                    self.compile_try_body(&case.body)?;
                    self.end_scope();
                    end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line);
                }

                // Timeout/default fallthrough (index == -1)
                if let Some((_, ref timeout_body)) = timeout {
                    self.compile_try_body(timeout_body)?;
                } else if let Some(ref def_body) = default_body {
                    self.compile_try_body(def_body)?;
                } else {
                    self.chunk.emit(Op::Nil, self.line);
                }

                for ej in end_jumps {
                    self.chunk.patch_jump(ej);
                }
            }
            Node::Spread(_) => {
                return Err(CompileError {
                    message: "spread (...) can only be used inside list literals, dict literals, or function call arguments".into(),
                    line: self.line,
                });
            }
        }
        Ok(())
    }

    /// Compile a destructuring binding pattern.
    /// Expects the RHS value to already be on the stack.
    /// After this, the value is consumed (popped) and each binding is defined.
    fn compile_destructuring(
        &mut self,
        pattern: &BindingPattern,
        is_mutable: bool,
    ) -> Result<(), CompileError> {
        let def_op = if is_mutable { Op::DefVar } else { Op::DefLet };
        match pattern {
            BindingPattern::Identifier(name) => {
                // Simple case: just define the variable
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(def_op, idx, self.line);
            }
            BindingPattern::Dict(fields) => {
                // Stack has the dict value.
                // Emit runtime type check: __assert_dict(value)
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self
                    .chunk
                    .add_constant(Constant::String("__assert_dict".into()));
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line); // discard nil result

                // For each non-rest field: dup dict, push key string, subscript, define var.
                // For rest field: dup dict, call __dict_rest builtin.
                let non_rest: Vec<_> = fields.iter().filter(|f| !f.is_rest).collect();
                let rest_field = fields.iter().find(|f| f.is_rest);

                for field in &non_rest {
                    self.chunk.emit(Op::Dup, self.line);
                    let key_idx = self.chunk.add_constant(Constant::String(field.key.clone()));
                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    self.chunk.emit(Op::Subscript, self.line);
                    let binding_name = field.alias.as_deref().unwrap_or(&field.key);
                    let name_idx = self
                        .chunk
                        .add_constant(Constant::String(binding_name.to_string()));
                    self.chunk.emit_u16(def_op, name_idx, self.line);
                }

                if let Some(rest) = rest_field {
                    // Call the __dict_rest builtin: __dict_rest(dict, [keys_to_exclude])
                    // Push function name
                    let fn_idx = self
                        .chunk
                        .add_constant(Constant::String("__dict_rest".into()));
                    self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                    // Swap so dict is above function name: [fn, dict]
                    self.chunk.emit(Op::Swap, self.line);
                    // Build the exclusion keys list
                    for field in &non_rest {
                        let key_idx = self.chunk.add_constant(Constant::String(field.key.clone()));
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    }
                    self.chunk
                        .emit_u16(Op::BuildList, non_rest.len() as u16, self.line);
                    // Call __dict_rest(dict, keys_list) — 2 args
                    self.chunk.emit_u8(Op::Call, 2, self.line);
                    let rest_name = &rest.key;
                    let rest_idx = self.chunk.add_constant(Constant::String(rest_name.clone()));
                    self.chunk.emit_u16(def_op, rest_idx, self.line);
                } else {
                    // Pop the source dict
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
            BindingPattern::List(elements) => {
                // Stack has the list value.
                // Emit runtime type check: __assert_list(value)
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self
                    .chunk
                    .add_constant(Constant::String("__assert_list".into()));
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line); // discard nil result

                let non_rest: Vec<_> = elements.iter().filter(|e| !e.is_rest).collect();
                let rest_elem = elements.iter().find(|e| e.is_rest);

                for (i, elem) in non_rest.iter().enumerate() {
                    self.chunk.emit(Op::Dup, self.line);
                    let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                    self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                    self.chunk.emit(Op::Subscript, self.line);
                    let name_idx = self.chunk.add_constant(Constant::String(elem.name.clone()));
                    self.chunk.emit_u16(def_op, name_idx, self.line);
                }

                if let Some(rest) = rest_elem {
                    // Slice the list from index non_rest.len() to end: list[n..]
                    // Slice op takes: object, start, end on stack
                    // self.chunk.emit(Op::Dup, self.line); -- list is still on stack
                    let start_idx = self
                        .chunk
                        .add_constant(Constant::Int(non_rest.len() as i64));
                    self.chunk.emit_u16(Op::Constant, start_idx, self.line);
                    self.chunk.emit(Op::Nil, self.line); // end = nil (to end)
                    self.chunk.emit(Op::Slice, self.line);
                    let rest_name_idx =
                        self.chunk.add_constant(Constant::String(rest.name.clone()));
                    self.chunk.emit_u16(def_op, rest_name_idx, self.line);
                } else {
                    // Pop the source list
                    self.chunk.emit(Op::Pop, self.line);
                }
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
            | Node::ToolDecl { .. }
            | Node::ImplBlock { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ThrowStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::RequireStmt { .. } => false,
            // These compound nodes explicitly produce a value
            Node::TryCatch { .. }
            | Node::TryExpr { .. }
            | Node::Retry { .. }
            | Node::GuardStmt { .. }
            | Node::DeadlineBlock { .. }
            | Node::MutexBlock { .. }
            | Node::Spread(_) => true,
            // All other expressions produce values
            _ => true,
        }
    }
}

impl Compiler {
    /// Compile a function body into a CompiledFunction (for import support).
    ///
    /// This path is used when a module is imported and its top-level `fn`
    /// declarations are loaded into the importer's environment. It MUST emit
    /// the same function preamble as the in-file `Node::FnDecl` path, or
    /// imported functions will behave differently from locally-defined ones —
    /// in particular, default parameter values would never be set and typed
    /// parameters would not be runtime-checked.
    ///
    /// `source_file`, when provided, tags the resulting chunk so runtime
    /// errors can attribute frames to the imported file rather than the
    /// entry-point pipeline.
    pub fn compile_fn_body(
        &mut self,
        params: &[TypedParam],
        body: &[SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
        let mut fn_compiler = Compiler::new();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        let is_gen = body_contains_yield(body);
        fn_compiler.compile_block(body)?;
        fn_compiler.chunk.emit(Op::Nil, 0);
        fn_compiler.chunk.emit(Op::Return, 0);
        fn_compiler.chunk.source_file = source_file;
        Ok(CompiledFunction {
            name: String::new(),
            params: TypedParam::names(params),
            default_start: TypedParam::default_start(params),
            chunk: fn_compiler.chunk,
            is_generator: is_gen,
            has_rest_param: false,
        })
    }

    /// Compile a match arm body, ensuring it always pushes exactly one value.
    fn compile_match_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        if body.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(body)?;
            if !Self::produces_value(&body.last().unwrap().node) {
                self.chunk.emit(Op::Nil, self.line);
            }
        }
        self.end_scope();
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

    fn compile_top_level_declarations(&mut self, program: &[SNode]) -> Result<(), CompileError> {
        for sn in program {
            if matches!(
                &sn.node,
                Node::FnDecl { .. }
                    | Node::ToolDecl { .. }
                    | Node::ImplBlock { .. }
                    | Node::StructDecl { .. }
                    | Node::EnumDecl { .. }
                    | Node::InterfaceDecl { .. }
                    | Node::TypeDecl { .. }
            ) {
                self.compile_node(sn)?;
            }
        }
        Ok(())
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
                Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } => {
                    Self::collect_enum_names(body, names);
                }
                Node::Block(stmts) => {
                    Self::collect_enum_names(stmts, names);
                }
                _ => {}
            }
        }
    }

    fn collect_interface_methods(
        nodes: &[SNode],
        interfaces: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for sn in nodes {
            match &sn.node {
                Node::InterfaceDecl { name, methods, .. } => {
                    let method_names: Vec<String> =
                        methods.iter().map(|m| m.name.clone()).collect();
                    interfaces.insert(name.clone(), method_names);
                }
                Node::Pipeline { body, .. }
                | Node::FnDecl { body, .. }
                | Node::ToolDecl { body, .. } => {
                    Self::collect_interface_methods(body, interfaces);
                }
                Node::Block(stmts) => {
                    Self::collect_interface_methods(stmts, interfaces);
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

/// Check if a list of AST nodes contains any `yield` expression (used to detect generator functions).
fn body_contains_yield(nodes: &[SNode]) -> bool {
    nodes.iter().any(|sn| node_contains_yield(&sn.node))
}

fn node_contains_yield(node: &Node) -> bool {
    match node {
        Node::YieldExpr { .. } => true,
        // Don't recurse into nested function/closure declarations — yield in a nested
        // function does NOT make the outer function a generator.
        Node::FnDecl { .. } | Node::Closure { .. } => false,
        Node::Block(stmts) => body_contains_yield(stmts),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            node_contains_yield(&condition.node)
                || body_contains_yield(then_body)
                || else_body.as_ref().is_some_and(|b| body_contains_yield(b))
        }
        Node::WhileLoop { condition, body } => {
            node_contains_yield(&condition.node) || body_contains_yield(body)
        }
        Node::ForIn { iterable, body, .. } => {
            node_contains_yield(&iterable.node) || body_contains_yield(body)
        }
        Node::TryCatch {
            body, catch_body, ..
        } => body_contains_yield(body) || body_contains_yield(catch_body),
        Node::TryExpr { body } => body_contains_yield(body),
        _ => false,
    }
}

/// Check if an AST node contains `_` identifier (pipe placeholder).
fn contains_pipe_placeholder(node: &SNode) -> bool {
    match &node.node {
        Node::Identifier(name) if name == "_" => true,
        Node::FunctionCall { args, .. } => args.iter().any(contains_pipe_placeholder),
        Node::MethodCall { object, args, .. } => {
            contains_pipe_placeholder(object) || args.iter().any(contains_pipe_placeholder)
        }
        Node::BinaryOp { left, right, .. } => {
            contains_pipe_placeholder(left) || contains_pipe_placeholder(right)
        }
        Node::UnaryOp { operand, .. } => contains_pipe_placeholder(operand),
        Node::ListLiteral(items) => items.iter().any(contains_pipe_placeholder),
        Node::PropertyAccess { object, .. } => contains_pipe_placeholder(object),
        Node::SubscriptAccess { object, index } => {
            contains_pipe_placeholder(object) || contains_pipe_placeholder(index)
        }
        _ => false,
    }
}

/// Replace all `_` identifiers with `__pipe` in an AST node (for pipe placeholder desugaring).
fn replace_pipe_placeholder(node: &SNode) -> SNode {
    let new_node = match &node.node {
        Node::Identifier(name) if name == "_" => Node::Identifier("__pipe".into()),
        Node::FunctionCall { name, args } => Node::FunctionCall {
            name: name.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::MethodCall {
            object,
            method,
            args,
        } => Node::MethodCall {
            object: Box::new(replace_pipe_placeholder(object)),
            method: method.clone(),
            args: args.iter().map(replace_pipe_placeholder).collect(),
        },
        Node::BinaryOp { op, left, right } => Node::BinaryOp {
            op: op.clone(),
            left: Box::new(replace_pipe_placeholder(left)),
            right: Box::new(replace_pipe_placeholder(right)),
        },
        Node::UnaryOp { op, operand } => Node::UnaryOp {
            op: op.clone(),
            operand: Box::new(replace_pipe_placeholder(operand)),
        },
        Node::ListLiteral(items) => {
            Node::ListLiteral(items.iter().map(replace_pipe_placeholder).collect())
        }
        Node::PropertyAccess { object, property } => Node::PropertyAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            property: property.clone(),
        },
        Node::SubscriptAccess { object, index } => Node::SubscriptAccess {
            object: Box::new(replace_pipe_placeholder(object)),
            index: Box::new(replace_pipe_placeholder(index)),
        },
        _ => return node.clone(),
    };
    SNode::new(new_node, node.span)
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
