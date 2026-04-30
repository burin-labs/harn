use std::collections::BTreeMap;
use std::rc::Rc;

use harn_parser::{Node, SNode, ShapeField, TypeExpr, TypedParam};

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};
use crate::value::VmValue;

use super::error::CompileError;
use super::yield_scan::body_contains_yield;
use super::{peel_node, Compiler, FinallyEntry};

impl Compiler {
    pub fn new() -> Self {
        Self {
            chunk: Chunk::new(),
            line: 1,
            column: 1,
            enum_names: std::collections::HashSet::new(),
            struct_layouts: std::collections::HashMap::new(),
            interface_methods: std::collections::HashMap::new(),
            loop_stack: Vec::new(),
            handler_depth: 0,
            finally_bodies: Vec::new(),
            temp_counter: 0,
            scope_depth: 0,
            type_aliases: std::collections::HashMap::new(),
            type_scopes: vec![std::collections::HashMap::new()],
            local_scopes: vec![std::collections::HashMap::new()],
            module_level: true,
        }
    }

    /// Compiler instance for a nested function-like body (fn, closure,
    /// tool, parallel arm, etc.). Differs from `new()` only in that
    /// `module_level` starts false — `try*` is allowed inside.
    pub(super) fn for_nested_body() -> Self {
        let mut c = Self::new();
        c.module_level = false;
        c
    }

    /// Populate `type_aliases` from a program's top-level `type T = ...`
    /// declarations so later lowerings can resolve alias names to their
    /// canonical `TypeExpr`.
    pub(super) fn collect_type_aliases(&mut self, program: &[SNode]) {
        for sn in program {
            if let Node::TypeDecl {
                name,
                type_expr,
                type_params: _,
            } = &sn.node
            {
                self.type_aliases.insert(name.clone(), type_expr.clone());
            }
        }
    }

    /// Expand a single layer of alias references. Returns the resolved
    /// `TypeExpr` with all `Named(T)` nodes whose `T` is a known alias
    /// replaced by the alias's body.
    pub(super) fn expand_alias(&self, ty: &TypeExpr) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => {
                if let Some(target) = self.type_aliases.get(name) {
                    self.expand_alias(target)
                } else {
                    TypeExpr::Named(name.clone())
                }
            }
            TypeExpr::Union(types) => {
                TypeExpr::Union(types.iter().map(|t| self.expand_alias(t)).collect())
            }
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: self.expand_alias(&field.type_expr),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            TypeExpr::List(inner) => TypeExpr::List(Box::new(self.expand_alias(inner))),
            TypeExpr::Iter(inner) => TypeExpr::Iter(Box::new(self.expand_alias(inner))),
            TypeExpr::Generator(inner) => TypeExpr::Generator(Box::new(self.expand_alias(inner))),
            TypeExpr::Stream(inner) => TypeExpr::Stream(Box::new(self.expand_alias(inner))),
            TypeExpr::DictType(k, v) => TypeExpr::DictType(
                Box::new(self.expand_alias(k)),
                Box::new(self.expand_alias(v)),
            ),
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params.iter().map(|p| self.expand_alias(p)).collect(),
                return_type: Box::new(self.expand_alias(return_type)),
            },
            TypeExpr::Applied { name, args } => TypeExpr::Applied {
                name: name.clone(),
                args: args.iter().map(|a| self.expand_alias(a)).collect(),
            },
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(s) => TypeExpr::LitString(s.clone()),
            TypeExpr::LitInt(v) => TypeExpr::LitInt(*v),
        }
    }

    /// Build the JSON-Schema VmValue for a named type alias, or `None` if
    /// the name is unknown or the alias cannot be lowered to a schema.
    pub(super) fn schema_value_for_alias(&self, name: &str) -> Option<VmValue> {
        let ty = self.type_aliases.get(name)?;
        let expanded = self.expand_alias(ty);
        Self::type_expr_to_schema_value(&expanded)
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
        Self::collect_struct_layouts(program, &mut self.struct_layouts);
        Self::collect_interface_methods(program, &mut self.interface_methods);
        self.collect_type_aliases(program);

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
                if let Some(parent_name) = extends {
                    self.compile_parent_pipeline(program, parent_name)?;
                }
                let saved = std::mem::replace(&mut self.module_level, false);
                self.compile_block(body)?;
                self.module_level = saved;
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
                self.compile_node(sn)?;
                if Self::produces_value(&sn.node) {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }

        for fb in self.all_pending_finallys() {
            self.compile_finally_inline(&fb)?;
        }
        if !pipeline_emits_value {
            self.chunk.emit(Op::Nil, self.line);
        }
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
        self.enum_names.insert("Result".to_string());
        Self::collect_struct_layouts(program, &mut self.struct_layouts);
        Self::collect_interface_methods(program, &mut self.interface_methods);
        self.collect_type_aliases(program);

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
            if let Node::Pipeline { body, extends, .. } = peel_node(sn) {
                if let Some(parent_name) = extends {
                    self.compile_parent_pipeline(program, parent_name)?;
                }
                let saved = std::mem::replace(&mut self.module_level, false);
                self.compile_block(body)?;
                self.module_level = saved;
            }
        }

        for fb in self.all_pending_finallys() {
            self.compile_finally_inline(&fb)?;
        }
        self.chunk.emit(Op::Nil, self.line);
        self.chunk.emit(Op::Return, self.line);
        Ok(self.chunk)
    }

    /// Recursively compile parent pipeline bodies (for extends).
    pub(super) fn compile_parent_pipeline(
        &mut self,
        program: &[SNode],
        parent_name: &str,
    ) -> Result<(), CompileError> {
        let parent = program
            .iter()
            .find(|sn| matches!(&sn.node, Node::Pipeline { name, .. } if name == parent_name));
        if let Some(sn) = parent {
            if let Node::Pipeline { body, extends, .. } = &sn.node {
                if let Some(grandparent) = extends {
                    self.compile_parent_pipeline(program, grandparent)?;
                }
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
                self.compile_node(default_expr)?;
                self.emit_init_or_define_binding(&param.name, false);
                let end_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(skip_jump);
                self.chunk.emit(Op::Pop, self.line);
                self.chunk.patch_jump(end_jump);
            }
        }
        Ok(())
    }

    /// Emit runtime type checks for parameters with type annotations.
    /// Interface types keep their dedicated runtime guard; all other supported
    /// runtime-checkable types compile to a schema literal and call
    /// `__assert_schema(value, param_name, schema)`.
    pub(super) fn emit_type_checks(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                if let harn_parser::TypeExpr::Named(name) = type_expr {
                    if let Some(methods) = self.interface_methods.get(name).cloned() {
                        let fn_idx = self
                            .chunk
                            .add_constant(Constant::String("__assert_interface".into()));
                        self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                        self.emit_get_binding(&param.name);
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

                if let Some(schema) = Self::type_expr_to_schema_value(type_expr) {
                    let fn_idx = self
                        .chunk
                        .add_constant(Constant::String("__assert_schema".into()));
                    self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                    self.emit_get_binding(&param.name);
                    let name_idx = self
                        .chunk
                        .add_constant(Constant::String(param.name.clone()));
                    self.chunk.emit_u16(Op::Constant, name_idx, self.line);
                    self.emit_vm_value_literal(&schema);
                    self.chunk.emit_u8(Op::Call, 3, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
    }

    pub(crate) fn type_expr_to_schema_value(type_expr: &harn_parser::TypeExpr) -> Option<VmValue> {
        match type_expr {
            harn_parser::TypeExpr::Named(name) => match name.as_str() {
                "int" | "float" | "string" | "bool" | "list" | "dict" | "set" | "nil"
                | "closure" | "bytes" => Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                    "type".to_string(),
                    VmValue::String(Rc::from(name.as_str())),
                )])))),
                _ => None,
            },
            harn_parser::TypeExpr::Shape(fields) => {
                let mut properties = BTreeMap::new();
                let mut required = Vec::new();
                for field in fields {
                    let field_schema = Self::type_expr_to_schema_value(&field.type_expr)?;
                    properties.insert(field.name.clone(), field_schema);
                    if !field.optional {
                        required.push(VmValue::String(Rc::from(field.name.as_str())));
                    }
                }
                let mut out = BTreeMap::new();
                out.insert("type".to_string(), VmValue::String(Rc::from("dict")));
                out.insert("properties".to_string(), VmValue::Dict(Rc::new(properties)));
                if !required.is_empty() {
                    out.insert("required".to_string(), VmValue::List(Rc::new(required)));
                }
                Some(VmValue::Dict(Rc::new(out)))
            }
            harn_parser::TypeExpr::List(inner) => {
                let mut out = BTreeMap::new();
                out.insert("type".to_string(), VmValue::String(Rc::from("list")));
                if let Some(item_schema) = Self::type_expr_to_schema_value(inner) {
                    out.insert("items".to_string(), item_schema);
                }
                Some(VmValue::Dict(Rc::new(out)))
            }
            harn_parser::TypeExpr::DictType(key, value) => {
                let mut out = BTreeMap::new();
                out.insert("type".to_string(), VmValue::String(Rc::from("dict")));
                if matches!(key.as_ref(), harn_parser::TypeExpr::Named(name) if name == "string") {
                    if let Some(value_schema) = Self::type_expr_to_schema_value(value) {
                        out.insert("additional_properties".to_string(), value_schema);
                    }
                }
                Some(VmValue::Dict(Rc::new(out)))
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
                                VmValue::String(Rc::from(s.as_str()))
                            }
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>();
                    return Some(VmValue::Dict(Rc::new(BTreeMap::from([
                        ("type".to_string(), VmValue::String(Rc::from("string"))),
                        ("enum".to_string(), VmValue::List(Rc::new(values))),
                    ]))));
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
                    return Some(VmValue::Dict(Rc::new(BTreeMap::from([
                        ("type".to_string(), VmValue::String(Rc::from("int"))),
                        ("enum".to_string(), VmValue::List(Rc::new(values))),
                    ]))));
                }
                let branches = members
                    .iter()
                    .filter_map(Self::type_expr_to_schema_value)
                    .collect::<Vec<_>>();
                if branches.is_empty() {
                    None
                } else {
                    Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                        "union".to_string(),
                        VmValue::List(Rc::new(branches)),
                    )]))))
                }
            }
            harn_parser::TypeExpr::FnType { .. } => {
                Some(VmValue::Dict(Rc::new(BTreeMap::from([(
                    "type".to_string(),
                    VmValue::String(Rc::from("closure")),
                )]))))
            }
            harn_parser::TypeExpr::Applied { .. } => None,
            harn_parser::TypeExpr::Iter(_)
            | harn_parser::TypeExpr::Generator(_)
            | harn_parser::TypeExpr::Stream(_) => None,
            harn_parser::TypeExpr::Never => None,
            harn_parser::TypeExpr::LitString(s) => Some(VmValue::Dict(Rc::new(BTreeMap::from([
                ("type".to_string(), VmValue::String(Rc::from("string"))),
                ("const".to_string(), VmValue::String(Rc::from(s.as_str()))),
            ])))),
            harn_parser::TypeExpr::LitInt(v) => Some(VmValue::Dict(Rc::new(BTreeMap::from([
                ("type".to_string(), VmValue::String(Rc::from("int"))),
                ("const".to_string(), VmValue::Int(*v)),
            ])))),
        }
    }

    pub(super) fn emit_vm_value_literal(&mut self, value: &VmValue) {
        match value {
            VmValue::String(text) => {
                let idx = self.chunk.add_constant(Constant::String(text.to_string()));
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
                    let key_idx = self.chunk.add_constant(Constant::String(key.clone()));
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

    /// Collect every pending finally body from the top of the stack down
    /// to `floor` (an index produced by `finally_bodies.len()` at some
    /// earlier point), skipping `CatchBarrier` markers. Used by `return`,
    /// `break`, and `continue` lowering — they transfer control past local
    /// handlers, so every `Finally` up to their target must run.
    pub(super) fn pending_finallys_down_to(&self, floor: usize) -> Vec<Vec<SNode>> {
        let mut out = Vec::new();
        for entry in self.finally_bodies[floor..].iter().rev() {
            if let FinallyEntry::Finally(body) = entry {
                out.push(body.clone());
            }
        }
        out
    }

    /// All pending finally bodies (entire stack), skipping barriers.
    pub(super) fn all_pending_finallys(&self) -> Vec<Vec<SNode>> {
        self.pending_finallys_down_to(0)
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
            let idx = self.chunk.add_constant(Constant::String(name.to_string()));
            self.chunk.emit_u16(Op::GetVar, idx, self.line);
        }
    }

    pub(super) fn emit_define_binding(&mut self, name: &str, mutable: bool) {
        if let Some(slot) = self.define_local_slot(name, mutable) {
            self.chunk.emit_u16(Op::DefLocalSlot, slot, self.line);
        } else {
            let idx = self.chunk.add_constant(Constant::String(name.to_string()));
            let op = if mutable { Op::DefVar } else { Op::DefLet };
            self.chunk.emit_u16(op, idx, self.line);
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
            let idx = self.chunk.add_constant(Constant::String(name.to_string()));
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

    pub(super) fn unwind_scopes_to(&mut self, target_depth: usize) {
        while self.scope_depth > target_depth {
            self.chunk.emit(Op::PopScope, self.line);
            self.scope_depth -= 1;
            self.type_scopes.pop();
            self.local_scopes.pop();
        }
    }

    pub(super) fn compile_scoped_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        self.begin_scope();
        if stmts.is_empty() {
            self.chunk.emit(Op::Nil, self.line);
        } else {
            self.compile_block(stmts)?;
        }
        self.end_scope();
        Ok(())
    }

    pub(super) fn compile_scoped_statements(
        &mut self,
        stmts: &[SNode],
    ) -> Result<(), CompileError> {
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

    pub(super) fn compile_block(&mut self, stmts: &[SNode]) -> Result<(), CompileError> {
        for (i, snode) in stmts.iter().enumerate() {
            self.compile_node(snode)?;
            let is_last = i == stmts.len() - 1;
            if is_last {
                // Ensure the block always leaves exactly one value on the stack.
                if !Self::produces_value(&snode.node) {
                    self.chunk.emit(Op::Nil, self.line);
                }
            } else if Self::produces_value(&snode.node) {
                self.chunk.emit(Op::Pop, self.line);
            }
        }
        Ok(())
    }

    /// Compile a match arm body, ensuring it always pushes exactly one value.
    pub(super) fn compile_match_body(&mut self, body: &[SNode]) -> Result<(), CompileError> {
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

    /// Extract the root variable name from a (possibly nested) access expression.
    pub(super) fn root_var_name(&self, node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.root_var_name(object)
            }
            Node::SubscriptAccess { object, .. } | Node::OptionalSubscriptAccess { object, .. } => {
                self.root_var_name(object)
            }
            _ => None,
        }
    }

    pub(super) fn compile_top_level_declarations(
        &mut self,
        program: &[SNode],
    ) -> Result<(), CompileError> {
        // Phase 1: evaluate module-level `let` / `var` bindings first, in
        // source order. This ensures function closures compiled in phase 2
        // capture these names in their env snapshot via `Op::Closure` —
        // fixing the "Undefined variable: FOO" surprise where a top-level
        // `let FOO = "..."` was silently dropped because it wasn't in this
        // match list. Keep in step with the import-time init path in
        // `crates/harn-vm/src/vm/imports.rs` (`module_state` construction).
        for sn in program {
            if matches!(&sn.node, Node::LetBinding { .. } | Node::VarBinding { .. }) {
                self.compile_node(sn)?;
            }
        }
        // Phase 2: compile type and function declarations. Function closures
        // created here capture the current env which now includes the
        // module-level bindings from phase 1. Attributed declarations are
        // compiled here too — the AttributedDecl arm in compile_node
        // dispatches to the inner declaration's compile path.
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
                | Node::InterfaceDecl { .. }
                | Node::TypeDecl { .. } => {
                    self.compile_node(sn)?;
                }
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
        params: &[TypedParam],
        body: &[SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.struct_layouts = self.struct_layouts.clone();
        fn_compiler.declare_param_slots(params);
        fn_compiler.record_param_types(params);
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
            chunk: Rc::new(fn_compiler.chunk),
            is_generator: is_gen,
            is_stream: false,
            has_rest_param: false,
        })
    }

    /// Check if a node produces a value on the stack that needs to be popped.
    pub(super) fn produces_value(node: &Node) -> bool {
        match node {
            Node::LetBinding { .. }
            | Node::VarBinding { .. }
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
            | Node::ThrowStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::RequireStmt { .. }
            | Node::DeferStmt { .. } => false,
            Node::TryCatch { .. }
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
