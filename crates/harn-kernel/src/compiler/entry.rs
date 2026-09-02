use std::sync::Arc;

use harn_parser::{Node, SNode, TypedParam};

use crate::chunk::{CompiledFunction, Op};

use super::error::CompileError;
use super::yield_scan::body_contains_yield;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_top_level_declarations(
        &mut self,
        program: &[SNode],
    ) -> Result<(), CompileError> {
        // Run module statements before declarations so callable closures
        // capture source-ordered module bindings. Keep this in step with the
        // import-time module state path in harn-vm.
        for node in program {
            if !harn_parser::lexical::is_deferred_module_declaration(node) {
                self.compile_discarded_stmt(node)?;
            }
        }
        for node in program {
            let inner_kind = match &node.node {
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
                } => self.compile_eval_pack_decl(
                    binding_name,
                    pack_id,
                    fields,
                    body,
                    summarize,
                    false,
                )?,
                Node::FnDecl { .. }
                | Node::ToolDecl { .. }
                | Node::SkillDecl { .. }
                | Node::ImplBlock { .. }
                | Node::StructDecl { .. }
                | Node::EnumDecl { .. }
                | Node::InterfaceDecl { .. } => self.compile_node(node)?,
                Node::TypeDecl { .. } => {}
                _ => {}
            }
        }
        Ok(())
    }

    /// Compile an importable function with the same preamble and metadata as
    /// an in-file declaration.
    pub fn compile_fn_body(
        &mut self,
        type_params: &[harn_parser::TypeParam],
        params: &[TypedParam],
        body: &[SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
        let mut compiler = self.nested_body();
        compiler.enum_names = self.enum_names.clone();
        compiler.enum_variant_owners = self.enum_variant_owners.clone();
        compiler.imported_enum_candidates = self.imported_enum_candidates.clone();
        compiler.imported_enum_candidates_authoritative =
            self.imported_enum_candidates_authoritative;
        compiler.interface_methods = self.interface_methods.clone();
        compiler.type_aliases = self.type_aliases.clone();
        compiler.struct_layouts = self.struct_layouts.clone();
        compiler.declare_param_slots(params);
        compiler.record_param_types(params);
        compiler.emit_default_preamble(params)?;
        compiler.emit_type_checks(params);
        let is_generator = body_contains_yield(body);
        compiler.seed_captured_idents(body);
        compiler.compile_block(body)?;
        compiler.chunk.emit(Op::Nil, 0);
        compiler.chunk.emit(Op::Return, 0);
        compiler.chunk.source_file = source_file;
        let param_slots = compiler.compile_param_slots(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&compiler.chunk, "function body", self.line)?;
        Ok(CompiledFunction {
            name: String::new(),
            type_params: type_params.iter().map(|param| param.name.clone()).collect(),
            nominal_type_names: compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(compiler.chunk),
            is_generator,
            is_stream: false,
            has_rest_param: params.last().is_some_and(|param| param.rest),
            has_runtime_type_checks,
        })
    }

    /// Compile a declared module function and retain its stable diagnostic name.
    pub fn compile_named_fn_body(
        &mut self,
        name: &str,
        type_params: &[harn_parser::TypeParam],
        params: &[TypedParam],
        body: &[SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
        let mut function = self.compile_fn_body(type_params, params, body, source_file)?;
        function.name = name.to_string();
        Ok(function)
    }
}
