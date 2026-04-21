use harn_lexer::StringSegment;
use harn_parser::{DictEntry, Node, SNode, TypedParam};

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::pipe::{contains_pipe_placeholder, replace_pipe_placeholder};
use super::Compiler;

impl Compiler {
    pub(super) fn compile_binary_op(
        &mut self,
        op: &str,
        left: &SNode,
        right: &SNode,
    ) -> Result<(), CompileError> {
        match op {
            "&&" => {
                self.compile_node(left)?;
                let jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_node(right)?;
                self.chunk.patch_jump(jump);
                // Normalize to bool.
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
                self.chunk.emit(Op::Nil, self.line);
                self.chunk.emit(Op::NotEqual, self.line);
                let jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.emit(Op::Pop, self.line);
                self.compile_node(right)?;
                let end = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(jump);
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.patch_jump(end);
                return Ok(());
            }
            "|>" => {
                self.compile_node(left)?;
                // `value |> func(_, arg)` desugars to `value |> { __pipe -> func(__pipe, arg) }`.
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

        let left_type = self.infer_expr_type(left);
        let right_type = self.infer_expr_type(right);
        self.compile_node(left)?;
        self.compile_node(right)?;
        if let Some(typed_op) =
            self.specialized_binary_op(op, left_type.as_ref(), right_type.as_ref())
        {
            self.chunk.emit(typed_op, self.line);
            return Ok(());
        }
        self.emit_generic_binary_op(op)?;
        Ok(())
    }

    pub(super) fn emit_generic_binary_op(&mut self, op: &str) -> Result<(), CompileError> {
        match op {
            "+" => self.chunk.emit(Op::Add, self.line),
            "-" => self.chunk.emit(Op::Sub, self.line),
            "*" => self.chunk.emit(Op::Mul, self.line),
            "/" => self.chunk.emit(Op::Div, self.line),
            "%" => self.chunk.emit(Op::Mod, self.line),
            "**" => self.chunk.emit(Op::Pow, self.line),
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
        Ok(())
    }

    pub(super) fn compile_function_call(
        &mut self,
        name: &str,
        args: &[SNode],
    ) -> Result<(), CompileError> {
        // Compile-time lowering: `schema_of(TypeAlias)` emits the
        // alias's JSON-Schema dict as a constant. Falls through to
        // the runtime `schema_of(...)` builtin when the argument is
        // not a known type alias (e.g. a string name computed at
        // runtime, or a dict pass-through).
        if name == "schema_of" && args.len() == 1 {
            if let Node::Identifier(alias) = &args[0].node {
                if let Some(schema) = self.schema_value_for_alias(alias) {
                    self.emit_vm_value_literal(&schema);
                    return Ok(());
                }
            }
        }
        // `schema_is(x, T)` / `schema_expect(x, T[, defaults])` /
        // `schema_parse(x, T)` / `schema_check(x, T)` /
        // `is_type(x, T)`: when the schema argument is a type-alias
        // identifier, inline the alias's JSON-Schema dict as a
        // constant. This is the counterpart to the parser-side
        // narrowing in `schema_type_expr_from_node`.
        if Self::is_schema_guard(name) && args.len() >= 2 {
            if let Node::Identifier(alias) = &args[1].node {
                if let Some(schema) = self.schema_value_for_alias(alias) {
                    let name_idx = self.chunk.add_constant(Constant::String(name.to_string()));
                    self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                    self.compile_node(&args[0])?;
                    self.emit_vm_value_literal(&schema);
                    for arg in &args[2..] {
                        self.compile_node(arg)?;
                    }
                    self.chunk.emit_u8(Op::Call, args.len() as u8, self.line);
                    return Ok(());
                }
            }
        }

        let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
        let name_idx = self.chunk.add_constant(Constant::String(name.to_string()));
        self.chunk.emit_u16(Op::Constant, name_idx, self.line);

        if has_spread {
            // Flush-and-concat pattern: build args into one list
            // (same as ListLiteral with spreads).
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
            for arg in args {
                self.compile_node(arg)?;
            }
            self.chunk.emit_u8(Op::Call, args.len() as u8, self.line);
        }
        Ok(())
    }

    pub(super) fn compile_method_call(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
    ) -> Result<(), CompileError> {
        // EnumName.Variant(args) desugars to BuildEnum.
        if let Node::Identifier(name) = &object.node {
            if self.enum_names.contains(name) {
                for arg in args {
                    self.compile_node(arg)?;
                }
                let enum_idx = self.chunk.add_constant(Constant::String(name.clone()));
                let var_idx = self
                    .chunk
                    .add_constant(Constant::String(method.to_string()));
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
        let name_idx = self
            .chunk
            .add_constant(Constant::String(method.to_string()));
        if has_spread {
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
        Ok(())
    }

    pub(super) fn compile_property_access(
        &mut self,
        object: &SNode,
        property: &str,
    ) -> Result<(), CompileError> {
        // Bare `EnumName.Variant` desugars to a zero-field BuildEnum.
        if let Node::Identifier(name) = &object.node {
            if self.enum_names.contains(name) {
                let enum_idx = self.chunk.add_constant(Constant::String(name.clone()));
                let var_idx = self
                    .chunk
                    .add_constant(Constant::String(property.to_string()));
                self.chunk.emit_u16(Op::BuildEnum, enum_idx, self.line);
                let hi = (var_idx >> 8) as u8;
                let lo = var_idx as u8;
                self.chunk.code.push(hi);
                self.chunk.code.push(lo);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
                self.chunk.lines.push(self.line);
                self.chunk.columns.push(self.column);
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
        let idx = self
            .chunk
            .add_constant(Constant::String(property.to_string()));
        self.chunk.emit_u16(Op::GetProperty, idx, self.line);
        Ok(())
    }

    pub(super) fn compile_list_literal(&mut self, elements: &[SNode]) -> Result<(), CompileError> {
        let has_spread = elements.iter().any(|e| matches!(&e.node, Node::Spread(_)));
        if !has_spread {
            for el in elements {
                self.compile_node(el)?;
            }
            self.chunk
                .emit_u16(Op::BuildList, elements.len() as u16, self.line);
        } else {
            // Flush-and-concat: accumulate non-spread elements and concat with spread lists.
            self.chunk.emit_u16(Op::BuildList, 0, self.line);
            let mut pending = 0u16;
            for el in elements {
                if let Node::Spread(inner) = &el.node {
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
                    self.compile_node(el)?;
                    pending += 1;
                }
            }
            if pending > 0 {
                self.chunk.emit_u16(Op::BuildList, pending, self.line);
                self.chunk.emit(Op::Add, self.line);
            }
        }
        Ok(())
    }

    pub(super) fn compile_dict_literal(
        &mut self,
        entries: &[DictEntry],
    ) -> Result<(), CompileError> {
        let has_spread = entries
            .iter()
            .any(|e| matches!(&e.value.node, Node::Spread(_)));
        if !has_spread {
            for entry in entries {
                self.compile_node(&entry.key)?;
                // Sugar: `output_schema: TypeAlias` inside an
                // `llm_call(..., { ... })` options dict lowers to
                // the alias's JSON-Schema dict constant. This lets
                // users reuse one `type T = { ... }` declaration
                // for both type-checking and structured-output
                // validation. Falls through to the normal expression
                // path when the name does not resolve.
                if Self::entry_key_is(&entry.key, "output_schema") {
                    if let Node::Identifier(alias) = &entry.value.node {
                        if let Some(schema) = self.schema_value_for_alias(alias) {
                            self.emit_vm_value_literal(&schema);
                            continue;
                        }
                    }
                }
                self.compile_node(&entry.value)?;
            }
            self.chunk
                .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
        } else {
            // Flush-and-merge via Add on empty dict.
            self.chunk.emit_u16(Op::BuildDict, 0, self.line);
            let mut pending = 0u16;
            for entry in entries {
                if let Node::Spread(inner) = &entry.value.node {
                    if pending > 0 {
                        self.chunk.emit_u16(Op::BuildDict, pending, self.line);
                        self.chunk.emit(Op::Add, self.line);
                        pending = 0;
                    }
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
        Ok(())
    }

    pub(super) fn compile_interpolated_string(
        &mut self,
        segments: &[StringSegment],
    ) -> Result<(), CompileError> {
        let mut part_count = 0u16;
        for seg in segments {
            match seg {
                StringSegment::Literal(s) => {
                    let idx = self.chunk.add_constant(Constant::String(s.clone()));
                    self.chunk.emit_u16(Op::Constant, idx, self.line);
                    part_count += 1;
                }
                StringSegment::Expression(expr_str, expr_line, expr_col) => {
                    let mut lexer =
                        harn_lexer::Lexer::with_position(expr_str, *expr_line, *expr_col);
                    if let Ok(tokens) = lexer.tokenize() {
                        let mut parser = harn_parser::Parser::new(tokens);
                        if let Ok(snode) = parser.parse_single_expression() {
                            self.compile_node(&snode)?;
                            let to_str = self
                                .chunk
                                .add_constant(Constant::String("to_string".into()));
                            self.chunk.emit_u16(Op::Constant, to_str, self.line);
                            self.chunk.emit(Op::Swap, self.line);
                            self.chunk.emit_u8(Op::Call, 1, self.line);
                            part_count += 1;
                        } else {
                            // Fallback: treat as literal.
                            let idx = self.chunk.add_constant(Constant::String(expr_str.clone()));
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
        Ok(())
    }
}
