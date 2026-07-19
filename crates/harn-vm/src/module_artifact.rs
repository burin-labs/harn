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

fn imported_enum_cache() -> &'static Mutex<ImportedEnumCache> {
    static CACHE: OnceLock<Mutex<ImportedEnumCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A single `import`-style declaration inside a module. Re-resolved at
/// instantiation time so that the cached artifact does not bake in
/// stale resolved paths.
#[derive(Debug, Serialize, Deserialize)]
pub struct ModuleImportSpec {
    pub path: String,
    pub selected_names: Option<Vec<String>>,
    pub is_pub: bool,
}

/// Serializable compile artifact for one `.harn` module. The runtime
/// turns this into a loaded module by replaying [`init_chunk`](Self::init_chunk)
/// into a fresh env, minting closures for each entry in
/// [`functions`](Self::functions), and re-issuing every nested
/// [`imports`](Self::imports).
#[derive(Debug, Serialize, Deserialize)]
pub struct ModuleArtifact {
    pub imports: Vec<ModuleImportSpec>,
    /// Cached bytecode that materializes exported type aliases after imports
    /// are bound and before value initialization runs.
    pub type_schema_init_chunk: Option<CachedChunk>,
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

impl ModuleArtifact {
    /// Bind relocatable cached bytecode to the source path used by this load.
    ///
    /// Module artifacts may move beside their source (`harn precompile`) or
    /// inside a package. Source paths are diagnostic/debug context, not a
    /// compilation input, so deserialize once and stamp every nested chunk at
    /// the load boundary instead of duplicating otherwise-identical artifacts.
    pub(crate) fn bind_source_file(&mut self, source_path: &Path) {
        let source_file = source_path.display().to_string();
        if let Some(chunk) = &mut self.type_schema_init_chunk {
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
    let imports = program
        .iter()
        .filter_map(|node| match &node.node {
            harn_parser::Node::ImportDecl { path, is_pub } => Some(ModuleImportSpec {
                path: path.clone(),
                selected_names: None,
                is_pub: *is_pub,
            }),
            harn_parser::Node::SelectiveImport {
                names,
                path,
                is_pub,
            } => Some(ModuleImportSpec {
                path: path.clone(),
                selected_names: Some(names.clone()),
                is_pub: *is_pub,
            }),
            _ => None,
        })
        .collect();

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
        let compiler = crate::Compiler::new();
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
            let constructor = crate::Compiler::new()
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
            let mut compiler = crate::Compiler::new();
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

        let mut compiler = crate::Compiler::new();
        compiler.add_imported_enum_candidates(imported_enum_candidates.iter().cloned());
        compiler.prepare_module_context(program);
        let func_chunk = compiler
            .compile_fn_body(type_params, params, body, module_source_file.clone())
            .map_err(|e| VmError::Runtime(format!("Import compile error: {e}")))?;
        functions.insert(name.clone(), func_chunk.freeze_for_cache());
    }

    let type_schema_init_chunk =
        crate::Compiler::compile_public_type_schema_initializers(program, module_source_file)
            .map_err(|error| VmError::Runtime(format!("Import schema compile error: {error}")))?
            .map(|chunk| chunk.freeze_for_cache());

    Ok(ModuleArtifact {
        imports,
        type_schema_init_chunk,
        init_chunk,
        functions,
        public_exports,
        public_value_names,
        public_type_names,
    })
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

/// Resolve imported enum names only for modules whose syntax can use them.
/// Most modules never contain a qualified property access; rebuilding a full
/// module graph for those files needlessly reparses imports and their
/// dependencies during every uncached module compilation.
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
        needs_imported_enum_candidates, parse_module_source,
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
        assert!(artifact.type_schema_init_chunk.is_some());
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
        let chunk = artifact.type_schema_init_chunk.expect("schema initializer");
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
pub fn run() { return Status.Ready() }
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
