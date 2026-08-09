//! Serializable shape of a compiled `.harn` module — the unit the
//! on-disk module cache stores.
//!
//! A module is anything `import` can name: a stdlib file (`std/foo`) or
//! a user file on disk. The artifact captures **only** the result of
//! the parse + compile pipeline; instantiation (running the `init`
//! chunk, creating closures bound to a fresh module env, and applying
//! re-exports) happens fresh per process and is not cached. This split
//! lets the cache short-circuit the expensive parse+compile while still
//! producing the per-process state the runtime needs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use harn_modules::{public_declarations, DefKind};
use serde::{Deserialize, Serialize};

use crate::chunk::{CachedChunk, CachedCompiledFunction};
use crate::value::VmError;

type ImportedEnumCache = BTreeMap<PathBuf, ([u8; 32], Vec<String>)>;

/// Authority provenance carried by a compiled module.
///
/// Ordinary source compilation always produces [`User`](Self::User).
/// Privileged variants can only be selected through explicit trusted-embedder
/// entry points; there is no source annotation, filename convention, or
/// environment switch that grants them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModuleProvenance {
    #[default]
    User,
    PrivilegedWire,
    /// A Rust embedder-selected route module and its private import graph.
    /// Unlike `PrivilegedWire`, callables may be exported because only the
    /// selecting host can receive them; ordinary Harn imports never load this
    /// provenance.
    TrustedHostDispatch,
}

fn imported_enum_cache() -> &'static Mutex<ImportedEnumCache> {
    static CACHE: OnceLock<Mutex<ImportedEnumCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A single `import`-style declaration inside a module. Re-resolved at
/// instantiation time so that the cached artifact does not bake in
/// stale resolved paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleImportSpec {
    pub path: String,
    pub binding: ModuleImportBinding,
    pub is_pub: bool,
}

/// The mutually exclusive binding forms of an import declaration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ModuleImportBinding {
    Wildcard,
    Selected(Vec<String>),
    Namespace {
        alias: String,
        demand: harn_parser::NamespaceDemand,
    },
}

/// Serializable compile artifact for one `.harn` module. The runtime
/// turns this into a loaded module by replaying [`init_chunk`](Self::init_chunk)
/// into a fresh env, minting closures for each entry in
/// [`functions`](Self::functions), and re-issuing every nested
/// [`imports`](Self::imports).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleArtifact {
    #[serde(default)]
    pub provenance: ModuleProvenance,
    pub imports: Vec<ModuleImportSpec>,
    /// Cached bytecode that materializes exported type aliases after imports
    /// are bound and before value initialization runs.
    pub type_schema_init_chunks: Vec<CachedChunk>,
    pub init_chunk: Option<CachedChunk>,
    pub functions: BTreeMap<String, CachedCompiledFunction>,
    /// The public declaration contract shared with `harn-modules`. Each name
    /// carries its source declaration kind so the loader can choose a closure,
    /// initialized value, schema, or type-only projection without maintaining
    /// a second AST export table.
    pub public_exports: BTreeMap<String, DefKind>,
    /// Public declarations whose runtime value is produced by replaying
    /// [`init_chunk`](Self::init_chunk), rather than the precompiled function
    /// table. This includes bindings, enums, tools, skills, and eval packs.
    pub public_value_names: HashSet<String>,
    /// Names of erased public type declarations (`type` and `interface`). They
    /// carry no runtime value of their own, but importers may still name them
    /// in selective imports. Public structs and enums are excluded because
    /// they export runtime constructors/namespaces.
    pub public_type_names: HashSet<String>,
}

/// Specialize a fully compiled module for one closed-program export demand.
///
/// Module initialization and every import spec remain intact. Only public
/// projection metadata, exported type schema initialization, and callable
/// bytecode proven unreachable from initialization or retained members are
/// removed. Generic module caches never call this function.
pub fn specialize_module_artifact(
    program: &[harn_parser::SNode],
    source_file: Option<String>,
    mut artifact: ModuleArtifact,
    demand: &harn_modules::ExportDemand,
) -> Result<ModuleArtifact, VmError> {
    use harn_parser::Node;
    use std::collections::{BTreeSet, HashMap};

    if matches!(demand, harn_modules::ExportDemand::WholeNamespace) {
        return Ok(artifact);
    }

    // Public re-exports currently share one projection with local bindings.
    // Until that runtime contract gains a distinct re-export projection,
    // pruning such a module could either leak extra exports or remove names its
    // own code uses. Widen locally rather than weakening semantics.
    if artifact.imports.iter().any(|import| import.is_pub) {
        return Ok(artifact);
    }

    let callable_names = artifact.functions.keys().cloned().collect::<HashSet<_>>();
    let mut declarations = HashMap::<String, &harn_parser::SNode>::new();
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        let name = match &inner.node {
            Node::FnDecl { name, .. }
            | Node::Pipeline { name, .. }
            | Node::StructDecl { name, .. } => Some(name),
            _ => None,
        };
        if let Some(name) = name {
            declarations.insert(name.clone(), inner);
        }
    }

    let mut pending = Vec::new();
    if let harn_modules::ExportDemand::Members(members) = demand {
        pending.extend(
            members
                .iter()
                .filter(|name| callable_names.contains(*name))
                .cloned(),
        );
    }
    // Every initializer is preserved, so every callable it can reach is a root.
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        if matches!(
            &inner.node,
            Node::LetBinding { .. }
                | Node::ConstBinding { .. }
                | Node::EnumDecl { is_pub: true, .. }
                | Node::ToolDecl { .. }
                | Node::SkillDecl { .. }
                | Node::EvalPackDecl { .. }
        ) {
            collect_callable_references(inner, &callable_names, &mut pending);
        }
    }

    let mut retained = HashSet::new();
    while let Some(name) = pending.pop() {
        if !retained.insert(name.clone()) {
            continue;
        }
        if let Some(declaration) = declarations.get(&name) {
            collect_callable_references(declaration, &callable_names, &mut pending);
        }
    }
    artifact.functions.retain(|name, _| retained.contains(name));
    artifact
        .public_exports
        .retain(|name, _| demand.contains(name));
    artifact
        .public_value_names
        .retain(|name| demand.contains(name));
    artifact
        .public_type_names
        .retain(|name| demand.contains(name));

    let selected_type_names = artifact
        .public_type_names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    artifact.type_schema_init_chunks =
        crate::Compiler::compile_selected_public_type_schema_initializers(
            program,
            source_file,
            Some(&selected_type_names),
        )
        .map_err(|error| VmError::Runtime(format!("Import schema compile error: {error}")))?
        .into_iter()
        .map(|chunk| chunk.freeze_for_cache())
        .collect();
    Ok(artifact)
}

fn collect_callable_references(
    node: &harn_parser::SNode,
    callable_names: &HashSet<String>,
    out: &mut Vec<String>,
) {
    use harn_parser::Node;
    let referenced = match &node.node {
        Node::Identifier(name)
        | Node::FunctionCall { name, .. }
        | Node::StructConstruct {
            struct_name: name, ..
        }
        | Node::EnumConstruct {
            enum_name: name, ..
        } => Some(name),
        _ => None,
    };
    if let Some(name) = referenced.filter(|name| callable_names.contains(*name)) {
        out.push(name.clone());
    }
    for child in harn_parser::visit::immediate_children(node) {
        collect_callable_references(child, callable_names, out);
    }
}

impl ModuleArtifact {
    /// Bind relocatable cached bytecode to the source path used by this load.
    ///
    /// Module artifacts may move beside their source (`harn precompile`) or
    /// inside a package. Source paths are diagnostic/debug context, not a
    /// compilation input, so deserialize once and stamp every nested chunk at
    /// the load boundary instead of duplicating otherwise-identical artifacts.
    pub(crate) fn bind_source_file(&mut self, source_path: &Path) {
        let source_file = source_path.display().to_string();
        for chunk in &mut self.type_schema_init_chunks {
            bind_chunk_source_file(chunk, &source_file);
        }
        if let Some(chunk) = &mut self.init_chunk {
            bind_chunk_source_file(chunk, &source_file);
        }
        for function in self.functions.values_mut() {
            bind_chunk_source_file(&mut function.chunk, &source_file);
        }
    }
}

fn bind_chunk_source_file(chunk: &mut CachedChunk, source_file: &str) {
    chunk.source_file = Some(source_file.to_string());
    for function in &mut chunk.functions {
        bind_chunk_source_file(&mut function.chunk, source_file);
    }
}

/// Compile a parsed `.harn` module into the serializable artifact shape.
/// Pure compilation — no I/O, no execution. Used by both the runtime
/// import path (`crates/harn-vm/src/vm/modules.rs`) and the
/// `harn precompile` CLI subcommand.
pub fn compile_module_artifact(
    program: &[harn_parser::SNode],
    module_source_file: Option<String>,
) -> Result<ModuleArtifact, VmError> {
    let imported_enum_candidates = module_source_file
        .as_deref()
        .filter(|_| needs_imported_enum_candidates(program))
        .and_then(|path| {
            harn_modules::build(&[Path::new(path).to_path_buf()])
                .imported_names_by_kind_for_file(Path::new(path), DefKind::Enum)
        })
        .unwrap_or_default();
    compile_module_artifact_with_imported_enums(
        program,
        module_source_file,
        &imported_enum_candidates.into_iter().collect::<Vec<_>>(),
    )
}

fn compile_module_artifact_with_imported_enums(
    program: &[harn_parser::SNode],
    module_source_file: Option<String>,
    imported_enum_candidates: &[String],
) -> Result<ModuleArtifact, VmError> {
    compile_module_artifact_with_provenance(
        program,
        module_source_file,
        imported_enum_candidates,
        ModuleProvenance::User,
    )
}

fn compile_module_artifact_with_provenance(
    program: &[harn_parser::SNode],
    module_source_file: Option<String>,
    imported_enum_candidates: &[String],
    provenance: ModuleProvenance,
) -> Result<ModuleArtifact, VmError> {
    let namespace_demands = harn_parser::namespace_import_demands(program);
    let imports: Vec<ModuleImportSpec> = program
        .iter()
        .filter_map(|node| match &node.node {
            harn_parser::Node::ImportDecl { path, is_pub } => Some(ModuleImportSpec {
                path: path.clone(),
                binding: ModuleImportBinding::Wildcard,
                is_pub: *is_pub,
            }),
            harn_parser::Node::SelectiveImport {
                names,
                path,
                is_pub,
            } => Some(ModuleImportSpec {
                path: path.clone(),
                binding: ModuleImportBinding::Selected(names.clone()),
                is_pub: *is_pub,
            }),
            harn_parser::Node::NamespaceImport {
                alias,
                path,
                is_pub,
            } => Some(ModuleImportSpec {
                path: path.clone(),
                binding: ModuleImportBinding::Namespace {
                    alias: alias.clone(),
                    demand: namespace_demands
                        .get(alias)
                        .cloned()
                        .unwrap_or(harn_parser::NamespaceDemand::Whole),
                },
                is_pub: *is_pub,
            }),
            _ => None,
        })
        .collect();

    if provenance == ModuleProvenance::PrivilegedWire {
        validate_privileged_wire_surface(program, &imports)?;
    }

    let compiler = || match provenance {
        ModuleProvenance::User => crate::Compiler::new(),
        ModuleProvenance::PrivilegedWire => {
            crate::Compiler::with_options(crate::CompilerOptions::privileged_wire())
        }
        ModuleProvenance::TrustedHostDispatch => {
            crate::Compiler::with_options(crate::CompilerOptions::privileged_wire())
        }
    };

    let init_nodes: Vec<harn_parser::SNode> = program
        .iter()
        .filter(|sn| {
            let inner = match &sn.node {
                harn_parser::Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => sn,
            };
            matches!(
                &inner.node,
                harn_parser::Node::LetBinding { .. }
                    | harn_parser::Node::ConstBinding { .. }
                    // Only public enums need a runtime namespace in an
                    // imported module. Private enum construction lowers
                    // directly to `BuildEnum`, just like local construction,
                    // so materializing a private namespace only adds cold
                    // module-init work and closures that can never be
                    // imported.
                    | harn_parser::Node::EnumDecl { is_pub: true, .. }
                    | harn_parser::Node::ToolDecl { .. }
                    | harn_parser::Node::SkillDecl { .. }
                    | harn_parser::Node::EvalPackDecl { .. }
            )
        })
        .cloned()
        .collect();
    let init_chunk = if init_nodes.is_empty() {
        None
    } else {
        let compiler = compiler();
        Some(
            compiler
                .compile_module_init(program, &init_nodes, imported_enum_candidates)
                .map_err(|e| VmError::Runtime(format!("Import init compile error: {e}")))?
                .freeze_for_cache(),
        )
    };

    let public_exports: BTreeMap<String, DefKind> = program
        .iter()
        .flat_map(public_declarations)
        .map(|export| (export.name, export.kind))
        .collect();
    let public_value_names = public_exports
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                DefKind::Variable
                    | DefKind::Enum
                    | DefKind::Tool
                    | DefKind::Skill
                    | DefKind::EvalPack
            )
        })
        .map(|(name, _)| name.clone())
        .collect();
    let public_type_names = public_exports
        .iter()
        .filter(|(_, kind)| !kind.has_runtime_value())
        .map(|(name, _)| name.clone())
        .collect();

    let mut functions = BTreeMap::new();
    for node in program {
        let inner = match &node.node {
            harn_parser::Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        if let harn_parser::Node::StructDecl { name, fields, .. } = &inner.node {
            // Struct constructors are ordinary module callables. Keeping
            // them in the artifact function table avoids replaying the
            // declaration through the module-init chunk while preserving
            // private struct use and public constructor imports.
            let constructor = compiler()
                .compile_struct_constructor(name, fields)
                .map_err(|error| VmError::Runtime(format!("Import compile error: {error}")))?;
            functions.insert(name.clone(), constructor.freeze_for_cache());
            continue;
        }
        if let harn_parser::Node::Pipeline {
            name,
            params,
            body,
            extends,
            ..
        } = &inner.node
        {
            let mut compiler = compiler();
            compiler.add_imported_enum_candidates(imported_enum_candidates.iter().cloned());
            let pipeline = compiler
                .compile_pipeline_callable(program, name, params, body, extends.as_deref())
                .map_err(|error| VmError::Runtime(format!("Import compile error: {error}")))?;
            functions.insert(name.clone(), pipeline.freeze_for_cache());
            continue;
        }
        let harn_parser::Node::FnDecl {
            name,
            type_params,
            params,
            body,
            ..
        } = &inner.node
        else {
            continue;
        };

        let mut compiler = compiler();
        compiler.add_imported_enum_candidates(imported_enum_candidates.iter().cloned());
        compiler.prepare_module_context(program);
        let func_chunk = compiler
            .compile_fn_body(type_params, params, body, module_source_file.clone())
            .map_err(|e| VmError::Runtime(format!("Import compile error: {e}")))?;
        functions.insert(name.clone(), func_chunk.freeze_for_cache());
    }

    let type_schema_init_chunks =
        crate::Compiler::compile_public_type_schema_initializers(program, module_source_file)
            .map_err(|error| VmError::Runtime(format!("Import schema compile error: {error}")))?
            .into_iter()
            .map(|chunk| chunk.freeze_for_cache())
            .collect();

    Ok(ModuleArtifact {
        provenance,
        imports,
        type_schema_init_chunks,
        init_chunk,
        functions,
        public_exports,
        public_value_names,
        public_type_names,
    })
}

fn validate_privileged_wire_surface(
    program: &[harn_parser::SNode],
    imports: &[ModuleImportSpec],
) -> Result<(), VmError> {
    if imports.iter().any(|import| import.is_pub) {
        return Err(VmError::Runtime(
            "Privileged wire modules cannot re-export imports".to_string(),
        ));
    }
    for export in program.iter().flat_map(public_declarations) {
        if export.kind.has_runtime_value() && export.kind != DefKind::Variable {
            return Err(VmError::Runtime(format!(
                "Privileged wire module export `{}` is a {:?}; only explicit capability-value bindings may cross the wire boundary",
                export.name, export.kind
            )));
        }
    }
    Ok(())
}

/// Lex + parse + [`compile_module_artifact`] in one call. Used when the
/// caller already has the raw source bytes and wants the artifact in one
/// step.
pub fn compile_module_artifact_from_source(
    source_path: &Path,
    source: &str,
) -> Result<ModuleArtifact, VmError> {
    let program = parse_module_source(source_path, source)?;
    let imported_enum_candidates =
        imported_enum_candidates_for_program(source_path, source, &program);
    compile_module_artifact_with_imported_enums(
        &program,
        Some(source_path.display().to_string()),
        &imported_enum_candidates,
    )
}

/// Compile a trusted embedder-owned module that may call
/// [`BuiltinExposure::PrivilegedWire`](harn_builtin_meta::BuiltinExposure)
/// primitives.
///
/// The resulting module may export only initialized capability values and
/// erased types. Runtime instantiation validates those values again, so
/// closures, dictionaries, and arbitrary host results cannot smuggle wire
/// authority into user modules.
pub fn compile_privileged_wire_module_artifact_from_source(
    source_path: &Path,
    source: &str,
) -> Result<ModuleArtifact, VmError> {
    let program = parse_module_source(source_path, source)?;
    let imported_enum_candidates =
        imported_enum_candidates_for_program(source_path, source, &program);
    compile_module_artifact_with_provenance(
        &program,
        Some(source_path.display().to_string()),
        &imported_enum_candidates,
        ModuleProvenance::PrivilegedWire,
    )
}

/// Compile one module in a Rust embedder-owned host-dispatch graph.
///
/// The runtime loader is responsible for keeping this provenance outside the
/// ordinary import/cache path and returning only the host-selected callable.
pub fn compile_trusted_host_dispatch_module_artifact_from_source(
    source_path: &Path,
    source: &str,
) -> Result<ModuleArtifact, VmError> {
    let program = parse_module_source(source_path, source)?;
    let imported_enum_candidates =
        imported_enum_candidates_for_program(source_path, source, &program);
    compile_module_artifact_with_provenance(
        &program,
        Some(source_path.display().to_string()),
        &imported_enum_candidates,
        ModuleProvenance::TrustedHostDispatch,
    )
}

/// Compile a trusted host-dispatch module with an already-resolved enum-import
/// projection. Suite prewarming uses this entry point so it can share the
/// module graph walk without weakening provenance or rebuilding the graph in
/// every fresh VM.
pub(crate) fn compile_trusted_host_dispatch_module_artifact_from_source_with_imported_enums(
    source_path: &Path,
    source: &str,
    imported_enum_candidates: impl IntoIterator<Item = String>,
) -> Result<ModuleArtifact, VmError> {
    let program = parse_module_source(source_path, source)?;
    let imported_enum_candidates = imported_enum_candidates.into_iter().collect::<Vec<_>>();
    compile_module_artifact_with_provenance(
        &program,
        Some(source_path.display().to_string()),
        &imported_enum_candidates,
        ModuleProvenance::TrustedHostDispatch,
    )
}

/// Resolve imported enum names only for modules whose match patterns can use
/// them. Ordinary property access is runtime lookup and does not need a graph
/// walk; avoiding it keeps uncached module compilation independent of the
/// size of unrelated import closures.
fn imported_enum_candidates_for_program(
    source_path: &Path,
    source: &str,
    program: &[harn_parser::SNode],
) -> Vec<String> {
    if !needs_imported_enum_candidates(program) {
        return Vec::new();
    }
    let source_hash = *blake3::hash(source.as_bytes()).as_bytes();
    let cache_key = harn_modules::canonical_path(source_path);
    let cacheable = is_immutable_stdlib_path(source_path);
    if cacheable {
        if let Some((_cached_hash, candidates)) = imported_enum_cache()
            .lock()
            .expect("imported enum cache lock poisoned")
            .get(&cache_key)
            .filter(|(cached_hash, _)| *cached_hash == source_hash)
        {
            return candidates.clone();
        }
    }

    // A graph walk is needed to resolve wildcard and re-exported enums, but
    // the result describes every module in that closure. Publish all those
    // projections at once so loading a large stdlib does not rebuild the same
    // reachable graph once per module artifact.
    let graph = harn_modules::build_with_source(source_path, source);
    if !cacheable {
        return sorted_imported_enum_candidates(&graph, source_path);
    }
    let mut projections = Vec::new();
    for path in graph.module_paths() {
        let module_source = if path == cache_key {
            Some(source.to_string())
        } else {
            harn_modules::read_module_source(&path).or_else(|| std::fs::read_to_string(&path).ok())
        };
        let Some(module_source) = module_source else {
            continue;
        };
        let candidates = sorted_imported_enum_candidates(&graph, &path);
        projections.push((
            path,
            (
                *blake3::hash(module_source.as_bytes()).as_bytes(),
                candidates,
            ),
        ));
    }
    let mut cache = imported_enum_cache()
        .lock()
        .expect("imported enum cache lock poisoned");
    for (path, projection) in projections {
        if is_immutable_stdlib_path(&path) {
            cache.insert(path, projection);
        }
    }
    cache
        .get(&cache_key)
        .filter(|(cached_hash, _)| *cached_hash == source_hash)
        .map(|(_, candidates)| candidates.clone())
        .unwrap_or_default()
}

fn sorted_imported_enum_candidates(
    graph: &harn_modules::ModuleGraph,
    source_path: &Path,
) -> Vec<String> {
    let mut candidates = graph
        .imported_names_by_kind_for_file(source_path, DefKind::Enum)
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates
}

fn is_immutable_stdlib_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|path| path.starts_with("<stdlib>/") || path.starts_with("<std>/"))
}

fn needs_imported_enum_candidates(program: &[harn_parser::SNode]) -> bool {
    harn_parser::visit::contains_identifier_enum_pattern(program)
}

fn parse_module_source(
    source_path: &Path,
    source: &str,
) -> Result<Vec<harn_parser::SNode>, VmError> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| {
        VmError::Runtime(format!(
            "Import lex error in {}: {e}",
            source_path.display()
        ))
    })?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser.parse().map_err(|e| {
        VmError::Runtime(format!(
            "Import parse error in {}: {e}",
            source_path.display()
        ))
    })
}

/// Parse and compile a source-backed module when the caller already has the
/// module graph's typed enum-import projection. This keeps precompile/pack
/// from rebuilding the graph separately for the entry chunk and module
/// artifact.
pub fn compile_module_artifact_from_source_with_imported_enums(
    source_path: &Path,
    source: &str,
    imported_enum_candidates: impl IntoIterator<Item = String>,
) -> Result<ModuleArtifact, VmError> {
    let program = parse_module_source(source_path, source)?;
    let imported_enum_candidates = imported_enum_candidates.into_iter().collect::<Vec<_>>();
    compile_module_artifact_with_imported_enums(
        &program,
        Some(source_path.display().to_string()),
        &imported_enum_candidates,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use harn_lexer::Lexer;
    use harn_parser::Parser;

    use super::{
        compile_module_artifact, compile_module_artifact_from_source,
        compile_privileged_wire_module_artifact_from_source, needs_imported_enum_candidates,
        parse_module_source, ModuleImportBinding, ModuleProvenance,
    };
    use crate::chunk::Constant;

    #[test]
    fn module_init_schema_of_uses_full_program_aliases() {
        let source = r"
pub type Item = {id: string}
const ITEM_SCHEMA: Schema<Item> = schema_of(Item)
";
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let artifact = compile_module_artifact(&program, None).unwrap();
        let constants = &artifact.init_chunk.expect("init chunk").constants;
        let strings = constants
            .iter()
            .filter_map(|constant| match constant {
                Constant::String(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(strings.contains(&"id"), "{strings:?}");
        assert!(!strings.contains(&"Item"), "{strings:?}");
    }

    #[test]
    fn type_only_modules_use_a_separate_schema_initializer() {
        let source = r"
pub type UserShape = {name: string, active?: bool}
pub type UserList = list<UserShape>
";

        let artifact =
            compile_module_artifact_from_source(Path::new("<test>/schemas.harn"), source)
                .expect("module compiles");

        assert!(
            artifact.init_chunk.is_none(),
            "erased type aliases must not inflate module init bytecode"
        );
        assert!(artifact.public_type_names.contains("UserShape"));
        assert!(artifact.public_type_names.contains("UserList"));
        assert_eq!(artifact.type_schema_init_chunks.len(), 2);
    }

    #[test]
    fn specialization_prunes_dead_pipeline_struct_and_enum_exports() {
        let source = r#"
pub enum KeptStatus { Ready }
pub enum DeadStatus { Gone }
pub struct KeptConfig { value: int }
pub struct DeadConfig { value: string }
pub pipeline kept_pipeline(harness: Harness) { return KeptConfig({value: 7}) }
pub pipeline dead_pipeline(harness: Harness) { return DeadConfig({value: "dead"}) }
"#;
        let source_path = Path::new("<test>/declarations.harn");
        let parsed = parse_module_source(source_path, source).expect("module parses");
        let full = compile_module_artifact(&parsed, Some(source_path.display().to_string()))
            .expect("module compiles");
        let selected = super::specialize_module_artifact(
            &parsed,
            Some(source_path.display().to_string()),
            full,
            &harn_modules::ExportDemand::Members(std::collections::BTreeSet::from([
                "KeptStatus".to_string(),
                "KeptConfig".to_string(),
                "kept_pipeline".to_string(),
            ])),
        )
        .expect("specialization succeeds");

        assert!(selected.public_exports.contains_key("KeptStatus"));
        assert!(selected.public_exports.contains_key("KeptConfig"));
        assert!(selected.public_exports.contains_key("kept_pipeline"));
        assert!(!selected.public_exports.contains_key("DeadStatus"));
        assert!(!selected.public_exports.contains_key("DeadConfig"));
        assert!(!selected.public_exports.contains_key("dead_pipeline"));
        assert!(selected.functions.contains_key("KeptConfig"));
        assert!(selected.functions.contains_key("kept_pipeline"));
        assert!(!selected.functions.contains_key("DeadConfig"));
        assert!(!selected.functions.contains_key("dead_pipeline"));
    }

    #[test]
    fn nested_namespace_import_retains_static_member_demand() {
        let artifact = compile_module_artifact_from_source(
            Path::new("<test>/wrapper.harn"),
            r#"
import * as lib from "./lib"
pub fn call() { return lib.greet() }
"#,
        )
        .expect("module compiles");

        let ModuleImportBinding::Namespace { alias, demand } = &artifact.imports[0].binding else {
            panic!("expected namespace import metadata");
        };
        assert_eq!(alias, "lib");
        assert_eq!(
            demand,
            &harn_parser::NamespaceDemand::Members(std::collections::BTreeSet::from([
                "greet".to_string(),
            ]))
        );
    }

    #[test]
    fn ordinary_modules_cannot_name_privileged_wire_builtins() {
        let error = compile_module_artifact_from_source(
            Path::new("<test>/user.harn"),
            r#"fn probe() { host_call("project.scan", {}) }"#,
        )
        .expect_err("ordinary source must not acquire wire authority");
        assert!(
            error.to_string().contains("not callable source API"),
            "{error}"
        );
    }

    #[test]
    fn explicit_privileged_compilation_stamps_private_wire_code() {
        let artifact = compile_privileged_wire_module_artifact_from_source(
            Path::new("<trusted>/wire.harn"),
            r#"fn probe() { host_call("project.scan", {}) }"#,
        )
        .expect("trusted private wire function compiles");
        assert_eq!(artifact.provenance, ModuleProvenance::PrivilegedWire);
        assert!(artifact.functions.contains_key("probe"));
        assert!(artifact.public_exports.is_empty());
    }

    #[test]
    fn privileged_wire_functions_cannot_cross_the_module_boundary() {
        let error = compile_privileged_wire_module_artifact_from_source(
            Path::new("<trusted>/wire.harn"),
            r#"pub fn probe() { host_call("project.scan", {}) }"#,
        )
        .expect_err("wire closures must not be exportable");
        assert!(
            error
                .to_string()
                .contains("only explicit capability-value bindings"),
            "{error}"
        );
    }

    #[test]
    fn privileged_wire_modules_cannot_reexport_imports() {
        let error = compile_privileged_wire_module_artifact_from_source(
            Path::new("<trusted>/wire.harn"),
            r#"pub import { probe } from "./other""#,
        )
        .expect_err("wire authority must be non-reexportable");
        assert!(
            error.to_string().contains("cannot re-export imports"),
            "{error}"
        );
    }

    #[test]
    fn schema_initializer_keeps_imported_alias_lookup_and_source() {
        let source = r#"
import { External } from "./external"
pub type Wrapped = {value: External}
"#;
        let source_path = Path::new("<test>/wrapped.harn");
        let artifact =
            compile_module_artifact_from_source(source_path, source).expect("module compiles");
        let chunk = artifact
            .type_schema_init_chunks
            .into_iter()
            .next()
            .expect("schema initializer");
        assert_eq!(chunk.source_file.as_deref(), Some("<test>/wrapped.harn"));
        assert!(chunk
            .constants
            .iter()
            .any(|constant| matches!(constant, Constant::String(value) if value == "External")));
    }

    #[test]
    fn imported_enum_graph_lookup_is_lazy_for_plain_modules() {
        let plain = parse_module_source(
            Path::new("<test>/plain.harn"),
            r#"
import { helper } from "./support"
pub fn run() -> int { return helper(1) }
"#,
        )
        .expect("plain module parses");
        assert!(!needs_imported_enum_candidates(&plain));

        let qualified = parse_module_source(
            Path::new("<test>/qualified.harn"),
            r#"
import { Status } from "./status"
pub fn run(value: Status) {
  match value {
    Status.Ready -> { return 1 }
    _ -> { return 0 }
  }
}
"#,
        )
        .expect("qualified module parses");
        assert!(needs_imported_enum_candidates(&qualified));
    }

    #[test]
    fn private_declarations_do_not_expand_module_init() {
        let artifact = compile_module_artifact_from_source(
            Path::new("<test>/private-declarations.harn"),
            r"
enum PrivateStatus { Ready }
struct PrivateConfig { value: int }
pub fn run() { return PrivateStatus.Ready }
",
        )
        .expect("private declarations compile");

        assert!(artifact.init_chunk.is_none());
        assert!(artifact.functions.contains_key("PrivateConfig"));
        assert!(!artifact.public_exports.contains_key("PrivateStatus"));
        assert!(!artifact.public_exports.contains_key("PrivateConfig"));
    }
}
