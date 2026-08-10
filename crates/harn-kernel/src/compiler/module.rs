//! Host-independent compilation of one module for a portable package.
//!
//! The full VM has a richer module loader, but the bytecode that a module
//! owns is the same compiler output used by the portable kernel.  Keeping this
//! small image builder here lets native and browser package adapters share the
//! compiler without making the kernel depend on paths, files, or the async VM.

use std::collections::BTreeMap;

use harn_parser::{Node, SNode};
use serde::{Deserialize, Serialize};

use crate::{Chunk, CompiledFunction};

use super::{peel_node, CompileError, Compiler};

/// A resolved import edge in a portable package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableImport {
    /// The spelling used in source, retained for diagnostics.
    pub path: String,
    /// Stable package-local module identifier selected by the host linker.
    pub target: String,
    pub selected_names: Option<Vec<String>>,
    pub namespace_alias: Option<String>,
    pub is_pub: bool,
}

/// The small declaration-kind projection needed at the runtime export seam.
/// The module graph remains the authority for resolving and validating this
/// projection; the kernel does not inspect filesystem paths or invent a
/// second visibility table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableExportKind {
    Function,
    Pipeline,
    Tool,
    Skill,
    EvalPack,
    Struct,
    Enum,
    Interface,
    Type,
    Variable,
}

impl PortableExportKind {
    pub const fn has_runtime_value(self) -> bool {
        !matches!(self, Self::Interface | Self::Type)
    }
}

/// JSON-friendly source projection for a package that has already been
/// resolved and typechecked by a host linker. The kernel deliberately accepts
/// stable module IDs and resolved import targets rather than filesystem paths;
/// this is the same data shape a browser worker can receive from a build
/// service without gaining loader authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSourceModule {
    pub id: String,
    pub source: String,
    #[serde(default)]
    pub imports: Vec<PortableImport>,
    #[serde(default)]
    pub exports: BTreeMap<String, PortableExportKind>,
    #[serde(default)]
    pub imported_enum_candidates: Vec<String>,
    #[serde(default)]
    pub source_file: Option<String>,
}

/// JSON-friendly source package manifest. `rootImports` and each module's
/// `imports` are linker output, not a second import parser: native hosts can
/// serialize the same projection produced by `harn-modules`, while Wasm only
/// parses source into the canonical Harn AST and hands it to the one compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSourcePackage {
    pub root_source: String,
    #[serde(default)]
    pub root_imports: Vec<PortableImport>,
    #[serde(default)]
    pub modules: Vec<PortableSourceModule>,
}

/// Compiled, host-independent image of a module.  Chunks are still immutable
/// compiler output; a runtime mints fresh environments and closures for each
/// execution, exactly like the native module loader.
#[derive(Debug, Clone)]
pub struct CompiledPortableModule {
    pub id: String,
    pub imports: Vec<PortableImport>,
    pub init: Option<Chunk>,
    pub functions: BTreeMap<String, CompiledFunction>,
    pub exports: BTreeMap<String, PortableExportKind>,
}

impl Compiler {
    /// Compile a module's initialization and callable declarations using the
    /// canonical compiler context.  This is deliberately pure: callers own
    /// source loading, graph resolution, and export policy.
    pub fn compile_portable_module(
        mut self,
        id: impl Into<String>,
        program: &[SNode],
        imports: Vec<PortableImport>,
        exports: BTreeMap<String, PortableExportKind>,
        imported_enum_candidates: &[String],
        source_file: Option<String>,
    ) -> Result<CompiledPortableModule, CompileError> {
        self.prepare_module_context(program);
        self.add_imported_enum_candidates(imported_enum_candidates.iter().cloned());

        let init_nodes: Vec<SNode> = program
            .iter()
            .filter(|sn| {
                let inner = peel_node(sn);
                matches!(
                    inner,
                    Node::LetBinding { .. }
                        | Node::ConstBinding { .. }
                        | Node::EnumDecl { is_pub: true, .. }
                        | Node::ToolDecl { .. }
                        | Node::SkillDecl { .. }
                        | Node::EvalPackDecl { .. }
                )
            })
            .cloned()
            .collect();
        let init = if init_nodes.is_empty() {
            None
        } else {
            Some(Compiler::with_options(self.options).compile_module_init(
                program,
                &init_nodes,
                imported_enum_candidates,
                &[],
            )?)
        };

        let mut functions = BTreeMap::new();
        for node in program {
            let inner = peel_node(node);
            match inner {
                Node::StructDecl { name, fields, .. } => {
                    let constructor = self.compile_struct_constructor(name, fields)?;
                    functions.insert(name.clone(), constructor);
                }
                Node::Pipeline {
                    name,
                    params,
                    body,
                    extends,
                    ..
                } => {
                    let function = self.compile_pipeline_callable(
                        program,
                        name,
                        params,
                        body,
                        extends.as_deref(),
                    )?;
                    functions.insert(name.clone(), function);
                }
                Node::FnDecl {
                    name,
                    type_params,
                    params,
                    body,
                    ..
                } => {
                    let mut compiler = Compiler::with_options(self.options);
                    compiler.prepare_module_context(program);
                    compiler.add_imported_enum_candidates(imported_enum_candidates.iter().cloned());
                    let mut function =
                        compiler.compile_fn_body(type_params, params, body, source_file.clone())?;
                    // `compile_fn_body` is also used for anonymous nested
                    // closures, so it intentionally leaves the display name
                    // empty. A module function is an exported artifact
                    // callable and must carry its stable declaration name.
                    function.name = name.clone();
                    functions.insert(name.clone(), function);
                }
                _ => {}
            }
        }

        Ok(CompiledPortableModule {
            id: id.into(),
            imports,
            init,
            functions,
            exports,
        })
    }
}
