use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::Arc;

use harn_parser::{Node, SNode, ShapeField, TypeExpr, TypedParam};

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};
use crate::value::VmValue;

use super::error::CompileError;
use super::yield_scan::body_contains_yield;
use super::{peel_node, Compiler, CompilerOptions, FinallyEntry};

#[cfg(test)]
thread_local! {
    /// Test-only override for the value-discarding classification used by
    /// [`Compiler::compile_discarded_stmt`]. Setting it forces a
    /// `produces_value` answer regardless of the node, letting tests
    /// deliberately miswire the classification and prove the #2622 balance
    /// assertion fires (see
    /// `compiler::tests::miswired_produces_value_trips_balance_assertion`).
    pub(super) static FORCE_DISCARDED_PRODUCES_VALUE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

impl Compiler {
    pub fn new() -> Self {
        Self::with_options(CompilerOptions::from_env())
    }

    pub fn with_options(options: CompilerOptions) -> Self {
        Self {
            options,
            chunk: Chunk::new(),
            line: 1,
            column: 1,
            enum_names: std::collections::HashSet::new(),
            enum_variant_owners: std::collections::HashMap::new(),
            struct_layouts: std::collections::HashMap::new(),
            interface_methods: std::collections::HashMap::new(),
            loop_stack: Vec::new(),
            handler_depth: 0,
            finally_bodies: Vec::new(),
            temp_counter: 0,
            scope_depth: 0,
            type_aliases: std::collections::HashMap::new(),
            type_scopes: vec![std::collections::HashMap::new()],
            monomorphic_bindings: std::collections::HashSet::new(),
            string_constants: std::collections::HashMap::new(),
            local_scopes: vec![std::collections::HashMap::new()],
            module_level: true,
            captured_bindings: std::collections::HashSet::new(),
        }
    }

    /// Compiler instance for a nested function-like body (fn, closure,
    /// tool, parallel arm, etc.). Differs from `new()` only in that
    /// `module_level` starts false — `try*` is allowed inside.
    pub(super) fn for_nested_body(options: CompilerOptions) -> Self {
        let mut c = Self::with_options(options);
        c.module_level = false;
        c
    }

    pub(super) fn nested_body(&self) -> Self {
        Self::for_nested_body(self.options)
    }

    pub(super) fn nominal_type_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .struct_layouts
            .keys()
            .chain(self.enum_names.iter())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub(super) fn string_constant(&mut self, value: &str) -> u16 {
        if let Some(idx) = self.string_constants.get(value) {
            return *idx;
        }
        let owned = value.to_string();
        let idx = self.chunk.add_constant(Constant::String(owned.clone()));
        self.string_constants.insert(owned, idx);
        idx
    }

    pub(super) fn owned_string_constant(&mut self, value: String) -> u16 {
        if let Some(idx) = self.string_constants.get(value.as_str()) {
            return *idx;
        }
        let idx = self.chunk.add_constant(Constant::String(value.clone()));
        self.string_constants.insert(value, idx);
        idx
    }

    /// Populate `type_aliases` from a program's top-level `type T = ...`
    /// declarations so later lowerings can resolve alias names to their
    /// canonical `TypeExpr`.
    pub(crate) fn collect_type_aliases(&mut self, program: &[SNode]) {
        for sn in program {
            if let Node::TypeDecl {
                name,
                type_expr,
                type_params: _,
                is_pub: _,
            } = peel_node(sn)
            {
                self.type_aliases.insert(name.clone(), type_expr.clone());
            }
        }
    }

    /// Fully expand alias references, inlining every `Named(T)` whose `T` is a
    /// known alias with the alias's body. A `visiting` set breaks recursive
    /// aliases (`type Tree = {value: int, children: [Tree]}`): once an alias is
    /// already being expanded on the current path, the self-reference is left
    /// as an unexpanded `Named(T)` instead of recursing forever. This mirrors
    /// the typechecker's `resolve_alias` cycle guard so both sides agree, and
    /// keeps schema lowering (`type_expr_to_schema_value`) finite — a
    /// cycle-broken `Named(T)` lowers to no runtime constraint at that nested
    /// position rather than overflowing the stack.
    pub(crate) fn expand_alias(&self, ty: &TypeExpr) -> TypeExpr {
        let mut visiting = std::collections::HashSet::new();
        self.expand_alias_inner(ty, &mut visiting)
    }

    fn expand_alias_inner(
        &self,
        ty: &TypeExpr,
        visiting: &mut std::collections::HashSet<String>,
    ) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => {
                if let Some(target) = self.type_aliases.get(name) {
                    if !visiting.insert(name.clone()) {
                        return TypeExpr::Named(name.clone());
                    }
                    let resolved = self.expand_alias_inner(target, visiting);
                    visiting.remove(name);
                    resolved
                } else {
                    TypeExpr::Named(name.clone())
                }
            }
            TypeExpr::Union(types) => TypeExpr::Union(
                types
                    .iter()
                    .map(|t| self.expand_alias_inner(t, visiting))
                    .collect(),
            ),
            TypeExpr::Intersection(types) => TypeExpr::Intersection(
                types
                    .iter()
                    .map(|t| self.expand_alias_inner(t, visiting))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.expand_alias_inner(&field.type_expr, visiting),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            TypeExpr::OpenShape { fields, rests } => TypeExpr::OpenShape {
                fields: fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.expand_alias_inner(&field.type_expr, visiting),
                        optional: field.optional,
                    })
                    .collect(),
                rests: rests
                    .iter()
                    .map(|r| self.expand_alias_inner(r, visiting))
                    .collect(),
            },
            TypeExpr::List(inner) => {
                TypeExpr::List(Box::new(self.expand_alias_inner(inner, visiting)))
            }
            TypeExpr::Iter(inner) => {
                TypeExpr::Iter(Box::new(self.expand_alias_inner(inner, visiting)))
            }
            TypeExpr::Generator(inner) => {
                TypeExpr::Generator(Box::new(self.expand_alias_inner(inner, visiting)))
            }
            TypeExpr::Stream(inner) => {
                TypeExpr::Stream(Box::new(self.expand_alias_inner(inner, visiting)))
            }
            TypeExpr::DictType(k, v) => TypeExpr::DictType(
                Box::new(self.expand_alias_inner(k, visiting)),
                Box::new(self.expand_alias_inner(v, visiting)),
            ),
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|p| self.expand_alias_inner(p, visiting))
                    .collect(),
                return_type: Box::new(self.expand_alias_inner(return_type, visiting)),
            },
            TypeExpr::Applied { name, args } => TypeExpr::Applied {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| self.expand_alias_inner(a, visiting))
                    .collect(),
            },
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(s) => TypeExpr::LitString(s.clone()),
            TypeExpr::LitInt(v) => TypeExpr::LitInt(*v),
            TypeExpr::Owned(inner) => {
                TypeExpr::Owned(Box::new(self.expand_alias_inner(inner, visiting)))
            }
        }
    }

    /// Build the JSON-Schema VmValue for a named type alias, or `None` if
    /// the name is unknown or the alias cannot be lowered to a schema.
    pub(super) fn schema_value_for_alias(&self, name: &str) -> Option<VmValue> {
        let ty = self.type_aliases.get(name)?;
        let expanded = self.expand_alias(ty);
        Self::type_expr_to_schema_value(&expanded)
    }

    /// Lower every `pub type` alias in `program` to its JSON-Schema VmValue.
    /// Used by the module artifact so importers get the same schema value in
    /// expression position (`output_schema: ImportedAlias`) that a local
    /// alias would lower to at compile time. Aliases whose bodies cannot be
    /// expressed as a JSON schema (function types, streams, ...) are omitted:
    /// they stay importable for annotations, just not as runtime schemas.
    pub fn lower_public_type_schemas(
        program: &[SNode],
    ) -> std::collections::BTreeMap<String, VmValue> {
        let mut compiler = Compiler::new();
        compiler.collect_type_aliases(program);
        let mut schemas = std::collections::BTreeMap::new();
        for sn in program {
            let inner = peel_node(sn);
            if let Node::TypeDecl {
                name, is_pub: true, ..
            } = inner
            {
                if let Some(schema) = compiler.schema_value_for_alias(name) {
                    schemas.insert(name.clone(), schema);
                }
            }
        }
        schemas
    }

    /// Schema-guard builtins that accept a schema as their second argument.
    /// When callers pass a type-alias identifier here, the compiler lowers
    /// it to the alias's JSON-Schema dict constant.
    pub(super) fn is_schema_guard(name: &str) -> bool {
        matches!(
            name,
            "schema_is"
                | "schema_expect"
                | "schema_parse"
                | "schema_check"
                | "schema_report"
                | "is_type"
                | "json_validate"
        )
    }

    /// Check whether a dict-literal key node matches the given keyword
    /// (identifier or string literal form).
    pub(super) fn entry_key_is(key: &SNode, keyword: &str) -> bool {
        matches!(
            &key.node,
            Node::Identifier(name) | Node::StringLiteral(name) | Node::RawStringLiteral(name)
                if name == keyword
        )
    }

    /// Compile a program (list of top-level nodes) into a Chunk.
    /// Finds the entry pipeline and compiles its body, including inherited bodies.
    pub fn compile(mut self, program: &[SNode]) -> Result<Chunk, CompileError> {
        // Pre-scan so we can recognize EnumName.Variant as enum construction
        // even when the enum is declared inside a pipeline.
        Self::collect_enum_names(program, &mut self.enum_names);
        self.enum_names.insert("Result".to_string());
        Self::collect_enum_variant_owners(program, &mut self.enum_variant_owners);
        Self::seed_builtin_variant_owners(&mut self.enum_variant_owners);
        Self::collect_struct_layouts(program, &mut self.struct_layouts);
        Self::collect_interface_methods(program, &mut self.interface_methods);
        self.collect_type_aliases(program);
        // Box module-level mutable `let`s that a top-level or pipeline-body
        // closure captures (harn#4479). Nested `fn`/closure/`tool` bodies reseed
        // their own capture set when compiled, so this only governs the
        // module-level bindings emitted by `self`.
        self.seed_captured_idents(program);

        for sn in program {
            match &sn.node {
                Node::ImportDecl { .. } | Node::SelectiveImport { .. } => {
                    self.compile_node(sn)?;
                }
                _ => {}
            }
        }
        let main = program
            .iter()
            .find(|sn| matches!(peel_node(sn), Node::Pipeline { name, .. } if name == "default"))
            .or_else(|| {
                program
                    .iter()
                    .find(|sn| matches!(peel_node(sn), Node::Pipeline { .. }))
            });

        // When a pipeline body produces a final value, that value flows
        // out of `vm.execute()` so the CLI can map it to a process exit
        // code (int → exit n, Result::Err(msg) → stderr+exit 1).
        let mut pipeline_emits_value = false;
        if let Some(sn) = main {
            self.compile_top_level_declarations(program)?;
            if let Node::Pipeline { body, extends, .. } = peel_node(sn) {
                self.compile_with_pipeline_captures(
                    program,
                    body,
                    extends.as_deref(),
                    |compiler| {
                        if let Some(parent_name) = extends {
                            compiler.compile_parent_pipeline(program, parent_name)?;
                        }
                        let saved = std::mem::replace(&mut compiler.module_level, false);
                        let result = compiler.compile_block(body);
                        compiler.module_level = saved;
                        result
                    },
                )?;
                pipeline_emits_value = true;
            }
        } else {
            // Script mode: no pipeline found, treat top-level as implicit entry.
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
                self.compile_discarded_stmt(sn)?;
            }
            // E4.1 entrypoint convention: a top-level `fn main(harness: Harness)`
            // is invoked automatically with the runtime-provided `harness`
            // global. The typechecker rejects every other signature with
            // HARN-NAM-101 so we don't need to re-validate the shape here.
            if Self::has_top_level_fn_main(program) {
                let harness_name = self.string_constant("harness");
                self.chunk.emit_u16(Op::GetVar, harness_name, self.line);
                self.emit_named_call("main", 1);
                pipeline_emits_value = true;
            }
        }

        self.drain_finallys_to_floor(0)?;
        if !pipeline_emits_value {
            self.chunk.emit(Op::Nil, self.line);
        }
        self.chunk.emit(Op::Return, self.line);
        super::ensure_chunk_addressable(&self.chunk, "the program body", self.line)?;
        Ok(self.chunk)
    }

    /// True when the program declares a top-level `fn main(...)`. Drives the
    /// auto-call wired by `compile()` for the new `main(harness: Harness)`
    /// entrypoint convention.
    fn has_top_level_fn_main(program: &[SNode]) -> bool {
        program
            .iter()
            .any(|sn| matches!(peel_node(sn), Node::FnDecl { name, .. } if name == "main"))
    }

    /// Compile a specific named pipeline (for test runners).
    pub fn compile_named(
        self,
        program: &[SNode],
        pipeline_name: &str,
    ) -> Result<Chunk, CompileError> {
        self.compile_named_inner(program, pipeline_name, false)
    }

    /// Compile a named pipeline and materialize its parameters from VM globals.
    ///
    /// This is for hosts that supply one binding set out-of-band, such as the
    /// CLI test runner's `@test(cases: ...)` rows. Plain `compile_named` keeps
    /// the historical behavior where unused pipeline parameters do not require
    /// ambient globals.
    pub fn compile_named_with_param_globals(
        self,
        program: &[SNode],
        pipeline_name: &str,
    ) -> Result<Chunk, CompileError> {
        self.compile_named_inner(program, pipeline_name, true)
    }

    fn compile_named_inner(
        mut self,
        program: &[SNode],
        pipeline_name: &str,
        bind_params_from_globals: bool,
    ) -> Result<Chunk, CompileError> {
        Self::collect_enum_names(program, &mut self.enum_names);
        self.enum_names.insert("Result".to_string());
        Self::collect_enum_variant_owners(program, &mut self.enum_variant_owners);
        Self::seed_builtin_variant_owners(&mut self.enum_variant_owners);
        Self::collect_struct_layouts(program, &mut self.struct_layouts);
        Self::collect_interface_methods(program, &mut self.interface_methods);
        self.collect_type_aliases(program);
        // Box module-level mutable `let`s that a top-level or pipeline-body
        // closure captures (harn#4479). Nested `fn`/closure/`tool` bodies reseed
        // their own capture set when compiled, so this only governs the
        // module-level bindings emitted by `self`.
        self.seed_captured_idents(program);

        for sn in program {
            if matches!(
                &sn.node,
                Node::ImportDecl { .. } | Node::SelectiveImport { .. }
            ) {
                self.compile_node(sn)?;
            }
        }
        let target = program.iter().find(
            |sn| matches!(peel_node(sn), Node::Pipeline { name, .. } if name == pipeline_name),
        );

        if let Some(sn) = target {
            self.compile_top_level_declarations(program)?;
            if let Node::Pipeline {
                body,
                extends,
                params,
                ..
            } = peel_node(sn)
            {
                self.compile_with_pipeline_captures(
                    program,
                    body,
                    extends.as_deref(),
                    |compiler| {
                        if let Some(parent_name) = extends {
                            compiler.compile_parent_pipeline(program, parent_name)?;
                        }
                        let saved = std::mem::replace(&mut compiler.module_level, false);
                        if bind_params_from_globals {
                            for param in params {
                                compiler.define_local_slot(param, false);
                                let idx = compiler.string_constant(param);
                                compiler.chunk.emit_u16(Op::GetVar, idx, compiler.line);
                                compiler.emit_init_or_define_binding(param, false);
                            }
                        }
                        let result = compiler.compile_block(body);
                        compiler.module_level = saved;
                        result
                    },
                )?;
            }
        }

        self.drain_finallys_to_floor(0)?;
        self.chunk.emit(Op::Nil, self.line);
        self.chunk.emit(Op::Return, self.line);
        super::ensure_chunk_addressable(&self.chunk, "the pipeline body", self.line)?;
        Ok(self.chunk)
    }

    /// Emit bytecode preamble for default parameter values.
    /// For each param with a default at index i, emits:
    ///   GetArgc; PushInt (i+1); GreaterEqual; JumpIfTrue <skip>;
    ///   [compile default expr]; DefLet param_name; <skip>:
    pub(super) fn emit_default_preamble(
        &mut self,
        params: &[TypedParam],
    ) -> Result<(), CompileError> {
        for (i, param) in params.iter().enumerate() {
            if let Some(default_expr) = &param.default_value {
                self.chunk.emit(Op::GetArgc, self.line);
                let threshold_idx = self.chunk.add_constant(Constant::Int((i + 1) as i64));
                self.chunk.emit_u16(Op::Constant, threshold_idx, self.line);
                self.chunk.emit(Op::GreaterEqual, self.line);
                let skip_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                // JumpIfTrue doesn't pop its boolean operand.
                self.chunk.emit(Op::Pop, self.line);
                // Compile the default with this param and all *later* params
                // hidden from local resolution. A default is evaluated left to
                // right at call time: it may reference an earlier parameter,
                // but a mention of its own name (or a later, not-yet-bound
                // parameter) must resolve to the enclosing scope — e.g.
                // `let n = 7; fn f(n = n * 2)` reads the outer `n`. Without the
                // mask, `n` bound to the param's own unset slot and threw at
                // runtime. Earlier params stay visible.
                let masked = self.mask_param_names(&params[i..]);
                let result = self.compile_node(default_expr);
                self.restore_param_names(masked);
                result?;
                self.emit_init_or_define_binding(&param.name, false);
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(skip_jump);
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.patch_jump(end_jump);
            }
        }
        Ok(())
    }

    /// Emit body-local type checks that call-site validation cannot cover.
    /// Ordinary supplied arguments are validated by precomputed
    /// [`crate::chunk::ParamSlot`] guards before the frame is entered. The
    /// bytecode preamble still checks interface parameters, because interface
    /// satisfaction depends on compiler-collected method metadata, and checks
    /// defaulted schema parameters only when the caller omitted that argument.
    pub(super) fn emit_type_checks(&mut self, params: &[TypedParam]) {
        for (param_index, param) in params.iter().enumerate() {
            if let Some(type_expr) = &param.type_expr {
                let check_type = if param.rest {
                    harn_parser::TypeExpr::List(Box::new(type_expr.clone()))
                } else {
                    type_expr.clone()
                };

                if let harn_parser::TypeExpr::Named(name) = &check_type {
                    if let Some(methods) = self.interface_methods.get(name).cloned() {
                        let fn_idx = self.string_constant("__assert_interface");
                        self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                        self.emit_get_binding(&param.name);
                        let name_idx = self.string_constant(&param.name);
                        self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                        let iface_idx = self.string_constant(name);
                        self.chunk.emit_u16(Op::Constant, iface_idx, self.line);
                        let methods_str = methods.join(",");
                        let methods_idx = self.owned_string_constant(methods_str);
                        self.chunk.emit_u16(Op::Constant, methods_idx, self.line);
                        self.chunk.emit_u8(Op::Call, 4, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        continue;
                    }
                }

                if param.default_value.is_some() {
                    if let Some(schema) = Self::type_expr_to_schema_value(&check_type) {
                        self.emit_default_param_schema_check(param_index, param, &schema);
                    }
                }
            }
        }
    }

    fn emit_default_param_schema_check(
        &mut self,
        param_index: usize,
        param: &TypedParam,
        schema: &VmValue,
    ) {
        self.chunk.emit(Op::GetArgc, self.line);
        let threshold_idx = self
            .chunk
            .add_constant(Constant::Int((param_index + 1) as i64));
        self.chunk.emit_u16(Op::Constant, threshold_idx, self.line);
        self.chunk.emit(Op::GreaterEqual, self.line);
        let supplied_jump = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
        self.chunk.emit(Op::Pop, self.line);
        self.emit_schema_assert_call(param, schema);
        let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
        self.chunk.patch_jump(supplied_jump);
        self.chunk.emit(Op::Pop, self.line);
        self.chunk.patch_jump(end_jump);
    }

    fn emit_schema_assert_call(&mut self, param: &TypedParam, schema: &VmValue) {
        let fn_idx = self.string_constant("__assert_schema");
        self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
        self.emit_get_binding(&param.name);
        let name_idx = self.string_constant(&param.name);
        self.chunk.emit_u16(Op::Constant, name_idx, self.line);
        self.emit_vm_value_literal(schema);
        self.chunk.emit_u8(Op::Call, 3, self.line);
        self.chunk.emit(Op::Pop, self.line);
    }

    pub(crate) fn type_expr_to_schema_value(type_expr: &harn_parser::TypeExpr) -> Option<VmValue> {
        match type_expr {
            harn_parser::TypeExpr::Named(name) => match name.as_str() {
                "int" | "float" | "string" | "bool" | "list" | "dict" | "set" | "nil"
                | "closure" | "bytes" => Some(VmValue::dict(BTreeMap::from([(
                    "type".to_string(),
                    VmValue::String(arcstr::ArcStr::from(name.as_str())),
                )]))),
                _ => None,
            },
            harn_parser::TypeExpr::Shape(fields)
            | harn_parser::TypeExpr::OpenShape { fields, .. } => {
                let mut properties = BTreeMap::new();
                let mut required = Vec::new();
                for field in fields {
                    let field_schema = Self::type_expr_to_schema_value(&field.type_expr)?;
                    properties.insert(field.name.clone(), field_schema);
                    if !field.optional {
                        required.push(VmValue::String(arcstr::ArcStr::from(field.name.as_str())));
                    }
                }
                let mut out = BTreeMap::new();
                out.put_str("type", "dict");
                out.insert("properties".to_string(), VmValue::dict(properties));
                if !required.is_empty() {
                    out.insert(
                        "required".to_string(),
                        VmValue::List(std::sync::Arc::new(required)),
                    );
                }
                Some(VmValue::dict(out))
            }
            harn_parser::TypeExpr::List(inner) => {
                let mut out = BTreeMap::new();
                out.put_str("type", "list");
                if let Some(item_schema) = Self::type_expr_to_schema_value(inner) {
                    out.insert("items".to_string(), item_schema);
                }
                Some(VmValue::dict(out))
            }
            harn_parser::TypeExpr::DictType(key, value) => {
                let mut out = BTreeMap::new();
                out.put_str("type", "dict");
                if matches!(key.as_ref(), harn_parser::TypeExpr::Named(name) if name == "string") {
                    if let Some(value_schema) = Self::type_expr_to_schema_value(value) {
                        out.insert("additional_properties".to_string(), value_schema);
                    }
                }
                Some(VmValue::dict(out))
            }
            harn_parser::TypeExpr::Union(members) => {
                // Special-case unions of literals: emit as `enum: [...]`
                // so the schema round-trips as canonical JSON Schema and
                // is ACP-/OpenAPI-compatible. Mixed unions fall back to
                // the `union:` key that validators recognize.
                if !members.is_empty()
                    && members
                        .iter()
                        .all(|m| matches!(m, harn_parser::TypeExpr::LitString(_)))
                {
                    let values = members
                        .iter()
                        .map(|m| match m {
                            harn_parser::TypeExpr::LitString(s) => {
                                VmValue::String(arcstr::ArcStr::from(s.as_str()))
                            }
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>();
                    return Some(VmValue::dict(BTreeMap::from([
                        (
                            "type".to_string(),
                            VmValue::String(arcstr::ArcStr::from("string")),
                        ),
                        (
                            "enum".to_string(),
                            VmValue::List(std::sync::Arc::new(values)),
                        ),
                    ])));
                }
                if !members.is_empty()
                    && members
                        .iter()
                        .all(|m| matches!(m, harn_parser::TypeExpr::LitInt(_)))
                {
                    let values = members
                        .iter()
                        .map(|m| match m {
                            harn_parser::TypeExpr::LitInt(v) => VmValue::Int(*v),
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>();
                    return Some(VmValue::dict(BTreeMap::from([
                        (
                            "type".to_string(),
                            VmValue::String(arcstr::ArcStr::from("int")),
                        ),
                        (
                            "enum".to_string(),
                            VmValue::List(std::sync::Arc::new(values)),
                        ),
                    ])));
                }
                let branches = members
                    .iter()
                    .map(Self::type_expr_to_schema_value)
                    .collect::<Option<Vec<_>>>()?;
                if branches.is_empty() {
                    None
                } else {
                    Some(VmValue::dict(BTreeMap::from([(
                        "union".to_string(),
                        VmValue::List(std::sync::Arc::new(branches)),
                    )])))
                }
            }
            harn_parser::TypeExpr::Intersection(members) => {
                // Encode `A & B` as JSON-Schema `allOf` (the runtime
                // accepts the snake_case `all_of` key directly). The
                // value must validate against every branch.
                let branches = members
                    .iter()
                    .map(Self::type_expr_to_schema_value)
                    .collect::<Option<Vec<_>>>()?;
                if branches.is_empty() {
                    None
                } else {
                    Some(VmValue::dict(BTreeMap::from([(
                        "all_of".to_string(),
                        VmValue::List(std::sync::Arc::new(branches)),
                    )])))
                }
            }
            harn_parser::TypeExpr::FnType { .. } => Some(VmValue::dict(BTreeMap::from([(
                "type".to_string(),
                VmValue::String(arcstr::ArcStr::from("closure")),
            )]))),
            harn_parser::TypeExpr::Applied { .. } => None,
            harn_parser::TypeExpr::Iter(_)
            | harn_parser::TypeExpr::Generator(_)
            | harn_parser::TypeExpr::Stream(_) => None,
            harn_parser::TypeExpr::Never => None,
            harn_parser::TypeExpr::LitString(s) => Some(VmValue::dict(BTreeMap::from([
                (
                    "type".to_string(),
                    VmValue::String(arcstr::ArcStr::from("string")),
                ),
                (
                    "const".to_string(),
                    VmValue::String(arcstr::ArcStr::from(s.as_str())),
                ),
            ]))),
            harn_parser::TypeExpr::LitInt(v) => Some(VmValue::dict(BTreeMap::from([
                (
                    "type".to_string(),
                    VmValue::String(arcstr::ArcStr::from("int")),
                ),
                ("const".to_string(), VmValue::Int(*v)),
            ]))),
            harn_parser::TypeExpr::Owned(inner) => Self::type_expr_to_schema_value(inner),
        }
    }

    pub(super) fn emit_vm_value_literal(&mut self, value: &VmValue) {
        match value {
            VmValue::String(text) => {
                let idx = self.string_constant(text);
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            VmValue::Int(number) => {
                let idx = self.chunk.add_constant(Constant::Int(*number));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            VmValue::Float(number) => {
                let idx = self.chunk.add_constant(Constant::Float(*number));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            VmValue::Bool(value) => {
                let idx = self.chunk.add_constant(Constant::Bool(*value));
                self.chunk.emit_u16(Op::Constant, idx, self.line);
            }
            VmValue::Nil => self.chunk.emit(Op::Nil, self.line),
            VmValue::List(items) => {
                for item in items.iter() {
                    self.emit_vm_value_literal(item);
                }
                self.chunk
                    .emit_u16(Op::BuildList, items.len() as u16, self.line);
            }
            VmValue::Dict(entries) => {
                for (key, item) in entries.iter() {
                    let key_idx = self.string_constant(key);
                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    self.emit_vm_value_literal(item);
                }
                self.chunk
                    .emit_u16(Op::BuildDict, entries.len() as u16, self.line);
            }
            _ => {}
        }
    }

    /// Emit the extra u16 type name index after a TryCatchSetup jump.
    pub(super) fn emit_type_name_extra(&mut self, type_name_idx: u16) {
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
    pub(super) fn compile_try_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        if body.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_scoped_block(body)?;
        }
        Ok(())
    }

    /// Compile catch error binding (error value is on stack from handler).
    pub(super) fn compile_catch_binding(
        &mut self,
        error_var: &Option<String>,
    ) -> Result<(), CompileError> {
        if let Some(var_name) = error_var {
            self.emit_define_binding(var_name, false);
        } else {
            self.chunk.emit(Op::Pop, self.line);
        }
        Ok(())
    }

    /// Compile finally body inline, discarding its result value.
    /// `compile_scoped_block` always leaves exactly one value on the stack
    /// (Nil for non-value tail statements), so the trailing Pop is
    /// unconditional — otherwise a finally ending in e.g. `x = x + 1`
    /// would leave a stray Nil that corrupts the surrounding expression
    /// when the enclosing try/finally is used in expression position.
    pub(super) fn compile_finally_inline(
        &mut self,
        finally_body: &[SNode],
    ) -> Result<(), CompileError> {
        if !finally_body.is_empty() {
            self.compile_scoped_block(finally_body)?;
            self.chunk.emit(Op::Pop, self.line);
        }
        Ok(())
    }

    /// Collect pending finally bodies from the top of the stack down to
    /// (but not including) the innermost `CatchBarrier`. Used by `throw`
    /// lowering: throws caught locally don't unwind past the catch, so
    /// finallys behind the barrier aren't on the throw's exit path.
    pub(super) fn pending_finallys_until_barrier(&self) -> Vec<Vec<SNode>> {
        let mut out = Vec::new();
        for entry in self.finally_bodies.iter().rev() {
            match entry {
                FinallyEntry::CatchBarrier => break,
                FinallyEntry::Finally(body) => out.push(body.clone()),
            }
        }
        out
    }

    /// True if there are any pending finally bodies (not just barriers).
    pub(super) fn has_pending_finally(&self) -> bool {
        self.finally_bodies
            .iter()
            .any(|e| matches!(e, FinallyEntry::Finally(_)))
    }

    /// Save a thrown value to a temp and rethrow without running finally.
    ///
    /// Historically this helper also invoked `compile_finally_inline` on the
    /// thrown path, but that produced observable double-runs: the
    /// `Node::ThrowStmt` lowering (below) already iterates `finally_bodies`
    /// and runs each pending finally inline *before* emitting `Op::Throw`, so
    /// a second run here fired the same side effects twice. Finally now runs
    /// exactly once — via the throw-emit path during unwinding.
    pub(super) fn compile_plain_rethrow(&mut self) -> Result<(), CompileError> {
        self.temp_counter += 1;
        let temp_name = format!("__finally_err_{}__", self.temp_counter);
        self.emit_define_binding(&temp_name, true);
        self.emit_get_binding(&temp_name);
        self.chunk.emit(Op::Throw, self.line);
        Ok(())
    }

    pub(super) fn declare_param_slots(&mut self, params: &[TypedParam]) {
        for param in params {
            self.define_local_slot(&param.name, false);
        }
    }

    /// Temporarily remove the given parameters' names from the innermost local
    /// scope so that, while compiling a default-value expression, references to
    /// them resolve to the enclosing scope instead of their not-yet-bound param
    /// slots. Returns the removed bindings so [`Self::restore_param_names`] can
    /// reinstate them afterward. See [`Self::emit_default_preamble`].
    fn mask_param_names(&mut self, params: &[TypedParam]) -> Vec<(String, super::LocalBinding)> {
        let mut removed = Vec::new();
        if let Some(scope) = self.local_scopes.last_mut() {
            for param in params {
                if let Some(binding) = scope.remove(&param.name) {
                    removed.push((param.name.clone(), binding));
                }
            }
        }
        removed
    }

    /// Reinstate parameter names removed by [`Self::mask_param_names`].
    fn restore_param_names(&mut self, removed: Vec<(String, super::LocalBinding)>) {
        if let Some(scope) = self.local_scopes.last_mut() {
            for (name, binding) in removed {
                scope.insert(name, binding);
            }
        }
    }

    /// Seed exact source bindings captured by nested callables in the body
    /// about to be compiled. Parser-owned lexical analysis accounts for
    /// parameters, patterns, blocks, loops, catches, selects, and nested
    /// callable boundaries before the VM decides whether to use `DefCell`.
    pub(super) fn seed_captured_idents(&mut self, body: &[SNode]) {
        let match_patterns = self.lexical_match_pattern_catalog();
        self.captured_bindings =
            harn_parser::lexical::captured_bindings_in_nested_callables(body, &match_patterns);
    }

    pub(super) fn lexical_match_pattern_catalog(
        &self,
    ) -> harn_parser::lexical::MatchPatternCatalog {
        harn_parser::lexical::MatchPatternCatalog::new(&self.enum_names, &self.enum_variant_owners)
    }

    /// Whether this mutable source binding must be boxed into a shared cell
    /// because a nested callable captures this exact declaration.
    #[inline]
    fn is_boxed_capture(&self, binding: &harn_parser::lexical::BindingId, mutable: bool) -> bool {
        mutable && self.captured_bindings.contains(binding)
    }

    fn define_local_slot(&mut self, name: &str, mutable: bool) -> Option<u16> {
        if self.module_level || harn_parser::is_discard_name(name) {
            return None;
        }
        let current = self.local_scopes.last_mut()?;
        if let Some(existing) = current.get_mut(name) {
            if existing.mutable || mutable {
                if mutable {
                    existing.mutable = true;
                    if let Some(info) = self.chunk.local_slots.get_mut(existing.slot as usize) {
                        info.mutable = true;
                    }
                }
                return Some(existing.slot);
            }
            return None;
        }
        let slot = self
            .chunk
            .add_local_slot(name.to_string(), mutable, self.scope_depth);
        current.insert(name.to_string(), super::LocalBinding { slot, mutable });
        Some(slot)
    }

    pub(super) fn resolve_local_slot(&self, name: &str) -> Option<super::LocalBinding> {
        if self.module_level {
            return None;
        }
        self.local_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(super) fn emit_get_binding(&mut self, name: &str) {
        if let Some(binding) = self.resolve_local_slot(name) {
            self.chunk
                .emit_u16(Op::GetLocalSlot, binding.slot, self.line);
        } else {
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::GetVar, idx, self.line);
        }
    }

    pub(super) fn emit_define_binding(&mut self, name: &str, mutable: bool) {
        if let Some(slot) = self.define_local_slot(name, mutable) {
            self.chunk.emit_u16(Op::DefLocalSlot, slot, self.line);
        } else {
            let idx = self.string_constant(name);
            let op = if mutable { Op::DefVar } else { Op::DefLet };
            self.chunk.emit_u16(op, idx, self.line);
        }
    }

    /// Define a binding parsed from source. Synthetic compiler temporaries use
    /// [`Self::emit_define_binding`] and are deliberately absent from lexical
    /// capture analysis.
    pub(super) fn emit_source_binding(
        &mut self,
        name: &str,
        mutable: bool,
        binding: harn_parser::lexical::BindingId,
    ) {
        if self.is_boxed_capture(&binding, mutable) {
            // Box a closure-captured mutable local into a shared cell. Runs
            // regardless of `module_level`: a captured top-level `let` needs the
            // same shared cell so a top-level closure observes its writes.
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::DefCell, idx, self.line);
        } else {
            self.emit_define_binding(name, mutable);
        }
    }

    pub(super) fn emit_init_or_define_binding(&mut self, name: &str, mutable: bool) {
        if let Some(binding) = self.resolve_local_slot(name) {
            self.chunk
                .emit_u16(Op::DefLocalSlot, binding.slot, self.line);
        } else {
            self.emit_define_binding(name, mutable);
        }
    }

    pub(super) fn emit_set_binding(&mut self, name: &str) {
        if let Some(binding) = self.resolve_local_slot(name) {
            let _ = binding.mutable;
            self.chunk
                .emit_u16(Op::SetLocalSlot, binding.slot, self.line);
        } else {
            let idx = self.string_constant(name);
            self.chunk.emit_u16(Op::SetVar, idx, self.line);
        }
    }

    pub(super) fn begin_scope(&mut self) {
        self.chunk.emit(Op::PushScope, self.line);
        self.scope_depth += 1;
        self.type_scopes.push(std::collections::HashMap::new());
        self.local_scopes.push(std::collections::HashMap::new());
    }

    pub(super) fn end_scope(&mut self) {
        if self.scope_depth > 0 {
            self.chunk.emit(Op::PopScope, self.line);
            self.scope_depth -= 1;
            self.type_scopes.pop();
            self.local_scopes.pop();
        }
    }

    /// Emit cleanup for an abrupt control-flow path without changing the
    /// compiler's lexical scope stacks for the source path that follows it.
    pub(super) fn emit_scope_unwind_to(&mut self, target_depth: usize) {
        for _ in target_depth..self.scope_depth {
            self.chunk.emit(Op::PopScope, self.line);
        }
    }

    pub(super) fn compile_scoped_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        let finally_floor = self.finally_bodies.len();
        if stmts.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(stmts)?;
        }
        self.drain_finallys_to_floor(finally_floor)?;
        self.end_scope();
        Ok(())
    }

    pub(super) fn compile_scoped_statements(
        &mut self,
        stmts: &[SNode],
    ) -> Result<(), CompileError> {
        self.begin_scope();
        self.record_monomorphic_var_bindings(stmts);
        let finally_floor = self.finally_bodies.len();
        for sn in stmts {
            self.compile_discarded_stmt(sn)?;
        }
        self.drain_finallys_to_floor(finally_floor)?;
        self.end_scope();
        Ok(())
    }

    /// Drain pending `defer` bodies down to a saved floor and run each inline
    /// in LIFO order. Each defer body is popped *before* its code is emitted so
    /// any `return` / `break` lowering inside the body sees the remaining
    /// pending defers (not itself).
    pub(super) fn drain_finallys_to_floor(&mut self, floor: usize) -> Result<(), CompileError> {
        while self.finally_bodies.len() > floor {
            let entry = self.finally_bodies.pop().expect("non-empty by guard");
            if let FinallyEntry::Finally(body) = entry {
                self.compile_finally_inline(&body)?;
            }
        }
        Ok(())
    }

    /// Run the pending finally/defer bodies a non-local transfer (`return`,
    /// `break`, `continue`) crosses on its way down to `floor`, innermost
    /// first, then restore the pending stack.
    ///
    /// Like [`Self::drain_finallys_to_floor`] each body is removed from the
    /// stack *before* it is inlined, so a `return`/`break`/`continue` inside a
    /// finally body runs only the finallys *outside* it instead of re-running
    /// the one it is in — which otherwise recursed forever at compile time and
    /// aborted the process with a stack overflow. Unlike that helper (used at
    /// scope exit), the stack is restored afterward because a transfer is a
    /// branch: the code the compiler emits after it still needs the pending
    /// finallys for the fall-through and sibling paths.
    pub(super) fn run_pending_finallys_for_transfer(
        &mut self,
        floor: usize,
    ) -> Result<(), CompileError> {
        if self.finally_bodies.len() <= floor {
            return Ok(());
        }
        let saved = self.finally_bodies[floor..].to_vec();
        let result = self.drain_finallys_to_floor(floor);
        self.finally_bodies.extend(saved);
        result
    }

    /// Like [`Self::run_pending_finallys_for_transfer`] but for a `throw`: run
    /// only the finallys between here and the innermost `CatchBarrier` (the
    /// ones the unwind actually crosses before a local `catch` halts it),
    /// masking each while it is inlined and restoring the stack afterward.
    pub(super) fn run_pending_finallys_until_barrier(&mut self) -> Result<(), CompileError> {
        let floor = self
            .finally_bodies
            .iter()
            .rposition(|e| matches!(e, FinallyEntry::CatchBarrier))
            .map(|i| i + 1)
            .unwrap_or(0);
        self.run_pending_finallys_for_transfer(floor)
    }

    /// Register an auto-drop defer for an `owned<T>` binding. The drop runs
    /// at scope exit alongside any user-written `defer { ... }` blocks (LIFO
    /// order) and on `return` / `break` / `continue` / `throw` via the
    /// existing finally-unwinding machinery.
    pub(super) fn maybe_register_owned_drop(
        &mut self,
        pattern: &harn_parser::BindingPattern,
        type_ann: Option<&TypeExpr>,
        span: harn_lexer::Span,
    ) {
        // Auto-drop only fires when the user explicitly opted in via
        // `owned<T>` on a single-identifier binding. Destructured patterns
        // (`{a, b}`, `[a, b]`, pairs) aren't auto-dropped: ownership of a
        // composite isn't well-defined, and users can wrap individual fields
        // with `owned<T>` and bind them separately if needed.
        let Some(ty) = type_ann else {
            return;
        };
        if !matches!(ty, TypeExpr::Owned(_)) {
            return;
        }
        let harn_parser::BindingPattern::Identifier(name) = pattern else {
            return;
        };
        if harn_parser::is_discard_name(name) {
            return;
        }
        let call = harn_parser::spanned(
            Node::FunctionCall {
                name: "drop".to_string(),
                args: vec![harn_parser::spanned(Node::Identifier(name.clone()), span)],
                type_args: Vec::new(),
            },
            span,
        );
        self.finally_bodies.push(FinallyEntry::Finally(vec![call]));
    }

    /// Compile a statement that appears in a value-discarding sequence —
    /// the script-mode module body, an inherited pipeline body, and block
    /// interiors — then pop its value when `produces_value` says it left
    /// one.
    ///
    /// In debug builds this also asserts the operand stack stayed balanced
    /// across the statement: a straight-line statement must net exactly one
    /// value when `produces_value` is true and zero otherwise. That turns a
    /// `produces_value` misclassification — like the attributed-decl gap
    /// fixed in #2610, where the loop popped against an empty stack — from a
    /// latent runtime "Stack underflow" (often masked further by the
    /// bytecode cache, #2621) into a loud compile-time failure in tests/CI.
    /// Statements containing branches or other non-linearly-modeled opcodes
    /// can't be summed by the lightweight model, so the assertion skips them
    /// (see [`Chunk::balance_delta_since`]).
    pub(super) fn compile_discarded_stmt(&mut self, sn: &SNode) -> Result<(), CompileError> {
        #[cfg(debug_assertions)]
        let probe = self.chunk.balance_probe();
        self.compile_node(sn)?;
        #[allow(unused_mut)]
        let mut produces = Self::produces_value(&sn.node);
        // Test-only hook: deliberately miswire the classification to prove
        // the balance assertion below trips on a `produces_value` gap (the
        // #2622 verification). No-op in non-test builds.
        #[cfg(test)]
        if let Some(forced) = FORCE_DISCARDED_PRODUCES_VALUE.with(std::cell::Cell::get) {
            produces = forced;
        }
        #[cfg(debug_assertions)]
        if let Some(delta) = self.chunk.balance_delta_since(probe) {
            let expected = i32::from(produces);
            debug_assert_eq!(
                delta, expected,
                "operand-stack imbalance at line {}: produces_value={produces} but the \
                 node's emitted bytecode netted {delta} (expected {expected}). A \
                 `produces_value` arm is out of sync with this node's codegen — see #2622.\n\
                 node: {:?}",
                self.line, sn.node,
            );
        }
        if produces {
            self.chunk.emit(Op::Pop, self.line);
        }
        Ok(())
    }

    pub(super) fn compile_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        self.record_monomorphic_var_bindings(stmts);
        for (i, snode) in stmts.iter().enumerate() {
            if i == stmts.len() - 1 {
                // The block's value is its last statement's. Backfill a `Nil`
                // when that statement produced none, so the block always
                // leaves exactly one value on the stack.
                self.compile_node(snode)?;
                if !Self::produces_value(&snode.node) {
                    self.chunk.emit(Op::Nil, self.line);
                }
            } else {
                self.compile_discarded_stmt(snode)?;
            }
        }
        Ok(())
    }

    /// Compile a match arm body, ensuring it always pushes exactly one value.
    pub(super) fn compile_match_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        let finally_floor = self.finally_bodies.len();
        if body.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(body)?;
            if !Self::produces_value(&body.last().unwrap().node) {
                self.chunk.emit(Op::Nil, self.line);
            }
        }
        self.drain_finallys_to_floor(finally_floor)?;
        self.end_scope();
        Ok(())
    }

    /// Emit the binary op instruction for a compound assignment operator.
    pub(super) fn emit_compound_op(&mut self, op: &str) -> Result<(), CompileError> {
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

    pub(super) fn compile_top_level_declarations(
        &mut self,
        program: &[SNode],
    ) -> Result<(), CompileError> {
        // Phase 1: execute module-level *statements* in source order —
        // bindings, assignments, expression statements, control flow. Running
        // bindings before phase 2 ensures function closures compiled there
        // capture these names in their env snapshot via `Op::Closure` —
        // fixing the "Undefined variable: FOO" surprise where a top-level
        // `let FOO = "..."` was silently dropped because it wasn't compiled
        // at all. Non-binding statements used to be silently dropped in
        // pipeline mode (`n = 2` or `log(...)` between a binding and a
        // pipeline simply never ran); they now execute exactly like script
        // mode. Keep in step with the import-time init path in
        // `crates/harn-vm/src/vm/imports.rs` (`module_state` construction).
        for sn in program {
            let handled_elsewhere = matches!(
                peel_node(sn),
                Node::Pipeline { .. }
                    | Node::ImportDecl { .. }
                    | Node::SelectiveImport { .. }
                    | Node::OverrideDecl { .. }
                    | Node::EvalPackDecl { .. }
                    | Node::FnDecl { .. }
                    | Node::ToolDecl { .. }
                    | Node::SkillDecl { .. }
                    | Node::ImplBlock { .. }
                    | Node::StructDecl { .. }
                    | Node::EnumDecl { .. }
                    | Node::InterfaceDecl { .. }
                    | Node::TypeDecl { .. }
            );
            if !handled_elsewhere {
                self.compile_discarded_stmt(sn)?;
            }
        }
        // Phase 2: compile function-like declarations. Function closures
        // created here capture the current env which now includes module-level
        // bindings from phase 1. Attributed declarations are compiled here too
        // — the AttributedDecl arm in compile_node dispatches to the inner
        // declaration's compile path.
        for sn in program {
            let inner_kind = match &sn.node {
                Node::AttributedDecl { inner, .. } => &inner.node,
                other => other,
            };
            match inner_kind {
                Node::EvalPackDecl {
                    binding_name,
                    pack_id,
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    self.compile_eval_pack_decl(
                        binding_name,
                        pack_id,
                        fields,
                        body,
                        summarize,
                        false,
                    )?;
                }
                Node::FnDecl { .. }
                | Node::ToolDecl { .. }
                | Node::SkillDecl { .. }
                | Node::ImplBlock { .. }
                | Node::StructDecl { .. }
                | Node::EnumDecl { .. }
                | Node::InterfaceDecl { .. } => {
                    self.compile_node(sn)?;
                }
                Node::TypeDecl { .. } => {}
                _ => {}
            }
        }
        Ok(())
    }

    /// Recursively collect all enum type names from the AST.
    pub(super) fn collect_enum_names(
        nodes: &[SNode],
        names: &mut std::collections::HashSet<String>,
    ) {
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
                Node::SkillDecl { fields, .. } => {
                    for (_k, v) in fields {
                        Self::collect_enum_names(std::slice::from_ref(v), names);
                    }
                }
                Node::EvalPackDecl {
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    for (_k, v) in fields {
                        Self::collect_enum_names(std::slice::from_ref(v), names);
                    }
                    Self::collect_enum_names(body, names);
                    if let Some(summary_body) = summarize {
                        Self::collect_enum_names(summary_body, names);
                    }
                }
                Node::Block(stmts) => {
                    Self::collect_enum_names(stmts, names);
                }
                Node::AttributedDecl { inner, .. } => {
                    Self::collect_enum_names(std::slice::from_ref(inner), names);
                }
                _ => {}
            }
        }
    }

    /// Collect variant name → owning enum names across the whole program
    /// (including nested declarations). Powers bare call-shaped match
    /// patterns (`Ok(v)` without the `Result.` qualifier): a pattern
    /// resolves only when exactly one visible enum owns the variant name.
    pub(super) fn collect_enum_variant_owners(
        nodes: &[SNode],
        owners: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        harn_parser::visit::walk_program(nodes, &mut |sn| {
            if let Node::EnumDecl { name, variants, .. } = &sn.node {
                for variant in variants {
                    let entry = owners.entry(variant.name.clone()).or_default();
                    if !entry.contains(name) {
                        entry.push(name.clone());
                    }
                }
            }
        });
    }

    /// Seed the built-in `Result` enum's variants into the owner map (the
    /// same special-casing `compile`/`compile_named` apply to `enum_names`).
    pub(super) fn seed_builtin_variant_owners(
        owners: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for variant in ["Ok", "Err"] {
            let entry = owners.entry(variant.to_string()).or_default();
            if !entry.contains(&"Result".to_string()) {
                entry.push("Result".to_string());
            }
        }
    }

    pub(super) fn collect_struct_layouts(
        nodes: &[SNode],
        layouts: &mut std::collections::HashMap<String, Vec<String>>,
    ) {
        for sn in nodes {
            match &sn.node {
                Node::StructDecl { name, fields, .. } => {
                    layouts.insert(
                        name.clone(),
                        fields.iter().map(|field| field.name.clone()).collect(),
                    );
                }
                Node::Pipeline { body, .. }
                | Node::FnDecl { body, .. }
                | Node::ToolDecl { body, .. } => {
                    Self::collect_struct_layouts(body, layouts);
                }
                Node::SkillDecl { fields, .. } => {
                    for (_k, v) in fields {
                        Self::collect_struct_layouts(std::slice::from_ref(v), layouts);
                    }
                }
                Node::EvalPackDecl {
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    for (_k, v) in fields {
                        Self::collect_struct_layouts(std::slice::from_ref(v), layouts);
                    }
                    Self::collect_struct_layouts(body, layouts);
                    if let Some(summary_body) = summarize {
                        Self::collect_struct_layouts(summary_body, layouts);
                    }
                }
                Node::Block(stmts) => {
                    Self::collect_struct_layouts(stmts, layouts);
                }
                Node::AttributedDecl { inner, .. } => {
                    Self::collect_struct_layouts(std::slice::from_ref(inner), layouts);
                }
                _ => {}
            }
        }
    }

    pub(super) fn collect_interface_methods(
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
                Node::SkillDecl { fields, .. } => {
                    for (_k, v) in fields {
                        Self::collect_interface_methods(std::slice::from_ref(v), interfaces);
                    }
                }
                Node::EvalPackDecl {
                    fields,
                    body,
                    summarize,
                    ..
                } => {
                    for (_k, v) in fields {
                        Self::collect_interface_methods(std::slice::from_ref(v), interfaces);
                    }
                    Self::collect_interface_methods(body, interfaces);
                    if let Some(summary_body) = summarize {
                        Self::collect_interface_methods(summary_body, interfaces);
                    }
                }
                Node::Block(stmts) => {
                    Self::collect_interface_methods(stmts, interfaces);
                }
                Node::AttributedDecl { inner, .. } => {
                    Self::collect_interface_methods(std::slice::from_ref(inner), interfaces);
                }
                _ => {}
            }
        }
    }

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
        type_params: &[harn_parser::TypeParam],
        params: &[TypedParam],
        body: &[SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
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
        fn_compiler.chunk.emit(Op::Nil, 0);
        fn_compiler.chunk.emit(Op::Return, 0);
        fn_compiler.chunk.source_file = source_file;
        let param_slots = crate::chunk::ParamSlot::vec_from_typed(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&fn_compiler.chunk, "function body", self.line)?;
        Ok(CompiledFunction {
            name: String::new(),
            type_params: type_params.iter().map(|param| param.name.clone()).collect(),
            nominal_type_names: fn_compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(fn_compiler.chunk),
            is_generator: is_gen,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks,
        })
    }

    /// Check if a node produces a value on the stack that needs to be popped.
    pub(super) fn produces_value(node: &Node) -> bool {
        match node {
            // An attribute decorates a declaration (fn/struct/enum/…), never
            // an expression — so an attributed top-level item is a statement
            // that leaves nothing on the operand stack, exactly like its bare
            // inner declaration. Classifying by the inner node prevents the
            // script-mode top-level loop from emitting a spurious `Pop` (which
            // underflows the stack) after compiling, e.g., a `@route pub fn`.
            Node::AttributedDecl { inner, .. } => Self::produces_value(&inner.node),
            Node::LetBinding { .. }
            | Node::ConstBinding { .. }
            | Node::Assignment { .. }
            | Node::ReturnStmt { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::SkillDecl { .. }
            | Node::EvalPackDecl { .. }
            | Node::ImplBlock { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            // Metadata-only declarations that emit no bytecode — see the
            // matching arm in `compile_node`.
            | Node::OverrideDecl { .. }
            | Node::Pipeline { .. }
            | Node::ThrowStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::RequireStmt { .. }
            | Node::DeferStmt { .. } => false,
            Node::TryCatch { has_catch: _, .. }
            | Node::TryExpr { .. }
            | Node::Retry { .. }
            | Node::GuardStmt { .. }
            | Node::DeadlineBlock { .. }
            | Node::MutexBlock { .. }
            | Node::Spread(_) => true,
            _ => true,
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}
