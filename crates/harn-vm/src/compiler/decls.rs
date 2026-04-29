use std::rc::Rc;

use harn_parser::{Attribute, DictEntry, Node, SNode, StructField, TypedParam};

use crate::chunk::{CompiledFunction, Constant, Op};

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_enum_construct(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[SNode],
    ) -> Result<(), CompileError> {
        for arg in args {
            self.compile_node(arg)?;
        }
        let enum_idx = self
            .chunk
            .add_constant(Constant::String(enum_name.to_string()));
        let var_idx = self
            .chunk
            .add_constant(Constant::String(variant.to_string()));
        // BuildEnum operands: enum_name_idx, variant_idx, field_count.
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
        Ok(())
    }

    pub(super) fn compile_struct_construct(
        &mut self,
        struct_name: &str,
        fields: &[DictEntry],
    ) -> Result<(), CompileError> {
        // Route through `__make_struct` so impl dispatch sees a StructInstance.
        let make_idx = self
            .chunk
            .add_constant(Constant::String("__make_struct".to_string()));
        let struct_name_idx = self
            .chunk
            .add_constant(Constant::String(struct_name.to_string()));
        self.chunk.emit_u16(Op::Constant, make_idx, self.line);
        self.chunk
            .emit_u16(Op::Constant, struct_name_idx, self.line);

        for entry in fields {
            self.compile_node(&entry.key)?;
            self.compile_node(&entry.value)?;
        }
        self.chunk
            .emit_u16(Op::BuildDict, fields.len() as u16, self.line);
        let arg_count = if let Some(field_names) = self.struct_layouts.get(struct_name).cloned() {
            self.emit_string_list(&field_names);
            3
        } else {
            2
        };
        self.chunk.emit_u8(Op::Call, arg_count, self.line);
        Ok(())
    }

    pub(super) fn compile_impl_block(
        &mut self,
        type_name: &str,
        methods: &[SNode],
    ) -> Result<(), CompileError> {
        // Lower into a `__impl_TypeName` dict of name -> closure.
        for method_sn in methods {
            if let Node::FnDecl {
                name, params, body, ..
            } = &method_sn.node
            {
                let key_idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::Constant, key_idx, self.line);

                let mut fn_compiler = Compiler::for_nested_body();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.interface_methods = self.interface_methods.clone();
                fn_compiler.type_aliases = self.type_aliases.clone();
                fn_compiler.struct_layouts = self.struct_layouts.clone();
                fn_compiler.declare_param_slots(params);
                fn_compiler.record_param_types(params);
                fn_compiler.emit_default_preamble(params)?;
                fn_compiler.emit_type_checks(params);
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Nil, self.line);
                fn_compiler.chunk.emit(Op::Return, self.line);

                let func = CompiledFunction {
                    name: format!("{}.{}", type_name, name),
                    params: TypedParam::names(params),
                    default_start: TypedParam::default_start(params),
                    chunk: Rc::new(fn_compiler.chunk),
                    is_generator: false,
                    is_stream: false,
                    has_rest_param: false,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(Rc::new(func));
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
        self.emit_define_binding(&impl_name, false);
        Ok(())
    }

    pub(super) fn compile_struct_decl(
        &mut self,
        name: &str,
        fields: &[StructField],
    ) -> Result<(), CompileError> {
        // Emit a constructor: StructName({field: val, ...}) -> StructInstance.
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        let params = vec![TypedParam::untyped("__fields")];
        fn_compiler.declare_param_slots(&params);
        fn_compiler.emit_default_preamble(&params)?;

        let make_idx = fn_compiler
            .chunk
            .add_constant(Constant::String("__make_struct".into()));
        fn_compiler
            .chunk
            .emit_u16(Op::Constant, make_idx, self.line);
        let sname_idx = fn_compiler
            .chunk
            .add_constant(Constant::String(name.to_string()));
        fn_compiler
            .chunk
            .emit_u16(Op::Constant, sname_idx, self.line);
        fn_compiler.emit_get_binding("__fields");
        let field_names: Vec<String> = fields.iter().map(|field| field.name.clone()).collect();
        fn_compiler.emit_string_list(&field_names);
        fn_compiler.chunk.emit_u8(Op::Call, 3, self.line);
        fn_compiler.chunk.emit(Op::Return, self.line);

        let func = CompiledFunction {
            name: name.to_string(),
            params: TypedParam::names(&params),
            default_start: None,
            chunk: Rc::new(fn_compiler.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(Rc::new(func));
        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        self.emit_define_binding(name, false);
        Ok(())
    }

    pub(super) fn emit_string_list(&mut self, values: &[String]) {
        for value in values {
            let idx = self.chunk.add_constant(Constant::String(value.clone()));
            self.chunk.emit_u16(Op::Constant, idx, self.line);
        }
        self.chunk
            .emit_u16(Op::BuildList, values.len() as u16, self.line);
    }

    pub(super) fn compile_attributed_decl(
        &mut self,
        attributes: &[Attribute],
        inner: &SNode,
    ) -> Result<(), CompileError> {
        // Validate first so misuse fails before we emit any code.
        for attr in attributes {
            if attr.name == "acp_tool" && !matches!(inner.node, Node::FnDecl { .. }) {
                return Err(CompileError {
                    message: "@acp_tool can only be applied to function declarations".into(),
                    line: self.line,
                });
            }
            if attr.name == "acp_skill" && !matches!(inner.node, Node::FnDecl { .. }) {
                return Err(CompileError {
                    message: "@acp_skill can only be applied to function declarations".into(),
                    line: self.line,
                });
            }
        }
        self.compile_node(inner)?;
        // @acp_tool desugars to a `tool_define(...)` call that
        // mirrors the imperative tool registration path. Emitted
        // after the inner FnDecl so the handler binding is in
        // scope. @acp_skill follows the same pattern against the
        // skill registry.
        for attr in attributes {
            if attr.name == "acp_tool" {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.emit_acp_tool_registration(attr, name)?;
                }
            } else if attr.name == "acp_skill" {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.emit_acp_skill_registration(attr, name)?;
                }
            }
        }
        Ok(())
    }

    /// Emit bytecode equivalent to:
    ///   tool_define(tool_registry(), <attr.name | fn_name>, "", {
    ///     handler: <fn_name>,
    ///     annotations: { kind: ..., side_effect_level: ..., ... },
    ///   })
    /// `annotations` collects every named attribute arg except `name`.
    pub(super) fn emit_acp_tool_registration(
        &mut self,
        attr: &harn_parser::Attribute,
        fn_name: &str,
    ) -> Result<(), CompileError> {
        let tool_name = attr
            .string_arg("name")
            .unwrap_or_else(|| fn_name.to_string());

        // Push tool_define
        let define_idx = self
            .chunk
            .add_constant(Constant::String("tool_define".into()));
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Push tool_registry()
        let reg_idx = self
            .chunk
            .add_constant(Constant::String("tool_registry".into()));
        self.chunk.emit_u16(Op::Constant, reg_idx, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        // Push tool name
        let name_const = self.chunk.add_constant(Constant::String(tool_name));
        self.chunk.emit_u16(Op::Constant, name_const, self.line);

        // Push empty description
        let desc_const = self.chunk.add_constant(Constant::String(String::new()));
        self.chunk.emit_u16(Op::Constant, desc_const, self.line);

        // Build config dict: { handler: <fn>, annotations: {...} }
        let handler_key = self.chunk.add_constant(Constant::String("handler".into()));
        self.chunk.emit_u16(Op::Constant, handler_key, self.line);
        self.emit_get_binding(fn_name);

        // Annotations dict from named args (skip "name").
        let mut ann_count: u16 = 0;
        for arg in &attr.args {
            let Some(ref key) = arg.name else {
                continue;
            };
            if key == "name" {
                continue;
            }
            let key_idx = self.chunk.add_constant(Constant::String(key.clone()));
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(&arg.value)?;
            ann_count += 1;
        }
        let ann_key_idx = self
            .chunk
            .add_constant(Constant::String("annotations".into()));
        self.chunk.emit_u16(Op::Constant, ann_key_idx, self.line);
        self.chunk.emit_u16(Op::BuildDict, ann_count, self.line);

        // Build outer config dict with 2 entries: handler + annotations.
        self.chunk.emit_u16(Op::BuildDict, 2, self.line);

        // Call tool_define(registry, name, desc, config) — 4 args.
        self.chunk.emit_u8(Op::Call, 4, self.line);
        self.chunk.emit(Op::Pop, self.line);
        Ok(())
    }

    /// Emit bytecode equivalent to:
    ///   skill_define(skill_registry(), <attr.name | fn_name>, {
    ///     on_activate: <fn_name>,
    ///     ...attribute_args (excluding `name`)
    ///   })
    ///
    /// Each attribute argument (except `name`) becomes a config dict
    /// entry — the attribute literal is the value. This lets authors
    /// write `@acp_skill(name: "deploy", when_to_use: "...", invocation: "explicit")`
    /// and have the resulting skill entry carry those fields. The
    /// annotated fn itself is registered as the `on_activate` lifecycle
    /// hook so invoking the skill calls the user's function.
    pub(super) fn emit_acp_skill_registration(
        &mut self,
        attr: &harn_parser::Attribute,
        fn_name: &str,
    ) -> Result<(), CompileError> {
        let skill_name = attr
            .string_arg("name")
            .unwrap_or_else(|| fn_name.to_string());

        // Push skill_define
        let define_idx = self
            .chunk
            .add_constant(Constant::String("skill_define".into()));
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Push skill_registry()
        let reg_idx = self
            .chunk
            .add_constant(Constant::String("skill_registry".into()));
        self.chunk.emit_u16(Op::Constant, reg_idx, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        // Push skill name
        let name_const = self.chunk.add_constant(Constant::String(skill_name));
        self.chunk.emit_u16(Op::Constant, name_const, self.line);

        // Build config dict: every named attr arg (except `name`) + on_activate.
        let mut entries: u16 = 0;
        for arg in &attr.args {
            let Some(ref key) = arg.name else {
                continue;
            };
            if key == "name" {
                continue;
            }
            let key_idx = self.chunk.add_constant(Constant::String(key.clone()));
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(&arg.value)?;
            entries += 1;
        }

        // on_activate: <fn_name>
        let activate_key = self
            .chunk
            .add_constant(Constant::String("on_activate".into()));
        self.chunk.emit_u16(Op::Constant, activate_key, self.line);
        self.emit_get_binding(fn_name);
        entries += 1;

        self.chunk.emit_u16(Op::BuildDict, entries, self.line);

        // Call skill_define(registry, name, config) — 3 args.
        self.chunk.emit_u8(Op::Call, 3, self.line);
        self.chunk.emit(Op::Pop, self.line);
        Ok(())
    }

    /// Compile a literal-only attribute argument value to a constant push.
    pub(super) fn compile_attribute_value(&mut self, node: &SNode) -> Result<(), CompileError> {
        match &node.node {
            Node::StringLiteral(s) | Node::RawStringLiteral(s) => {
                let idx = self.chunk.add_constant(Constant::String(s.clone()));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::IntLiteral(i) => {
                let idx = self.chunk.add_constant(Constant::Int(*i));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::FloatLiteral(f) => {
                let idx = self.chunk.add_constant(Constant::Float(*f));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::BoolLiteral(b) => {
                self.chunk
                    .emit(if *b { Op::True } else { Op::False }, self.line);
            }
            Node::NilLiteral => {
                self.chunk.emit(Op::Nil, self.line);
            }
            Node::Identifier(name) => {
                // Treat bare identifiers as string sentinels (e.g. `kind: edit`
                // should behave the same as `kind: "edit"`). This mirrors
                // common attribute-DSL ergonomics.
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            _ => {
                return Err(CompileError {
                    message: "attribute argument must be a literal value".into(),
                    line: self.line,
                });
            }
        }
        Ok(())
    }
}
