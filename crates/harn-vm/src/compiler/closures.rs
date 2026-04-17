use std::collections::BTreeMap;
use std::rc::Rc;

use harn_parser::{SNode, TypedParam};

use crate::chunk::{CompiledFunction, Constant, Op};
use crate::schema;
use crate::value::VmValue;

use super::error::CompileError;
use super::yield_scan::body_contains_yield;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_fn_decl(
        &mut self,
        name: &str,
        params: &[TypedParam],
        body: &[SNode],
    ) -> Result<(), CompileError> {
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        let is_gen = body_contains_yield(body);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        for fb in fn_compiler.all_pending_finallys() {
            fn_compiler.compile_finally_inline(&fb)?;
        }
        fn_compiler.chunk.emit(Op::Nil, self.line);
        fn_compiler.chunk.emit(Op::Return, self.line);

        let func = CompiledFunction {
            name: name.to_string(),
            params: TypedParam::names(params),
            default_start: TypedParam::default_start(params),
            chunk: fn_compiler.chunk,
            is_generator: is_gen,
            has_rest_param: params.last().is_some_and(|p| p.rest),
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(func);

        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        let name_idx = self.chunk.add_constant(Constant::String(name.to_string()));
        self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
        Ok(())
    }

    pub(super) fn compile_tool_decl(
        &mut self,
        name: &str,
        description: &Option<String>,
        params: &[TypedParam],
        return_type: &Option<harn_parser::TypeExpr>,
        body: &[SNode],
    ) -> Result<(), CompileError> {
        // Compile the body as a closure, then call `tool_define(registry, name, description, config)`.
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        for fb in fn_compiler.all_pending_finallys() {
            fn_compiler.compile_finally_inline(&fb)?;
        }
        fn_compiler.chunk.emit(Op::Return, self.line);

        let func = CompiledFunction {
            name: name.to_string(),
            params: TypedParam::names(params),
            default_start: TypedParam::default_start(params),
            chunk: fn_compiler.chunk,
            is_generator: false,
            has_rest_param: params.last().is_some_and(|p| p.rest),
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(func);

        let define_name = self
            .chunk
            .add_constant(Constant::String("tool_define".into()));
        self.chunk.emit_u16(Op::Constant, define_name, self.line);

        let reg_name = self
            .chunk
            .add_constant(Constant::String("tool_registry".into()));
        self.chunk.emit_u16(Op::Constant, reg_name, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        let tool_name_idx = self.chunk.add_constant(Constant::String(name.to_string()));
        self.chunk.emit_u16(Op::Constant, tool_name_idx, self.line);

        let desc = description.as_deref().unwrap_or("");
        let desc_idx = self.chunk.add_constant(Constant::String(desc.to_string()));
        self.chunk.emit_u16(Op::Constant, desc_idx, self.line);

        // Build parameters dict using the same schema lowering as
        // runtime param validation so tools expose nested shapes,
        // unions, item schemas, defaults, and dict value schemas.
        let mut param_count: u16 = 0;
        for p in params {
            let pn_idx = self.chunk.add_constant(Constant::String(p.name.clone()));
            self.chunk.emit_u16(Op::Constant, pn_idx, self.line);

            let base_schema = p
                .type_expr
                .as_ref()
                .and_then(Self::type_expr_to_schema_value)
                .unwrap_or_else(|| {
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "type".to_string(),
                        VmValue::String(Rc::from("any")),
                    )])))
                });
            let public_schema =
                schema::schema_to_json_schema_value(&base_schema).map_err(|error| {
                    CompileError {
                        message: format!(
                            "failed to lower tool parameter schema for '{}': {}",
                            p.name, error
                        ),
                        line: self.line,
                    }
                })?;
            let mut param_schema = match public_schema {
                VmValue::Dict(map) => (*map).clone(),
                _ => BTreeMap::new(),
            };

            if p.default_value.is_some() {
                param_schema.insert("required".to_string(), VmValue::Bool(false));
            }

            self.emit_vm_value_literal(&VmValue::Dict(Rc::new(param_schema)));

            if let Some(default_value) = p.default_value.as_ref() {
                let default_key = self.chunk.add_constant(Constant::String("default".into()));
                self.chunk.emit_u16(Op::Constant, default_key, self.line);
                self.compile_node(default_value)?;
                self.chunk.emit_u16(Op::BuildDict, 1, self.line);
                self.chunk.emit(Op::Add, self.line);
            }

            param_count += 1;
        }
        self.chunk.emit_u16(Op::BuildDict, param_count, self.line);

        let params_key = self
            .chunk
            .add_constant(Constant::String("parameters".into()));
        self.chunk.emit_u16(Op::Constant, params_key, self.line);
        self.chunk.emit(Op::Swap, self.line);

        let handler_key = self.chunk.add_constant(Constant::String("handler".into()));
        self.chunk.emit_u16(Op::Constant, handler_key, self.line);
        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);

        let mut config_entries = 2u16;
        if let Some(return_type) = return_type
            .as_ref()
            .and_then(Self::type_expr_to_schema_value)
        {
            let return_type =
                schema::schema_to_json_schema_value(&return_type).map_err(|error| {
                    CompileError {
                        message: format!(
                            "failed to lower tool return schema for '{}': {}",
                            name, error
                        ),
                        line: self.line,
                    }
                })?;
            let returns_key = self.chunk.add_constant(Constant::String("returns".into()));
            self.chunk.emit_u16(Op::Constant, returns_key, self.line);
            self.emit_vm_value_literal(&return_type);
            config_entries += 1;
        }

        self.chunk
            .emit_u16(Op::BuildDict, config_entries, self.line);

        self.chunk.emit_u8(Op::Call, 4, self.line);

        let bind_idx = self.chunk.add_constant(Constant::String(name.to_string()));
        self.chunk.emit_u16(Op::DefLet, bind_idx, self.line);
        Ok(())
    }

    pub(super) fn compile_closure(
        &mut self,
        params: &[TypedParam],
        body: &[SNode],
    ) -> Result<(), CompileError> {
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        let is_gen = body_contains_yield(body);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        for fb in fn_compiler.all_pending_finallys() {
            fn_compiler.compile_finally_inline(&fb)?;
        }
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
        Ok(())
    }
}
