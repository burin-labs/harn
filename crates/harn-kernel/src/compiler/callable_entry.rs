use std::sync::Arc;

use harn_parser::{Node, SNode};

use super::{
    ensure_chunk_addressable, peel_node, CompileError, CompiledCallableBatch,
    CompiledCallableEntry, Compiler,
};
use crate::chunk::Op;

impl Compiler {
    /// Compile a named pipeline for invocation with explicit host values.
    pub fn compile_named_pipeline_entry(
        self,
        program: &[SNode],
        pipeline_name: &str,
        fixture_name: Option<&str>,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.compile_named_pipeline_entries(program, &[(pipeline_name, fixture_name)])?
            .pop()
            .ok_or_else(|| CompileError {
                message: "named pipeline entry request was empty".to_string(),
                line: 0,
            })?
    }

    /// Compile several named pipelines while lowering the file's imports and
    /// top-level declarations exactly once.
    ///
    /// Each returned bootstrap remains an immutable, self-contained artifact;
    /// runtimes can therefore instantiate it in a fresh VM without sharing
    /// module state between invocations. The request and result orders match.
    pub fn compile_named_pipeline_entries(
        self,
        program: &[SNode],
        entries: &[(&str, Option<&str>)],
    ) -> Result<Vec<Result<CompiledCallableEntry, CompileError>>, CompileError> {
        self.compile_named_callable_entries(program, entries, &[])
            .map(|batch| batch.pipelines)
    }

    /// Compile pipeline and function entry artifacts through one shared file
    /// lowering. This is the owning batch boundary for hosts that need several
    /// independently executable callables from the same source module.
    pub fn compile_named_callable_entries(
        mut self,
        program: &[SNode],
        pipelines: &[(&str, Option<&str>)],
        functions: &[&str],
    ) -> Result<CompiledCallableBatch, CompileError> {
        if pipelines.is_empty() && functions.is_empty() {
            return Ok(CompiledCallableBatch {
                pipelines: Vec::new(),
                functions: Vec::new(),
            });
        }
        self.prepare_module_context(program);
        self.compile_entry_imports(program)?;
        self.compile_top_level_declarations(program)?;

        let base_chunk = self.chunk.clone();
        let base_string_constants = self.string_constants.clone();
        let mut compiled_pipelines = Vec::with_capacity(pipelines.len());
        for (pipeline_name, fixture_name) in pipelines {
            self.chunk = base_chunk.clone();
            self.string_constants.clone_from(&base_string_constants);
            compiled_pipelines.push(self.finish_named_pipeline_entry(
                program,
                pipeline_name,
                *fixture_name,
            ));
        }
        let mut compiled_functions = Vec::with_capacity(functions.len());
        for function_name in functions {
            self.chunk = base_chunk.clone();
            self.string_constants.clone_from(&base_string_constants);
            compiled_functions.push(self.finish_named_function_entry(program, function_name));
        }
        Ok(CompiledCallableBatch {
            pipelines: compiled_pipelines,
            functions: compiled_functions,
        })
    }

    fn finish_named_pipeline_entry(
        &mut self,
        program: &[SNode],
        pipeline_name: &str,
        fixture_name: Option<&str>,
    ) -> Result<CompiledCallableEntry, CompileError> {
        let fixture_expects_harness = fixture_name.is_some_and(|fixture_name| {
            program.iter().any(|node| {
                matches!(
                    peel_node(node),
                    Node::FnDecl { name, params, .. }
                        if name == fixture_name
                            && params.first().is_some_and(|param| matches!(
                                param.type_expr.as_ref(),
                                Some(harn_parser::TypeExpr::Named(name)) if name == "Harness"
                            ))
                )
            })
        });
        if let Some(fixture_name) = fixture_name {
            let fixture = self.string_constant(fixture_name);
            self.chunk.emit_u16(Op::GetVar, fixture, self.line);
        }
        let target = program.iter().find(
            |node| matches!(peel_node(node), Node::Pipeline { name, .. } if name == pipeline_name),
        );
        let Some(target) = target else {
            return Err(CompileError {
                message: format!("Unknown pipeline: {pipeline_name}"),
                line: self.line,
            });
        };
        let Node::Pipeline {
            name,
            body,
            extends,
            params,
            ..
        } = peel_node(target)
        else {
            unreachable!("pipeline target was matched above");
        };
        let expects_harness = params.first().is_some_and(|param| {
            matches!(
                param.type_expr.as_ref(),
                Some(harn_parser::TypeExpr::Named(name)) if name == "Harness"
            )
        });
        let callable =
            self.compile_pipeline_callable(program, name, params, body, extends.as_deref())?;
        let function_index = self.chunk.functions.len();
        self.chunk.functions.push(Arc::new(callable));
        self.chunk
            .emit_u16(Op::Closure, function_index as u16, self.line);
        if fixture_name.is_some() {
            self.chunk.emit_u16(Op::BuildList, 2, self.line);
        }
        self.chunk.emit(Op::Return, self.line);
        ensure_chunk_addressable(&self.chunk, "the callable entry", self.line)?;
        Ok(CompiledCallableEntry {
            bootstrap: std::mem::take(&mut self.chunk),
            has_fixture: fixture_name.is_some(),
            fixture_expects_harness,
            expects_harness,
        })
    }

    /// Compile a named function as a top-level callable entry.
    pub fn compile_named_function_entry(
        self,
        program: &[SNode],
        function_name: &str,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.compile_named_callable_entries(program, &[], &[function_name])?
            .functions
            .pop()
            .ok_or_else(|| CompileError {
                message: "named function entry request was empty".to_string(),
                line: 0,
            })?
    }

    fn finish_named_function_entry(
        &mut self,
        program: &[SNode],
        function_name: &str,
    ) -> Result<CompiledCallableEntry, CompileError> {
        let target = program.iter().find(
            |node| matches!(peel_node(node), Node::FnDecl { name, .. } if name == function_name),
        );
        let Some(target) = target else {
            return Err(CompileError {
                message: format!("Unknown function: {function_name}"),
                line: self.line,
            });
        };
        let expects_harness = matches!(
            peel_node(target),
            Node::FnDecl { params, .. }
                if params.first().is_some_and(|param| matches!(
                    param.type_expr.as_ref(),
                    Some(harn_parser::TypeExpr::Named(name)) if name == "Harness"
                ))
        );
        let function = self.string_constant(function_name);
        self.chunk.emit_u16(Op::GetVar, function, self.line);
        self.chunk.emit(Op::Return, self.line);
        ensure_chunk_addressable(&self.chunk, "the callable entry", self.line)?;
        Ok(CompiledCallableEntry {
            bootstrap: std::mem::take(&mut self.chunk),
            has_fixture: false,
            fixture_expects_harness: false,
            expects_harness,
        })
    }

    fn compile_entry_imports(&mut self, program: &[SNode]) -> Result<(), CompileError> {
        for node in program {
            if matches!(
                &node.node,
                Node::ImportDecl { .. }
                    | Node::SelectiveImport { .. }
                    | Node::NamespaceImport { .. }
            ) {
                self.compile_node(node)?;
            }
        }
        Ok(())
    }
}
