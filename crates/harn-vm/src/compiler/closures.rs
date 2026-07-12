use std::collections::BTreeMap;
use std::sync::Arc;

use harn_parser::{SNode, TypeParam, TypedParam};

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
        type_params: &[TypeParam],
        params: &[TypedParam],
        body: &[SNode],
        is_stream: bool,
    ) -> Result<(), CompileError> {
        let mut fn_compiler = self.nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.enum_variant_owners = self.enum_variant_owners.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        fn_compiler.declare_param_slots(params);
        fn_compiler.record_param_types(params);
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        let is_gen = is_stream || body_contains_yield(body);
        fn_compiler.seed_captured_idents(body);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        fn_compiler.drain_finallys_to_floor(0)?;
        fn_compiler.chunk.emit(Op::Nil, self.line);
        fn_compiler.chunk.emit(Op::Return, self.line);

        let param_slots = crate::chunk::ParamSlot::vec_from_typed(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&fn_compiler.chunk, &format!("fn `{name}`"), self.line)?;
        let func = CompiledFunction {
            name: name.to_string(),
            type_params: type_params.iter().map(|param| param.name.clone()).collect(),
            nominal_type_names: fn_compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(fn_compiler.chunk),
            is_generator: is_gen,
            is_stream,
            has_rest_param: params.last().is_some_and(|p| p.rest),
            has_runtime_type_checks,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(Arc::new(func));

        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        self.emit_define_binding(name, false);
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
        let mut fn_compiler = self.nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.enum_variant_owners = self.enum_variant_owners.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        fn_compiler.declare_param_slots(params);
        fn_compiler.record_param_types(params);
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        fn_compiler.seed_captured_idents(body);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        fn_compiler.drain_finallys_to_floor(0)?;
        fn_compiler.chunk.emit(Op::Return, self.line);

        let param_slots = crate::chunk::ParamSlot::vec_from_typed(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&fn_compiler.chunk, &format!("fn `{name}`"), self.line)?;
        let func = CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: fn_compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(fn_compiler.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: params.last().is_some_and(|p| p.rest),
            has_runtime_type_checks,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(Arc::new(func));

        let define_name = self.string_constant("tool_define");
        self.chunk.emit_u16(Op::Constant, define_name, self.line);

        let reg_name = self.string_constant("tool_registry");
        self.chunk.emit_u16(Op::Constant, reg_name, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        let tool_name_idx = self.string_constant(name);
        self.chunk.emit_u16(Op::Constant, tool_name_idx, self.line);

        let desc = description.as_deref().unwrap_or("");
        let desc_idx = self.string_constant(desc);
        self.chunk.emit_u16(Op::Constant, desc_idx, self.line);

        // Build parameters dict using the same schema lowering as
        // runtime param validation so tools expose nested shapes,
        // unions, item schemas, defaults, and dict value schemas.
        let mut param_count: u16 = 0;
        for p in params {
            let pn_idx = self.string_constant(&p.name);
            self.chunk.emit_u16(Op::Constant, pn_idx, self.line);

            let base_schema = p
                .type_expr
                .as_ref()
                .and_then(Self::type_expr_to_schema_value)
                .unwrap_or_else(|| {
                    VmValue::dict(BTreeMap::from([(
                        "type".to_string(),
                        VmValue::String(arcstr::ArcStr::from("any")),
                    )]))
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
                _ => crate::value::DictMap::new(),
            };

            if p.default_value.is_some() {
                param_schema.insert(crate::value::intern_key("required"), VmValue::Bool(false));
            }

            self.emit_vm_value_literal(&VmValue::dict(param_schema));

            if let Some(default_value) = p.default_value.as_ref() {
                let default_key = self.string_constant("default");
                self.chunk.emit_u16(Op::Constant, default_key, self.line);
                self.compile_node(default_value)?;
                self.chunk.emit_u16(Op::BuildDict, 1, self.line);
                self.chunk.emit(Op::Add, self.line);
            }

            param_count += 1;
        }
        self.chunk.emit_u16(Op::BuildDict, param_count, self.line);

        let params_key = self.string_constant("parameters");
        self.chunk.emit_u16(Op::Constant, params_key, self.line);
        self.chunk.emit(Op::Swap, self.line);

        let handler_key = self.string_constant("handler");
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
                            "failed to lower tool return schema for '{name}': {error}"
                        ),
                        line: self.line,
                    }
                })?;
            let returns_key = self.string_constant("returns");
            self.chunk.emit_u16(Op::Constant, returns_key, self.line);
            self.emit_vm_value_literal(&return_type);
            config_entries += 1;
        }

        self.chunk
            .emit_u16(Op::BuildDict, config_entries, self.line);

        self.chunk.emit_u8(Op::Call, 4, self.line);

        self.emit_define_binding(name, false);
        Ok(())
    }

    /// Lower a `skill NAME { key value ... }` declaration to
    /// `let NAME = skill_define(skill_registry(), NAME, { key: value, ... })`.
    ///
    /// Each `(field_name, expr)` pair becomes a dict entry. Lifecycle hook
    /// expressions (e.g. `on_activate fn() { ... }`) lower to closure
    /// values just like any other `fn(...) { ... }` literal.
    pub(super) fn compile_skill_decl(
        &mut self,
        name: &str,
        fields: &[(String, SNode)],
    ) -> Result<(), CompileError> {
        // Push skill_define
        let define_idx = self.string_constant("skill_define");
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Push skill_registry()
        let reg_idx = self.string_constant("skill_registry");
        self.chunk.emit_u16(Op::Constant, reg_idx, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        // Push skill name
        let name_const = self.string_constant(name);
        self.chunk.emit_u16(Op::Constant, name_const, self.line);

        // Build config dict from fields.
        let mut field_count: u16 = 0;
        for (key, value) in fields {
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_node(value)?;
            field_count += 1;
        }
        self.chunk.emit_u16(Op::BuildDict, field_count, self.line);

        // Call skill_define(registry, name, config) — 3 args.
        self.chunk.emit_u8(Op::Call, 3, self.line);

        // Bind result to skill name as a variable.
        self.emit_define_binding(name, false);
        Ok(())
    }

    pub(super) fn compile_eval_pack_decl(
        &mut self,
        binding_name: &str,
        pack_id: &str,
        fields: &[(String, SNode)],
        body: &[SNode],
        summarize: &Option<Vec<SNode>>,
        run_body: bool,
    ) -> Result<(), CompileError> {
        self.emit_eval_pack_manifest_value(pack_id, fields)?;
        self.emit_define_binding(binding_name, false);

        if run_body && (!body.is_empty() || summarize.is_some()) {
            self.begin_scope();
            let finally_floor = self.finally_bodies.len();

            let mut visible_fields = vec!["id".to_string(), "version".to_string()];
            for (field_name, _) in fields {
                if !visible_fields.iter().any(|name| name == field_name) {
                    visible_fields.push(field_name.clone());
                }
            }
            for field_name in visible_fields {
                self.emit_get_binding(binding_name);
                let field_idx = self.string_constant(&field_name);
                self.chunk.emit_u16(Op::GetProperty, field_idx, self.line);
                self.emit_define_binding(&field_name, false);
            }

            for sn in body {
                self.compile_discarded_stmt(sn)?;
            }
            if let Some(summary_body) = summarize {
                for sn in summary_body {
                    self.compile_discarded_stmt(sn)?;
                }
            }
            self.drain_finallys_to_floor(finally_floor)?;
            self.end_scope();
        }
        Ok(())
    }

    fn emit_eval_pack_manifest_value(
        &mut self,
        pack_id: &str,
        fields: &[(String, SNode)],
    ) -> Result<(), CompileError> {
        let manifest_idx = self.string_constant("eval_pack_manifest");
        self.chunk.emit_u16(Op::Constant, manifest_idx, self.line);

        let has_id = fields.iter().any(|(key, _)| key == "id");
        let has_version = fields.iter().any(|(key, _)| key == "version");
        let mut entry_count = fields.len() as u16;
        if !has_version {
            let key_idx = self.string_constant("version");
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            let value_idx = self.chunk.add_constant(Constant::Int(1));
            self.chunk.emit_u16(Op::Constant, value_idx, self.line);
            entry_count += 1;
        }
        if !has_id {
            let key_idx = self.string_constant("id");
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            let value_idx = self.string_constant(pack_id);
            self.chunk.emit_u16(Op::Constant, value_idx, self.line);
            entry_count += 1;
        }
        for (key, value) in fields {
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_node(value)?;
        }
        self.chunk.emit_u16(Op::BuildDict, entry_count, self.line);
        self.chunk.emit_u8(Op::Call, 1, self.line);
        Ok(())
    }

    pub(super) fn compile_closure(
        &mut self,
        params: &[TypedParam],
        body: &[SNode],
    ) -> Result<(), CompileError> {
        let mut fn_compiler = self.nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.enum_variant_owners = self.enum_variant_owners.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        fn_compiler.declare_param_slots(params);
        fn_compiler.record_param_types(params);
        fn_compiler.emit_default_preamble(params)?;
        fn_compiler.emit_type_checks(params);
        let is_gen = body_contains_yield(body);
        fn_compiler.seed_captured_idents(body);
        fn_compiler.compile_block(body)?;
        // Run pending defers before implicit return
        fn_compiler.drain_finallys_to_floor(0)?;
        fn_compiler.chunk.emit(Op::Return, self.line);

        let param_slots = crate::chunk::ParamSlot::vec_from_typed(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&fn_compiler.chunk, "closure", self.line)?;
        let func = CompiledFunction {
            name: "<closure>".to_string(),
            type_params: Vec::new(),
            nominal_type_names: fn_compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(fn_compiler.chunk),
            is_generator: is_gen,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(Arc::new(func));

        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        Ok(())
    }
}
