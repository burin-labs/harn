use std::sync::Arc;

use harn_parser::{Node, SNode};

use super::{ensure_chunk_addressable, peel_node, CompileError, CompiledCallableEntry, Compiler};
use crate::chunk::Op;

impl Compiler {
    /// Compile a named pipeline for invocation with explicit host values.
    pub fn compile_named_pipeline_entry(
        mut self,
        program: &[SNode],
        pipeline_name: &str,
        fixture_name: Option<&str>,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.prepare_module_context(program);
        self.compile_entry_imports(program)?;
        self.compile_top_level_declarations(program)?;

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
            bootstrap: self.chunk,
            has_fixture: fixture_name.is_some(),
        })
    }

    /// Compile a named function as a top-level callable entry.
    pub fn compile_named_function_entry(
        mut self,
        program: &[SNode],
        function_name: &str,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.prepare_module_context(program);
        self.compile_entry_imports(program)?;
        self.compile_top_level_declarations(program)?;
        let target = program.iter().any(
            |node| matches!(peel_node(node), Node::FnDecl { name, .. } if name == function_name),
        );
        if !target {
            return Err(CompileError {
                message: format!("Unknown function: {function_name}"),
                line: self.line,
            });
        }
        let function = self.string_constant(function_name);
        self.chunk.emit_u16(Op::GetVar, function, self.line);
        self.chunk.emit(Op::Return, self.line);
        ensure_chunk_addressable(&self.chunk, "the callable entry", self.line)?;
        Ok(CompiledCallableEntry {
            bootstrap: self.chunk,
            has_fixture: false,
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
