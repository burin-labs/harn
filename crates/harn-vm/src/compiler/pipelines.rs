//! Pipeline entry and inheritance lowering.
//!
//! Pipelines compile into the root chunk, unlike ordinary function-like bodies.
//! Their inheritance chain therefore needs one capture analysis spanning every
//! body that shares that runtime environment.

use harn_parser::{Node, SNode};

use super::error::CompileError;
use super::{peel_node, Compiler};

impl Compiler {
    /// Compile one pipeline inheritance chain with its exact capture set.
    ///
    /// A child closure can capture a mutable binding declared by an inherited
    /// parent pipeline. Analyze the complete lineage as one lexical body rather
    /// than losing that cross-body capture. Restoring the prior set preserves
    /// the enclosing compilation context.
    pub(super) fn compile_with_pipeline_captures<T>(
        &mut self,
        program: &[SNode],
        body: &[SNode],
        extends: Option<&str>,
        compile: impl FnOnce(&mut Self) -> Result<T, CompileError>,
    ) -> Result<T, CompileError> {
        let mut lineage = Vec::new();
        Self::append_parent_pipeline_nodes(program, extends, &mut lineage);
        lineage.extend(body.iter().cloned());
        let match_patterns = self.lexical_match_pattern_catalog();
        let captured =
            harn_parser::lexical::captured_bindings_in_nested_callables(&lineage, &match_patterns);
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
        let saved_catalog = (self.enum_names.clone(), self.enum_variant_owners.clone());
        let result: Result<(), CompileError> = (|| {
            if let Some(grandparent) = extends {
                self.compile_parent_pipeline(program, grandparent)?;
            }
            for statement in body {
                self.compile_discarded_stmt(statement)?;
            }
            Ok(())
        })();
        (self.enum_names, self.enum_variant_owners) = saved_catalog;
        result
    }

    fn append_parent_pipeline_nodes(
        program: &[SNode],
        parent_name: Option<&str>,
        lineage: &mut Vec<SNode>,
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
        Self::append_parent_pipeline_nodes(program, extends.as_deref(), lineage);
        lineage.extend(body.iter().cloned());
    }
}
