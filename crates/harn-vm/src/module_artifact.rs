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
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chunk::{CachedChunk, CachedCompiledFunction};
use crate::value::VmError;

/// A single `import`-style declaration inside a module. Re-resolved at
/// instantiation time so that the cached artifact does not bake in
/// stale resolved paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleArtifact {
    pub imports: Vec<ModuleImportSpec>,
    pub init_chunk: Option<CachedChunk>,
    pub functions: BTreeMap<String, CachedCompiledFunction>,
    pub public_names: HashSet<String>,
    /// Names of top-level `pub const` / `pub let` value bindings. Their values
    /// are not known at compile time (they are produced by replaying
    /// [`init_chunk`](Self::init_chunk)); the runtime reads each name out of
    /// the instantiated module env and binds it into importers. Disjoint from
    /// [`functions`](Self::functions) and [`public_type_names`](Self::public_type_names).
    pub public_value_names: HashSet<String>,
    /// Names of `pub type` aliases. Type aliases are erased at runtime — they
    /// carry no value of their own — but importers may still name them in a
    /// selective `import { T } from "..."` (for annotations and
    /// schema-as-type use), so the loader must accept these names.
    pub public_type_names: HashSet<String>,
    /// JSON-Schema lowering (serialized as canonical JSON text) for each
    /// `pub type` alias whose body can be expressed as a schema. The loader
    /// binds the imported name to this dict so expression-position uses such
    /// as `output_schema: ImportedAlias` see the same value a local alias
    /// lowers to at compile time. Subset of [`public_type_names`](Self::public_type_names).
    pub public_type_schemas: BTreeMap<String, String>,
}

/// Compile a parsed `.harn` module into the serializable artifact shape.
/// Pure compilation — no I/O, no execution. Used by both the runtime
/// import path (`crates/harn-vm/src/vm/modules.rs`) and the
/// `harn precompile` CLI subcommand.
pub fn compile_module_artifact(
    program: &[harn_parser::SNode],
    module_source_file: Option<String>,
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
                harn_parser::Node::LetBinding { .. } | harn_parser::Node::ConstBinding { .. }
            )
        })
        .cloned()
        .collect();
    let init_chunk = if init_nodes.is_empty() {
        None
    } else {
        let mut compiler = crate::Compiler::new();
        compiler.collect_type_aliases(program);
        Some(
            compiler
                .compile(&init_nodes)
                .map_err(|e| VmError::Runtime(format!("Import init compile error: {e}")))?
                .freeze_for_cache(),
        )
    };

    let mut functions = BTreeMap::new();
    let mut public_names = HashSet::new();
    let mut public_value_names = HashSet::new();
    let mut public_type_names = HashSet::new();
    for node in program {
        let inner = match &node.node {
            harn_parser::Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        if let harn_parser::Node::TypeDecl {
            name, is_pub: true, ..
        } = &inner.node
        {
            public_type_names.insert(name.clone());
            continue;
        }
        // `pub const` / `pub let`: record the exported value-binding names. The
        // value itself is produced when the init chunk is replayed at
        // instantiation time; the runtime reads it out of the module env then.
        if let harn_parser::Node::ConstBinding {
            pattern,
            is_pub: true,
            ..
        }
        | harn_parser::Node::LetBinding {
            pattern,
            is_pub: true,
            ..
        } = &inner.node
        {
            collect_binding_identifier_names(pattern, &mut public_value_names);
            continue;
        }
        let harn_parser::Node::FnDecl {
            name,
            type_params,
            params,
            body,
            is_pub,
            ..
        } = &inner.node
        else {
            continue;
        };

        let mut compiler = crate::Compiler::new();
        compiler.collect_type_aliases(program);
        let func_chunk = compiler
            .compile_fn_body(type_params, params, body, module_source_file.clone())
            .map_err(|e| VmError::Runtime(format!("Import compile error: {e}")))?;
        functions.insert(name.clone(), func_chunk.freeze_for_cache());
        if *is_pub {
            public_names.insert(name.clone());
        }
    }

    let public_type_schemas = crate::Compiler::lower_public_type_schemas(program)
        .into_iter()
        .map(|(name, schema)| (name, crate::stdlib::json::vm_value_to_json(&schema)))
        .collect();

    Ok(ModuleArtifact {
        imports,
        init_chunk,
        functions,
        public_names,
        public_value_names,
        public_type_names,
        public_type_schemas,
    })
}

/// Collect every plain-identifier name bound by a binding pattern (recursing
/// into list/dict/pair destructures). Used to enumerate the value names a
/// `pub const` / `pub let` contributes to a module's public surface.
fn collect_binding_identifier_names(
    pattern: &harn_parser::BindingPattern,
    out: &mut HashSet<String>,
) {
    use harn_parser::BindingPattern;
    match pattern {
        BindingPattern::Identifier(name) => {
            out.insert(name.clone());
        }
        BindingPattern::Pair(first, second) => {
            out.insert(first.clone());
            out.insert(second.clone());
        }
        BindingPattern::List(elements) => {
            for element in elements {
                out.insert(element.name.clone());
            }
        }
        BindingPattern::Dict(fields) => {
            for field in fields {
                out.insert(field.alias.clone().unwrap_or_else(|| field.key.clone()));
            }
        }
    }
}

/// Lex + parse + [`compile_module_artifact`] in one call. Used when the
/// caller already has the raw source bytes and wants the artifact in one
/// step.
pub fn compile_module_artifact_from_source(
    source_path: &Path,
    source: &str,
) -> Result<ModuleArtifact, VmError> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| {
        VmError::Runtime(format!(
            "Import lex error in {}: {e}",
            source_path.display()
        ))
    })?;
    let mut parser = harn_parser::Parser::new(tokens);
    let program = parser.parse().map_err(|e| {
        VmError::Runtime(format!(
            "Import parse error in {}: {e}",
            source_path.display()
        ))
    })?;
    compile_module_artifact(&program, Some(source_path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use harn_lexer::Lexer;
    use harn_parser::Parser;

    use super::{compile_module_artifact, compile_module_artifact_from_source};
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
    fn type_only_modules_export_schemas_without_init_bytecode() {
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
        assert!(artifact.public_type_schemas.contains_key("UserShape"));
        assert!(artifact.public_type_schemas.contains_key("UserList"));
    }
}
