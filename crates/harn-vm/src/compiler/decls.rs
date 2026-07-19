use std::sync::Arc;

use harn_parser::{Attribute, DictEntry, EnumVariant, Node, SNode, StructField, TypedParam};

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
        self.emit_build_enum(enum_name, variant, args.len());
        Ok(())
    }

    /// Emit `BuildEnum` after its fields are already on the operand stack.
    pub(super) fn emit_build_enum(&mut self, enum_name: &str, variant: &str, field_count: usize) {
        let enum_idx = self.string_constant(enum_name);
        let var_idx = self.string_constant(variant);
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
        let fc = field_count as u16;
        let fhi = (fc >> 8) as u8;
        let flo = fc as u8;
        self.chunk.code.push(fhi);
        self.chunk.code.push(flo);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
        self.chunk.lines.push(self.line);
        self.chunk.columns.push(self.column);
    }

    pub(super) fn compile_struct_construct(
        &mut self,
        struct_name: &str,
        fields: &[DictEntry],
    ) -> Result<(), CompileError> {
        // Route through `__make_struct` so impl dispatch sees a StructInstance.
        let make_idx = self.string_constant("__make_struct");
        let struct_name_idx = self.string_constant(struct_name);
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
                name,
                type_params,
                params,
                body,
                ..
            } = &method_sn.node
            {
                let key_idx = self.string_constant(name);
                self.chunk.emit_u16(Op::Constant, key_idx, self.line);

                let mut fn_compiler = self.nested_body();
                fn_compiler.enum_names = self.enum_names.clone();
                fn_compiler.enum_variant_owners = self.enum_variant_owners.clone();
                fn_compiler.imported_enum_candidates = self.imported_enum_candidates.clone();
                fn_compiler.imported_enum_candidates_authoritative =
                    self.imported_enum_candidates_authoritative;
                fn_compiler.interface_methods = self.interface_methods.clone();
                fn_compiler.type_aliases = self.type_aliases.clone();
                fn_compiler.struct_layouts = self.struct_layouts.clone();
                fn_compiler.declare_param_slots(params);
                fn_compiler.record_param_types(params);
                fn_compiler.emit_default_preamble(params)?;
                fn_compiler.emit_type_checks(params);
                fn_compiler.seed_captured_idents(body);
                fn_compiler.compile_block(body)?;
                fn_compiler.chunk.emit(Op::Nil, self.line);
                fn_compiler.chunk.emit(Op::Return, self.line);

                let param_slots = fn_compiler.compile_param_slots(params);
                let has_runtime_type_checks =
                    CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
                super::ensure_chunk_addressable(
                    &fn_compiler.chunk,
                    &format!("method `{type_name}.{name}`"),
                    self.line,
                )?;
                let func = CompiledFunction {
                    name: format!("{type_name}.{name}"),
                    type_params: type_params.iter().map(|param| param.name.clone()).collect(),
                    nominal_type_names: fn_compiler.nominal_type_names(),
                    params: param_slots,
                    default_start: TypedParam::default_start(params),
                    chunk: Arc::new(fn_compiler.chunk),
                    is_generator: false,
                    is_stream: false,
                    has_rest_param: false,
                    has_runtime_type_checks,
                };
                let fn_idx = self.chunk.functions.len();
                self.chunk.functions.push(Arc::new(func));
                self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
            }
        }
        let method_count = methods
            .iter()
            .filter(|m| matches!(m.node, Node::FnDecl { .. }))
            .count();
        self.chunk
            .emit_u16(Op::BuildDict, method_count as u16, self.line);
        let impl_name = format!("__impl_{type_name}");
        self.emit_define_binding(&impl_name, false);
        Ok(())
    }

    pub(super) fn compile_struct_decl(
        &mut self,
        name: &str,
        fields: &[StructField],
    ) -> Result<(), CompileError> {
        let func = self.compile_struct_constructor(name, fields)?;
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(Arc::new(func));
        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        self.emit_define_binding(name, false);
        Ok(())
    }

    /// Materialize an enum namespace so imported enums retain the same
    /// `Color.Ready(...)` construction surface as locally declared enums.
    /// Local construction still lowers directly to `BuildEnum`; this namespace
    /// is the runtime bridge for a module whose consumer cannot see the enum
    /// declaration during its own compilation.
    pub(super) fn compile_enum_decl(
        &mut self,
        enum_name: &str,
        variants: &[EnumVariant],
    ) -> Result<(), CompileError> {
        for variant in variants {
            let key_idx = self.string_constant(&variant.name);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            if variant.fields.is_empty() {
                self.emit_build_enum(enum_name, &variant.name, 0);
                continue;
            }

            let params: Vec<TypedParam> = variant
                .fields
                .iter()
                .enumerate()
                .map(|(index, _)| TypedParam::untyped(format!("__field_{index}")))
                .collect();
            let mut constructor = self.nested_body();
            constructor.enum_names = self.enum_names.clone();
            constructor.enum_variant_owners = self.enum_variant_owners.clone();
            constructor.imported_enum_candidates = self.imported_enum_candidates.clone();
            constructor.imported_enum_candidates_authoritative =
                self.imported_enum_candidates_authoritative;
            constructor.interface_methods = self.interface_methods.clone();
            constructor.type_aliases = self.type_aliases.clone();
            constructor.struct_layouts = self.struct_layouts.clone();
            constructor.declare_param_slots(&params);
            constructor.emit_default_preamble(&params)?;
            for param in &params {
                constructor.emit_get_binding(&param.name);
            }
            constructor.emit_build_enum(enum_name, &variant.name, params.len());
            constructor.chunk.emit(Op::Return, self.line);

            let param_slots = crate::chunk::ParamSlot::vec_from_typed(&params);
            super::ensure_chunk_addressable(
                &constructor.chunk,
                &format!("enum constructor `{enum_name}.{}`", variant.name),
                self.line,
            )?;
            let function = CompiledFunction {
                name: format!("{enum_name}.{}", variant.name),
                type_params: Vec::new(),
                nominal_type_names: constructor.nominal_type_names(),
                params: param_slots,
                default_start: None,
                chunk: Arc::new(constructor.chunk),
                is_generator: false,
                is_stream: false,
                has_rest_param: false,
                has_runtime_type_checks: false,
            };
            let index = self.chunk.functions.len();
            self.chunk.functions.push(Arc::new(function));
            self.chunk.emit_u16(Op::Closure, index as u16, self.line);
        }
        self.chunk
            .emit_u16(Op::BuildDict, variants.len() as u16, self.line);
        self.emit_define_binding(enum_name, false);
        Ok(())
    }

    /// Compile the runtime constructor paired with a struct declaration.
    ///
    /// Module artifacts call this directly so an imported `pub struct`
    /// exports the same constructor closure as an in-file declaration.
    pub(crate) fn compile_struct_constructor(
        &self,
        name: &str,
        fields: &[StructField],
    ) -> Result<CompiledFunction, CompileError> {
        let mut fn_compiler = self.nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.enum_variant_owners = self.enum_variant_owners.clone();
        fn_compiler.imported_enum_candidates = self.imported_enum_candidates.clone();
        fn_compiler.imported_enum_candidates_authoritative =
            self.imported_enum_candidates_authoritative;
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        fn_compiler.struct_layouts.insert(
            name.to_string(),
            fields.iter().map(|field| field.name.clone()).collect(),
        );
        let params = vec![TypedParam::untyped("__fields")];
        fn_compiler.declare_param_slots(&params);
        fn_compiler.emit_default_preamble(&params)?;

        let make_idx = fn_compiler.string_constant("__make_struct");
        fn_compiler
            .chunk
            .emit_u16(Op::Constant, make_idx, self.line);
        let sname_idx = fn_compiler.string_constant(name);
        fn_compiler
            .chunk
            .emit_u16(Op::Constant, sname_idx, self.line);
        fn_compiler.emit_get_binding("__fields");
        let field_names: Vec<String> = fields.iter().map(|field| field.name.clone()).collect();
        fn_compiler.emit_string_list(&field_names);
        fn_compiler.chunk.emit_u8(Op::Call, 3, self.line);
        fn_compiler.chunk.emit(Op::Return, self.line);

        let param_slots = fn_compiler.compile_param_slots(&params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&fn_compiler.chunk, &format!("fn `{name}`"), self.line)?;
        Ok(CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: fn_compiler.nominal_type_names(),
            params: param_slots,
            default_start: None,
            chunk: Arc::new(fn_compiler.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks,
        })
    }

    pub(super) fn emit_string_list(&mut self, values: &[String]) {
        for value in values {
            let idx = self.string_constant(value);
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
            } else if attr.name == "step" {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.emit_step_registration(attr, name)?;
                }
            } else if attr.name == "persona" {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.emit_persona_registration(attr, name)?;
                }
            }
        }
        Ok(())
    }

    /// Emit bytecode equivalent to:
    ///   __register_persona("merge_captain", { name: "merge_captain" })
    /// so `@step` calls can inherit the active persona while the
    /// persona function is on the VM call stack.
    pub(super) fn emit_persona_registration(
        &mut self,
        attr: &harn_parser::Attribute,
        fn_name: &str,
    ) -> Result<(), CompileError> {
        let define_idx = self.string_constant("__register_persona");
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        let fn_const = self.string_constant(fn_name);
        self.chunk.emit_u16(Op::Constant, fn_const, self.line);

        let mut entries: u16 = 0;
        if let Some(name) = attr.named_arg("name") {
            let key_idx = self.string_constant("name");
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(name)?;
            entries += 1;
        }
        if let Some(stages) = attr.named_arg("stages") {
            if matches!(stages.node, Node::ListLiteral(_)) {
                let key_idx = self.string_constant("stages");
                self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                self.compile_attribute_value(stages)?;
                entries += 1;
            }
        }
        if let Some(output_style) = attr.named_arg("output_style") {
            let key_idx = self.string_constant("output_style");
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(output_style)?;
            entries += 1;
        }
        self.chunk.emit_u16(Op::BuildDict, entries, self.line);
        self.chunk.emit_u8(Op::Call, 2, self.line);
        self.chunk.emit(Op::Pop, self.line);
        Ok(())
    }

    /// Emit bytecode equivalent to:
    ///   __register_step("plan_step", { name: "plan", model: "...",
    ///                                  error_boundary: "continue",
    ///                                  budget: { max_tokens: 200, max_usd: 0.05 } })
    /// Runs at module load so the per-step metadata is in
    /// `crates/harn-vm/src/step_runtime.rs`'s registry by the time
    /// any persona body invokes the step's function.
    pub(super) fn emit_step_registration(
        &mut self,
        attr: &harn_parser::Attribute,
        fn_name: &str,
    ) -> Result<(), CompileError> {
        // Push the builtin name.
        let define_idx = self.string_constant("__register_step");
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Arg 0: function name (the registry key).
        let fn_const = self.string_constant(fn_name);
        self.chunk.emit_u16(Op::Constant, fn_const, self.line);

        // Arg 1: metadata dict — emit only the fields the step
        // declared, matching the parser's KNOWN_KEYS for @step.
        const META_KEYS: &[&str] = &["name", "model", "approval", "receipt", "error_boundary"];
        let mut entries: u16 = 0;
        for arg in &attr.args {
            let Some(ref key) = arg.name else {
                continue;
            };
            if !META_KEYS.contains(&key.as_str()) {
                continue;
            }
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(&arg.value)?;
            entries += 1;
        }
        // Budget is forwarded as a nested dict so the runtime can
        // distinguish "no budget set" from "budget set with no
        // fields". The parser already enforces shape; we just plumb
        // the dict literal through verbatim.
        if let Some(budget) = attr.named_arg("budget") {
            if matches!(budget.node, Node::DictLiteral(_)) {
                let key_idx = self.string_constant("budget");
                self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                self.compile_attribute_value(budget)?;
                entries += 1;
            }
        }
        self.chunk.emit_u16(Op::BuildDict, entries, self.line);

        // Call __register_step(fn_name, meta_dict). The builtin
        // returns nil; pop it so we don't leave dead values on the
        // stack at module top-level.
        self.chunk.emit_u8(Op::Call, 2, self.line);
        self.chunk.emit(Op::Pop, self.line);
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
        let define_idx = self.string_constant("tool_define");
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Push tool_registry()
        let reg_idx = self.string_constant("tool_registry");
        self.chunk.emit_u16(Op::Constant, reg_idx, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        // Push tool name
        let name_const = self.owned_string_constant(tool_name);
        self.chunk.emit_u16(Op::Constant, name_const, self.line);

        // Push empty description
        let desc_const = self.string_constant("");
        self.chunk.emit_u16(Op::Constant, desc_const, self.line);

        // Build config dict: { handler: <fn>, annotations: {...} }
        let handler_key = self.string_constant("handler");
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
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(&arg.value)?;
            ann_count += 1;
        }
        let ann_key_idx = self.string_constant("annotations");
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
        let define_idx = self.string_constant("skill_define");
        self.chunk.emit_u16(Op::Constant, define_idx, self.line);

        // Push skill_registry()
        let reg_idx = self.string_constant("skill_registry");
        self.chunk.emit_u16(Op::Constant, reg_idx, self.line);
        self.chunk.emit_u8(Op::Call, 0, self.line);

        // Push skill name
        let name_const = self.owned_string_constant(skill_name);
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
            let key_idx = self.string_constant(key);
            self.chunk.emit_u16(Op::Constant, key_idx, self.line);
            self.compile_attribute_value(&arg.value)?;
            entries += 1;
        }

        // on_activate: <fn_name>
        let activate_key = self.string_constant("on_activate");
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
                let idx = self.string_constant(s);
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
                // common attribute-DSL ergonomics. The parser also folds
                // dotted sentinels like `github.pr_opened` into this node.
                let idx = self.string_constant(name);
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            Node::ListLiteral(items) => {
                for item in items {
                    self.compile_attribute_value(item)?;
                }
                self.chunk
                    .emit_u16(Op::BuildList, items.len() as u16, self.line);
            }
            Node::DictLiteral(entries) => {
                for entry in entries {
                    self.compile_attribute_value(&entry.key)?;
                    self.compile_attribute_value(&entry.value)?;
                }
                self.chunk
                    .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
            }
            Node::FunctionCall { name, args, .. } => {
                let rendered_args = args
                    .iter()
                    .map(attribute_value_repr)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                let idx = self.owned_string_constant(format!("{name}({rendered_args})"));
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

fn attribute_value_repr(node: &SNode) -> Result<String, CompileError> {
    match &node.node {
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => Ok(format!("{s:?}")),
        Node::IntLiteral(i) => Ok(i.to_string()),
        Node::FloatLiteral(f) => Ok(f.to_string()),
        Node::BoolLiteral(b) => Ok(b.to_string()),
        Node::NilLiteral => Ok("nil".to_string()),
        Node::Identifier(name) => Ok(name.clone()),
        Node::ListLiteral(items) => {
            let items = items
                .iter()
                .map(attribute_value_repr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            Ok(format!("[{items}]"))
        }
        Node::DictLiteral(entries) => {
            let entries = entries
                .iter()
                .map(|entry| {
                    Ok(format!(
                        "{}: {}",
                        attribute_value_repr(&entry.key)?,
                        attribute_value_repr(&entry.value)?
                    ))
                })
                .collect::<Result<Vec<_>, CompileError>>()?
                .join(", ");
            Ok(format!("{{{entries}}}"))
        }
        Node::FunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(attribute_value_repr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            Ok(format!("{name}({args})"))
        }
        _ => Err(CompileError {
            message: "attribute argument must be a literal value".into(),
            line: node.span.line as u32,
        }),
    }
}
