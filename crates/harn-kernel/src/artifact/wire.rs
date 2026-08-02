use std::collections::HashSet;
use std::sync::Arc;

use harn_parser::TypeExpr;

use crate::{Chunk, CompiledFunction, Constant, LocalSlotInfo, ParamSlot};

use super::validation::{semantic_abi_fingerprint, validate_code, MetadataBudget};
use super::{ArtifactLimits, Diagnostic, EntryKind};

mod reader;
mod writer;

pub(super) use reader::ArtifactReader;
pub(super) use writer::encode_wire_program;

pub(super) struct WireProgram {
    pub(super) semantic_abi: [u8; 32],
    pub(super) entry: String,
    pub(super) entry_kind: EntryKind,
    pub(super) expects_harness: bool,
    pub(super) chunks: Vec<WireChunk>,
    pub(super) functions: Vec<WireFunction>,
}

#[derive(Debug, Clone)]
pub(super) struct WireChunk {
    pub(super) code: Vec<u8>,
    pub(super) constants: Vec<Constant>,
    pub(super) lines: Vec<u32>,
    pub(super) columns: Vec<u32>,
    pub(super) source_file: Option<String>,
    pub(super) functions: Vec<u32>,
    pub(super) local_slots: Vec<WireLocalSlot>,
    pub(super) references_outer_names: bool,
}

#[derive(Debug, Clone)]
pub(super) struct WireFunction {
    pub(super) name: String,
    pub(super) type_params: Vec<String>,
    pub(super) nominal_type_names: Vec<String>,
    pub(super) params: Vec<WireParam>,
    pub(super) default_start: Option<u32>,
    pub(super) chunk: u32,
    pub(super) is_generator: bool,
    pub(super) is_stream: bool,
    pub(super) has_rest_param: bool,
    pub(super) has_runtime_type_checks: bool,
}

#[derive(Debug, Clone)]
pub(super) struct WireParam {
    pub(super) name: String,
    pub(super) type_expr: Option<TypeExpr>,
    pub(super) has_default: bool,
}

#[derive(Debug, Clone)]
pub(super) struct WireLocalSlot {
    pub(super) name: String,
    pub(super) mutable: bool,
    pub(super) scope_depth: u32,
}

impl WireProgram {
    pub(super) fn from_image(
        root: &Chunk,
        entry: String,
        entry_kind: EntryKind,
        expects_harness: bool,
    ) -> Result<Self, Diagnostic> {
        let mut pending = vec![root.clone()];
        let mut chunks = Vec::new();
        let mut functions = Vec::new();
        let mut cursor = 0usize;
        while cursor < pending.len() {
            let chunk = pending[cursor].clone();
            let mut function_ids = Vec::with_capacity(chunk.functions.len());
            for function in &chunk.functions {
                let child_chunk = u32::try_from(pending.len())
                    .map_err(|_| Diagnostic::artifact("artifact_too_large", "too many chunks"))?;
                pending.push((*function.chunk).clone());
                let function_id = u32::try_from(functions.len()).map_err(|_| {
                    Diagnostic::artifact("artifact_too_large", "too many functions")
                })?;
                functions.push(WireFunction {
                    name: function.name.clone(),
                    type_params: function.type_params.clone(),
                    nominal_type_names: function.nominal_type_names.clone(),
                    params: function
                        .params
                        .iter()
                        .map(|param| WireParam {
                            name: param.name.clone(),
                            type_expr: param.type_expr.clone(),
                            has_default: param.has_default,
                        })
                        .collect(),
                    default_start: function.default_start.map(|value| value as u32),
                    chunk: child_chunk,
                    is_generator: function.is_generator,
                    is_stream: function.is_stream,
                    has_rest_param: function.has_rest_param,
                    has_runtime_type_checks: function.has_runtime_type_checks,
                });
                function_ids.push(function_id);
            }
            chunks.push(WireChunk {
                code: chunk.code.clone(),
                constants: chunk.constants.clone(),
                lines: chunk.lines.clone(),
                columns: chunk.columns.clone(),
                source_file: chunk.source_file.clone(),
                functions: function_ids,
                local_slots: chunk
                    .local_slots
                    .iter()
                    .map(|slot| {
                        Ok(WireLocalSlot {
                            name: slot.name.clone(),
                            mutable: slot.mutable,
                            scope_depth: u32::try_from(slot.scope_depth).map_err(|_| {
                                Diagnostic::artifact(
                                    "artifact_metadata_too_large",
                                    "local scope depth exceeds the portable u32 range",
                                )
                            })?,
                        })
                    })
                    .collect::<Result<_, Diagnostic>>()?,
                references_outer_names: chunk.references_outer_names,
            });
            cursor += 1;
        }
        Ok(Self {
            semantic_abi: semantic_abi_fingerprint(),
            entry,
            entry_kind,
            expects_harness,
            chunks,
            functions,
        })
    }

    pub(super) fn validate_metadata(&self, limits: ArtifactLimits) -> Result<(), Diagnostic> {
        if self.chunks.is_empty() {
            return Err(Diagnostic::artifact(
                "artifact_malformed",
                "artifact has no root chunk",
            ));
        }
        if self.chunks.len() > limits.max_chunks {
            return Err(Diagnostic::artifact(
                "artifact_too_many_chunks",
                "artifact chunk count exceeds limit",
            ));
        }
        if self.functions.len() > limits.max_functions {
            return Err(Diagnostic::artifact(
                "artifact_too_many_functions",
                "artifact function count exceeds limit",
            ));
        }

        let mut budget = MetadataBudget::new(limits);
        budget.string(&self.entry)?;
        for chunk in &self.chunks {
            budget.instructions(chunk.code.len())?;
            budget.constants(chunk.constants.len())?;
            budget.metadata(chunk.functions.len())?;
            budget.metadata(chunk.local_slots.len())?;
            if let Some(source_file) = &chunk.source_file {
                budget.string(source_file)?;
            }
            for constant in &chunk.constants {
                if let Constant::String(value) = constant {
                    budget.string(value)?;
                }
            }
            for local in &chunk.local_slots {
                budget.string(&local.name)?;
            }
        }
        for (function_id, function) in self.functions.iter().enumerate() {
            if function.name.is_empty()
                || (function.has_rest_param && function.params.is_empty())
                || (function.is_stream && !function.is_generator)
            {
                return Err(Diagnostic::artifact(
                    "artifact_invalid_function",
                    format!("function {function_id} has incoherent callable metadata"),
                ));
            }
            budget.string(&function.name)?;
            budget.metadata(function.type_params.len())?;
            let mut unique_type_params = HashSet::with_capacity(function.type_params.len());
            for name in &function.type_params {
                if name.is_empty() || !unique_type_params.insert(name.as_str()) {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_function",
                        format!("function {function_id} has invalid type parameters"),
                    ));
                }
                budget.string(name)?;
            }
            budget.metadata(function.nominal_type_names.len())?;
            let mut unique_nominal_types =
                HashSet::with_capacity(function.nominal_type_names.len());
            for name in &function.nominal_type_names {
                if name.is_empty() || !unique_nominal_types.insert(name.as_str()) {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_function",
                        format!("function {function_id} has invalid nominal type metadata"),
                    ));
                }
                budget.string(name)?;
            }
            budget.metadata(function.params.len())?;
            let mut unique_params = HashSet::with_capacity(function.params.len());
            for param in &function.params {
                if param.name.is_empty()
                    || (param.name != "_" && !unique_params.insert(param.name.as_str()))
                {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_function",
                        format!("function {function_id} has invalid parameter names"),
                    ));
                }
                budget.string(&param.name)?;
                if let Some(type_expr) = &param.type_expr {
                    budget.type_expr(type_expr, 1)?;
                }
            }
            let carries_runtime_types = function
                .params
                .iter()
                .any(|param| param.type_expr.is_some());
            if function.has_runtime_type_checks != carries_runtime_types {
                return Err(Diagnostic::artifact(
                    "artifact_runtime_type_metadata",
                    format!(
                        "function {function_id} runtime-type flag does not match its parameter metadata"
                    ),
                ));
            }
            let first_default = function
                .params
                .iter()
                .position(|parameter| parameter.has_default);
            if function.default_start.map(|start| start as usize) != first_default
                || first_default.is_some_and(|start| {
                    function.params[start..]
                        .iter()
                        .any(|parameter| !parameter.has_default)
                })
            {
                return Err(Diagnostic::artifact(
                    "artifact_invalid_function",
                    format!("function {function_id} default parameter metadata is incoherent"),
                ));
            }
        }
        let entry_matches = self.functions.iter().any(|function| {
            function.name == self.entry
                && function.params.first().is_some_and(|parameter| {
                    matches!(
                        parameter.type_expr.as_ref(),
                        Some(TypeExpr::Named(name)) if name == "Harness"
                    )
                }) == self.expects_harness
        });
        if self.entry.is_empty() || !entry_matches {
            return Err(Diagnostic::artifact(
                "artifact_invalid_entry",
                "artifact entry metadata does not identify a matching callable",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_and_build(&self, limits: ArtifactLimits) -> Result<Chunk, Diagnostic> {
        if self.semantic_abi != semantic_abi_fingerprint() {
            return Err(Diagnostic::artifact(
                "artifact_semantic_abi",
                "artifact compiler/opcode/capability contract does not match this kernel",
            ));
        }
        self.validate_metadata(limits)?;
        if self.chunks.is_empty() {
            return Err(Diagnostic::artifact(
                "artifact_malformed",
                "artifact has no root chunk",
            ));
        }
        if self.chunks.len() > limits.max_chunks {
            return Err(Diagnostic::artifact(
                "artifact_too_many_chunks",
                "artifact chunk count exceeds limit",
            ));
        }
        if self.functions.len() > limits.max_functions {
            return Err(Diagnostic::artifact(
                "artifact_too_many_functions",
                "artifact function count exceeds limit",
            ));
        }
        // Named-call bytecode is shared by user functions and builtins. The
        // function metadata is the artifact's mechanically-derived user
        // callable catalog; keeping it here avoids a second compiler registry.
        let user_callables = self
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<HashSet<_>>();
        for (chunk_id, chunk) in self.chunks.iter().enumerate() {
            if chunk.lines.len() != chunk.code.len() || chunk.columns.len() != chunk.code.len() {
                return Err(Diagnostic::artifact(
                    "artifact_malformed",
                    format!("chunk {chunk_id} source maps do not match its code length"),
                ));
            }
            validate_code(
                &chunk.code,
                &chunk.constants,
                &user_callables,
                chunk.functions.len(),
                chunk.local_slots.len(),
                chunk_id,
            )?;
            for &function in &chunk.functions {
                if function as usize >= self.functions.len() {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_index",
                        format!("chunk {chunk_id} references missing function {function}"),
                    ));
                }
            }
        }
        let mut built: Vec<Option<Arc<Chunk>>> = vec![None; self.chunks.len()];
        for chunk_id in (0..self.chunks.len()).rev() {
            let wire = &self.chunks[chunk_id];
            let mut functions = Vec::with_capacity(wire.functions.len());
            for &function_id in &wire.functions {
                let function = &self.functions[function_id as usize];
                if function.chunk as usize <= chunk_id
                    || function.chunk as usize >= self.chunks.len()
                {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_graph",
                        format!("function {function_id} has a cyclic or missing chunk reference"),
                    ));
                }
                let child = built[function.chunk as usize].clone().ok_or_else(|| {
                    Diagnostic::artifact(
                        "artifact_invalid_graph",
                        "function chunk was not constructed",
                    )
                })?;
                functions.push(Arc::new(CompiledFunction {
                    name: function.name.clone(),
                    type_params: function.type_params.clone(),
                    nominal_type_names: function.nominal_type_names.clone(),
                    params: function
                        .params
                        .iter()
                        .map(|param| ParamSlot {
                            name: param.name.clone(),
                            type_expr: param.type_expr.clone(),
                            has_default: param.has_default,
                        })
                        .collect(),
                    default_start: function.default_start.map(|value| value as usize),
                    chunk: child,
                    is_generator: function.is_generator,
                    is_stream: function.is_stream,
                    has_rest_param: function.has_rest_param,
                    has_runtime_type_checks: function.has_runtime_type_checks,
                }));
            }
            built[chunk_id] = Some(Arc::new(Chunk::from_artifact_parts(
                wire.code.clone(),
                wire.constants.clone(),
                wire.lines.clone(),
                wire.columns.clone(),
                wire.source_file.clone(),
                functions,
                wire.local_slots
                    .iter()
                    .map(|slot| LocalSlotInfo {
                        name: slot.name.clone(),
                        mutable: slot.mutable,
                        scope_depth: slot.scope_depth as usize,
                    })
                    .collect(),
                wire.references_outer_names,
            )));
        }
        Arc::try_unwrap(built[0].take().expect("root chunk constructed")).map_err(|_| {
            Diagnostic::artifact(
                "artifact_invalid_graph",
                "root chunk has an unexpected internal reference",
            )
        })
    }
}
