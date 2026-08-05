use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use harn_parser::TypeExpr;

use crate::program::ChunkParts;
use crate::{
    BindingTypeSlot, Chunk, CompiledFunction, Constant, LocalSlotInfo, ParamSlot,
    PortableExportKind, PortableImport,
};

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
    pub(super) root_imports: Vec<PortableImport>,
    pub(super) modules: Vec<WireModule>,
}

#[derive(Debug, Clone)]
pub(super) struct WireModule {
    pub(super) id: String,
    pub(super) imports: Vec<PortableImport>,
    pub(super) init: Option<u32>,
    pub(super) functions: BTreeMap<String, u32>,
    pub(super) exports: BTreeMap<String, PortableExportKind>,
}

pub(super) struct BuiltProgram {
    pub(super) root: Chunk,
    pub(super) root_imports: Vec<PortableImport>,
    pub(super) modules: Vec<BuiltModule>,
}

pub(super) struct BuiltModule {
    pub(super) id: String,
    pub(super) imports: Vec<PortableImport>,
    pub(super) init: Option<Chunk>,
    pub(super) functions: BTreeMap<String, Arc<CompiledFunction>>,
    pub(super) exports: BTreeMap<String, PortableExportKind>,
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
    pub(super) binding_types: Vec<WireBindingType>,
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
pub(super) struct WireBindingType {
    pub(super) name: String,
    pub(super) type_expr: TypeExpr,
    pub(super) nominal_type_names: Vec<String>,
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
                binding_types: chunk
                    .binding_types
                    .iter()
                    .map(|slot| WireBindingType {
                        name: slot.name.clone(),
                        type_expr: slot.type_expr.clone(),
                        nominal_type_names: slot.nominal_type_names.clone(),
                    })
                    .collect(),
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
            root_imports: Vec::new(),
            modules: Vec::new(),
        })
    }

    pub(super) fn from_package(
        root: &Chunk,
        modules: &[crate::CompiledPortableModule],
        root_imports: Vec<PortableImport>,
        entry: String,
        entry_kind: EntryKind,
        expects_harness: bool,
    ) -> Result<Self, Diagnostic> {
        let mut pending = vec![root.clone()];
        let mut functions = Vec::new();
        let mut module_wires = Vec::with_capacity(modules.len());
        let append_function = |function: &CompiledFunction,
                               pending: &mut Vec<Chunk>,
                               functions: &mut Vec<WireFunction>| {
            let child_chunk = u32::try_from(pending.len())
                .map_err(|_| Diagnostic::artifact("artifact_too_large", "too many chunks"))?;
            pending.push((*function.chunk).clone());
            let function_id = u32::try_from(functions.len())
                .map_err(|_| Diagnostic::artifact("artifact_too_large", "too many functions"))?;
            functions.push(WireFunction::from_compiled(function, child_chunk));
            Ok::<u32, Diagnostic>(function_id)
        };

        // Module order is part of the artifact's canonical bytes. The caller
        // normally supplies graph order, but sorting here makes a package
        // deterministic even when a parallel graph walk returns a hash map.
        let mut modules = modules.to_vec();
        modules.sort_by(|left, right| left.id.cmp(&right.id));
        for module in &modules {
            let init = module
                .init
                .as_ref()
                .map(|chunk| {
                    let id = u32::try_from(pending.len()).map_err(|_| {
                        Diagnostic::artifact("artifact_too_large", "too many chunks")
                    })?;
                    pending.push(chunk.clone());
                    Ok::<u32, Diagnostic>(id)
                })
                .transpose()?;
            let mut module_functions = BTreeMap::new();
            for (name, function) in &module.functions {
                let id = append_function(function, &mut pending, &mut functions)?;
                module_functions.insert(name.clone(), id);
            }
            module_wires.push(WireModule {
                id: module.id.clone(),
                imports: module.imports.clone(),
                init,
                functions: module_functions,
                exports: module.exports.clone(),
            });
        }

        // The root and every module init/function may contain nested closures.
        // Walk the complete chunk queue once, appending their function records
        // in source order just like `from_image`.
        let mut chunks = Vec::new();
        let mut cursor = 0usize;
        while cursor < pending.len() {
            let chunk = pending[cursor].clone();
            let mut function_ids = Vec::with_capacity(chunk.functions.len());
            for function in &chunk.functions {
                let id = append_function(function, &mut pending, &mut functions)?;
                function_ids.push(id);
            }
            chunks.push(Self::wire_chunk(&chunk, function_ids)?);
            cursor += 1;
        }
        Ok(Self {
            semantic_abi: semantic_abi_fingerprint(),
            entry,
            entry_kind,
            expects_harness,
            chunks,
            functions,
            root_imports,
            modules: module_wires,
        })
    }

    fn wire_chunk(chunk: &Chunk, functions: Vec<u32>) -> Result<WireChunk, Diagnostic> {
        Ok(WireChunk {
            code: chunk.code.clone(),
            constants: chunk.constants.clone(),
            lines: chunk.lines.clone(),
            columns: chunk.columns.clone(),
            source_file: chunk.source_file.clone(),
            functions,
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
            binding_types: chunk
                .binding_types
                .iter()
                .map(|slot| WireBindingType {
                    name: slot.name.clone(),
                    type_expr: slot.type_expr.clone(),
                    nominal_type_names: slot.nominal_type_names.clone(),
                })
                .collect(),
            references_outer_names: chunk.references_outer_names,
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
            budget.metadata(chunk.binding_types.len())?;
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
            for binding in &chunk.binding_types {
                budget.string(&binding.name)?;
                budget.type_expr(&binding.type_expr, 1)?;
                for nominal in &binding.nominal_type_names {
                    budget.string(nominal)?;
                }
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
        budget_imports(&mut budget, &self.root_imports)?;
        if self.modules.len() > limits.max_chunks {
            return Err(Diagnostic::artifact(
                "artifact_too_many_modules",
                "package module count exceeds limit",
            ));
        }
        let mut module_ids = HashSet::with_capacity(self.modules.len());
        for module in &self.modules {
            if module.id.is_empty() || !module_ids.insert(module.id.as_str()) {
                return Err(Diagnostic::artifact(
                    "artifact_invalid_module",
                    "package contains an empty or duplicate module id",
                ));
            }
            budget.string(&module.id)?;
            budget_imports(&mut budget, &module.imports)?;
            if let Some(init) = module.init {
                if init as usize >= self.chunks.len() {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_index",
                        format!(
                            "module `{}` references missing init chunk {init}",
                            module.id
                        ),
                    ));
                }
            }
            for (name, function) in &module.functions {
                budget.string(name)?;
                if *function as usize >= self.functions.len() {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_index",
                        format!(
                            "module `{}` references missing function {function}",
                            module.id
                        ),
                    ));
                }
            }
            for name in module.exports.keys() {
                budget.string(name)?;
            }
        }
        // Import targets are package-local identifiers, not arbitrary paths.
        // Resolve this graph at the artifact boundary so a malformed or
        // hand-crafted artifact cannot make the runtime consult host paths or
        // silently turn a missing dependency into an empty module.
        validate_import_targets(&self.root_imports, &module_ids, "root")?;
        for module in &self.modules {
            validate_import_targets(&module.imports, &module_ids, &module.id)?;
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

    pub(super) fn validate_and_build(
        &self,
        limits: ArtifactLimits,
    ) -> Result<BuiltProgram, Diagnostic> {
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
                chunk.functions.len(),
                chunk.local_slots.len(),
                chunk.binding_types.len(),
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
            built[chunk_id] = Some(Arc::new(Chunk::from_artifact_parts(ChunkParts {
                code: wire.code.clone(),
                constants: wire.constants.clone(),
                lines: wire.lines.clone(),
                columns: wire.columns.clone(),
                source_file: wire.source_file.clone(),
                functions,
                local_slots: wire
                    .local_slots
                    .iter()
                    .map(|slot| LocalSlotInfo {
                        name: slot.name.clone(),
                        mutable: slot.mutable,
                        scope_depth: slot.scope_depth as usize,
                    })
                    .collect(),
                binding_types: wire
                    .binding_types
                    .iter()
                    .map(|slot| BindingTypeSlot {
                        name: slot.name.clone(),
                        type_expr: slot.type_expr.clone(),
                        nominal_type_names: slot.nominal_type_names.clone(),
                    })
                    .collect(),
                references_outer_names: wire.references_outer_names,
            })));
        }
        let root =
            Arc::try_unwrap(built[0].take().expect("root chunk constructed")).map_err(|_| {
                Diagnostic::artifact(
                    "artifact_invalid_graph",
                    "root chunk has an unexpected internal reference",
                )
            })?;

        let mut modules = Vec::with_capacity(self.modules.len());
        let mut module_ids = HashSet::new();
        for module in &self.modules {
            if module.id.is_empty() || !module_ids.insert(module.id.as_str()) {
                return Err(Diagnostic::artifact(
                    "artifact_invalid_module",
                    "package contains an empty or duplicate module id",
                ));
            }
            let init = module
                .init
                .map(|chunk| {
                    built
                        .get(chunk as usize)
                        .and_then(Option::clone)
                        .ok_or_else(|| {
                            Diagnostic::artifact(
                                "artifact_invalid_module",
                                "module initialization chunk was not constructed",
                            )
                        })
                })
                .transpose()?;
            let mut functions = BTreeMap::new();
            for (name, function_id) in &module.functions {
                let function = self.functions.get(*function_id as usize).ok_or_else(|| {
                    Diagnostic::artifact(
                        "artifact_invalid_module",
                        "module references a missing function",
                    )
                })?;
                let chunk = built
                    .get(function.chunk as usize)
                    .and_then(Option::clone)
                    .ok_or_else(|| {
                        Diagnostic::artifact(
                            "artifact_invalid_module",
                            "module function chunk was not constructed",
                        )
                    })?;
                functions.insert(
                    name.clone(),
                    Arc::new(CompiledFunction {
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
                        chunk,
                        is_generator: function.is_generator,
                        is_stream: function.is_stream,
                        has_rest_param: function.has_rest_param,
                        has_runtime_type_checks: function.has_runtime_type_checks,
                    }),
                );
            }
            modules.push(BuiltModule {
                id: module.id.clone(),
                imports: module.imports.clone(),
                init: init
                    .map(|chunk| Arc::try_unwrap(chunk).unwrap_or_else(|chunk| (*chunk).clone())),
                functions,
                exports: module.exports.clone(),
            });
        }
        Ok(BuiltProgram {
            root,
            root_imports: self.root_imports.clone(),
            modules,
        })
    }
}

impl WireFunction {
    fn from_compiled(function: &CompiledFunction, chunk: u32) -> Self {
        Self {
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
            chunk,
            is_generator: function.is_generator,
            is_stream: function.is_stream,
            has_rest_param: function.has_rest_param,
            has_runtime_type_checks: function.has_runtime_type_checks,
        }
    }
}

fn budget_imports(
    budget: &mut MetadataBudget,
    imports: &[PortableImport],
) -> Result<(), Diagnostic> {
    budget.metadata(imports.len())?;
    for import in imports {
        budget.string(&import.path)?;
        budget.string(&import.target)?;
        if let Some(names) = &import.selected_names {
            budget.metadata(names.len())?;
            for name in names {
                budget.string(name)?;
            }
        }
        if let Some(alias) = &import.namespace_alias {
            budget.string(alias)?;
        }
    }
    Ok(())
}

pub(super) fn validate_import_targets(
    imports: &[PortableImport],
    module_ids: &HashSet<&str>,
    owner: &str,
) -> Result<(), Diagnostic> {
    for import in imports {
        if import.path.is_empty() || import.target.is_empty() {
            return Err(Diagnostic::artifact(
                "artifact_invalid_import",
                format!("{owner} contains an import with an empty path or target"),
            ));
        }
        if import.selected_names.is_some() && import.namespace_alias.is_some() {
            return Err(Diagnostic::artifact(
                "artifact_invalid_import",
                format!(
                    "{owner} import `{}` mixes selected and namespace bindings",
                    import.path
                ),
            ));
        }
        if let Some(names) = &import.selected_names {
            let mut seen = HashSet::with_capacity(names.len());
            for name in names {
                if name.is_empty() || !seen.insert(name.as_str()) {
                    return Err(Diagnostic::artifact(
                        "artifact_invalid_import",
                        format!(
                            "{owner} import `{}` has duplicate or empty selected names",
                            import.path
                        ),
                    ));
                }
            }
        }
        if import.namespace_alias.as_deref().is_some_and(str::is_empty) {
            return Err(Diagnostic::artifact(
                "artifact_invalid_import",
                format!(
                    "{owner} import `{}` has an empty namespace alias",
                    import.path
                ),
            ));
        }
        if !module_ids.contains(import.target.as_str()) {
            return Err(Diagnostic::artifact(
                "artifact_invalid_import",
                format!(
                    "{owner} import `{}` targets missing module `{}`",
                    import.path, import.target
                ),
            ));
        }
    }
    Ok(())
}
