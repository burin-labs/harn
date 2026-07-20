//! Pipeline entry and inheritance lowering.
//!
//! Pipelines compile into the root chunk, unlike ordinary function-like bodies.
//! Their inheritance chain therefore needs one capture analysis spanning every
//! body that shares that runtime environment.

use std::sync::Arc;

use harn_parser::{Node, SNode, TypedParam};

use crate::chunk::{CompiledFunction, Op};

use super::error::CompileError;
use super::{peel_node, Compiler};

impl Compiler {
    /// Compile a pipeline body as a regular VM callable.
    ///
    /// Hosts invoke named pipelines with out-of-band arguments. Lowering that
    /// boundary to a [`CompiledFunction`] keeps arity and runtime
    /// type guards on the same path as every other Harn call instead of
    /// duplicating validation in each host adapter.
    pub(crate) fn compile_pipeline_callable(
        &self,
        program: &[SNode],
        name: &str,
        params: &[TypedParam],
        body: &[SNode],
        extends: Option<&str>,
    ) -> Result<CompiledFunction, CompileError> {
        let mut pipeline_compiler = self.nested_body();
        pipeline_compiler.imported_enum_candidates = self.imported_enum_candidates.clone();
        pipeline_compiler.imported_enum_candidates_authoritative =
            self.imported_enum_candidates_authoritative;
        pipeline_compiler.collect_module_enum_catalog(program);
        if pipeline_compiler.enum_names.insert("Result".to_string()) {
            Self::seed_builtin_variant_owners(&mut pipeline_compiler.enum_variant_owners);
        }
        pipeline_compiler.collect_imported_enum_candidates(program);
        Self::collect_struct_layouts(program, &mut pipeline_compiler.struct_layouts);
        Self::collect_interface_methods(program, &mut pipeline_compiler.interface_methods);
        pipeline_compiler.collect_type_aliases(program);
        pipeline_compiler.declare_param_slots(params);
        pipeline_compiler.record_param_types(params);
        pipeline_compiler.emit_default_preamble(params)?;
        pipeline_compiler.emit_type_checks(params);
        pipeline_compiler.compile_with_pipeline_captures(program, body, extends, |compiler| {
            if let Some(parent_name) = extends {
                compiler.compile_parent_pipeline(program, parent_name)?;
            }
            compiler.compile_block(body)
        })?;
        pipeline_compiler.drain_finallys_to_floor(0)?;
        pipeline_compiler.chunk.emit(Op::Nil, self.line);
        pipeline_compiler.chunk.emit(Op::Return, self.line);

        let param_slots = pipeline_compiler.compile_param_slots(params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(
            &pipeline_compiler.chunk,
            &format!("pipeline `{name}`"),
            self.line,
        )?;
        Ok(CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: pipeline_compiler.nominal_type_names(),
            params: param_slots,
            default_start: TypedParam::default_start(params),
            chunk: Arc::new(pipeline_compiler.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: params.last().is_some_and(|param| param.rest),
            has_runtime_type_checks,
        })
    }

    /// Compile one pipeline inheritance chain with its exact capture set.
    ///
    /// A child closure can capture a mutable binding declared by an inherited
    /// parent pipeline. Analyze the complete lineage as one cumulative value
    /// scope while resetting source-order enum shadowing at each pipeline
    /// boundary. Restoring the prior set preserves the enclosing compilation
    /// context.
    pub(super) fn compile_with_pipeline_captures<'a, T>(
        &mut self,
        program: &'a [SNode],
        body: &'a [SNode],
        extends: Option<&str>,
        compile: impl FnOnce(&mut Self) -> Result<T, CompileError>,
    ) -> Result<T, CompileError> {
        let mut lineage = Vec::new();
        Self::append_parent_pipeline_bodies(program, extends, &mut lineage);
        lineage.push(body);
        let match_patterns = self.lexical_match_pattern_catalog();
        let captured =
            harn_parser::lexical::captured_bindings_in_pipeline_lineage(&lineage, &match_patterns);
        let saved = std::mem::replace(&mut self.captured_bindings, captured);
        let result = compile(self);
        self.captured_bindings = saved;
        result
    }

    /// Recursively compile parent pipeline bodies for `extends`.
    pub(super) fn compile_parent_pipeline(
        &mut self,
        program: &[SNode],
        parent_name: &str,
    ) -> Result<(), CompileError> {
        let parent = program.iter().find(
            |node| matches!(peel_node(node), Node::Pipeline { name, .. } if name == parent_name),
        );
        let Some(parent) = parent else {
            return Ok(());
        };
        let Node::Pipeline { body, extends, .. } = peel_node(parent) else {
            return Ok(());
        };

        // Each pipeline is typechecked in a fresh child of the final module
        // catalog. Inheritance sequences runtime statements, but a parent's
        // source-order enum shadowing must not change how its child is lowered.
        let saved_catalog = self.enum_catalog_snapshot();
        let result: Result<(), CompileError> = (|| {
            if let Some(grandparent) = extends {
                self.compile_parent_pipeline(program, grandparent)?;
            }
            for statement in body {
                self.compile_discarded_stmt(statement)?;
            }
            Ok(())
        })();
        self.restore_enum_catalog(saved_catalog);
        result
    }

    fn append_parent_pipeline_bodies<'a>(
        program: &'a [SNode],
        parent_name: Option<&str>,
        lineage: &mut Vec<&'a [SNode]>,
    ) {
        let Some(parent_name) = parent_name else {
            return;
        };
        let Some(parent) = program.iter().find(
            |node| matches!(peel_node(node), Node::Pipeline { name, .. } if name == parent_name),
        ) else {
            return;
        };
        let Node::Pipeline { body, extends, .. } = peel_node(parent) else {
            return;
        };
        Self::append_parent_pipeline_bodies(program, extends.as_deref(), lineage);
        lineage.push(body);
    }
}
