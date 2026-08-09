//! Filesystem adapter for the authority-free portable compiler.
//!
//! The canonical module resolver closes a source file's import graph here.
//! Every native caller then hands the same normalized package to
//! `harn-kernel`; benchmark and host commands must not grow their own loaders.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use harn_kernel::{
    EntryKind, PortableExportKind, PortableImport, PortableSourceModule, PortableSourcePackage,
    ProgramArtifact, PORTABLE_MAX_PACKAGE_BYTES, PORTABLE_MAX_PACKAGE_MODULES,
    PORTABLE_MAX_SOURCE_BYTES,
};

pub(crate) struct PortableSourceInput {
    source: String,
    package: Option<PortableSourcePackage>,
}

impl PortableSourceInput {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let source = read_source(path)?;
        let package = build_package(path, &source)?;
        Ok(Self { source, package })
    }

    pub(crate) fn compile(
        &self,
        entry: &str,
        entry_kind: EntryKind,
    ) -> Result<ProgramArtifact, String> {
        let result = match self.package.as_ref() {
            Some(package) => {
                harn_kernel::compile_source_package(package.clone(), entry, entry_kind)
            }
            None => harn_kernel::compile_program(&self.source, entry, entry_kind),
        };
        result.map_err(render_diagnostics)
    }

    /// Return the deterministic, data-only projection consumed by browser
    /// `compilePackage`. Filesystem and module-graph authority stay here.
    pub(crate) fn source_package(&self) -> PortableSourcePackage {
        self.package
            .clone()
            .unwrap_or_else(|| PortableSourcePackage {
                root_source: self.source.clone(),
                root_imports: Vec::new(),
                modules: Vec::new(),
            })
    }
}

/// Project the canonical module graph into the portable compiler's closed,
/// path-independent package. `None` means the root has no imports and can use
/// the smaller single-module artifact path.
fn build_package(
    root_path: &Path,
    root_source: &str,
) -> Result<Option<PortableSourcePackage>, String> {
    let root = harn_modules::canonical_path(root_path);
    let root_program = parse_module_source(root_source)?;
    let graph = harn_modules::build(std::slice::from_ref(&root));
    let has_imports = root_program.iter().any(|node| {
        matches!(
            node.node,
            harn_parser::Node::ImportDecl { .. }
                | harn_parser::Node::SelectiveImport { .. }
                | harn_parser::Node::NamespaceImport { .. }
        )
    });
    if !has_imports {
        return Ok(None);
    }

    let module_paths = graph
        .module_paths()
        .into_iter()
        .filter(|path| path != &root)
        .collect::<Vec<_>>();
    if module_paths.len() > PORTABLE_MAX_PACKAGE_MODULES {
        return Err(format!(
            "portable package has {} modules; limit is {PORTABLE_MAX_PACKAGE_MODULES}",
            module_paths.len()
        ));
    }
    let module_ids = module_paths
        .iter()
        .enumerate()
        .map(|(index, path)| (path.clone(), format!("module/{index}")))
        .collect::<BTreeMap<_, _>>();
    let root_imports = portable_imports(&root, &root_program, &module_ids)?;
    let mut modules = Vec::new();
    let mut package_source_bytes = root_source.len();
    for path in module_paths {
        let Some(source) = harn_modules::read_module_source(&path) else {
            return Err(format!(
                "portable package cannot read module {}",
                path.display()
            ));
        };
        if source.len() > PORTABLE_MAX_SOURCE_BYTES {
            return Err(format!(
                "portable module {} has {} bytes; limit is {PORTABLE_MAX_SOURCE_BYTES}",
                path.display(),
                source.len()
            ));
        }
        package_source_bytes = package_source_bytes.saturating_add(source.len());
        if package_source_bytes > PORTABLE_MAX_PACKAGE_BYTES {
            return Err(format!(
                "portable package sources exceed the {PORTABLE_MAX_PACKAGE_BYTES}-byte limit"
            ));
        }
        let program = parse_module_source(&source)?;
        let mut exports = BTreeMap::new();
        for name in graph.exports_for_module(&path) {
            let Some(kind) = graph.exported_kind(&path, &name) else {
                return Err(format!(
                    "portable package has no declaration kind for {}::{name}",
                    path.display()
                ));
            };
            exports.insert(name, portable_export_kind(kind));
        }
        let mut imported_enum_candidates = graph
            .imported_names_by_kind_for_file(&path, harn_modules::DefKind::Enum)
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        imported_enum_candidates.sort();
        let Some(id) = module_ids.get(&path).cloned() else {
            return Err(format!(
                "portable package has no stable id for {}",
                path.display()
            ));
        };
        modules.push(PortableSourceModule {
            id: id.clone(),
            source,
            imports: portable_imports(&path, &program, &module_ids)?,
            exports,
            imported_enum_candidates,
            source_file: Some(id),
        });
    }
    Ok(Some(PortableSourcePackage {
        root_source: root_source.to_string(),
        root_imports,
        modules,
    }))
}

fn read_source(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    if metadata.len() > PORTABLE_MAX_SOURCE_BYTES as u64 {
        return Err(format!(
            "portable source {} has {} bytes; limit is {PORTABLE_MAX_SOURCE_BYTES}",
            path.display(),
            metadata.len()
        ));
    }
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    if source.len() > PORTABLE_MAX_SOURCE_BYTES {
        return Err(format!(
            "portable source {} grew beyond the {PORTABLE_MAX_SOURCE_BYTES}-byte limit while being read",
            path.display()
        ));
    }
    Ok(source)
}

fn parse_module_source(source: &str) -> Result<Vec<harn_parser::SNode>, String> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer
        .tokenize()
        .map_err(|error| format!("portable package lex error: {error}"))?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser
        .parse()
        .map_err(|error| format!("portable package parse error: {error}"))
}

fn portable_imports(
    path: &Path,
    program: &[harn_parser::SNode],
    module_ids: &BTreeMap<std::path::PathBuf, String>,
) -> Result<Vec<PortableImport>, String> {
    let mut imports = Vec::new();
    for node in program {
        let (raw_path, selected_names, namespace_alias, is_pub) = match &node.node {
            harn_parser::Node::ImportDecl { path, is_pub } => (path.clone(), None, None, *is_pub),
            harn_parser::Node::SelectiveImport {
                names,
                path,
                is_pub,
            } => (path.clone(), Some(names.clone()), None, *is_pub),
            harn_parser::Node::NamespaceImport {
                alias,
                path,
                is_pub,
            } => (path.clone(), None, Some(alias.clone()), *is_pub),
            _ => continue,
        };
        let target = harn_modules::resolve_import_path(path, &raw_path).ok_or_else(|| {
            format!(
                "portable package cannot resolve import `{raw_path}` from {}",
                path.display()
            )
        })?;
        let target = harn_modules::canonical_path(&target);
        let target_id = module_ids.get(&target).ok_or_else(|| {
            format!(
                "portable package import `{raw_path}` from {} targets the package root or an unresolved module",
                path.display()
            )
        })?;
        imports.push(PortableImport {
            path: raw_path,
            target: target_id.clone(),
            selected_names,
            namespace_alias,
            is_pub,
        });
    }
    Ok(imports)
}

fn portable_export_kind(kind: harn_modules::DefKind) -> PortableExportKind {
    match kind {
        harn_modules::DefKind::Function => PortableExportKind::Function,
        harn_modules::DefKind::Pipeline => PortableExportKind::Pipeline,
        harn_modules::DefKind::Tool => PortableExportKind::Tool,
        harn_modules::DefKind::Skill => PortableExportKind::Skill,
        harn_modules::DefKind::EvalPack => PortableExportKind::EvalPack,
        harn_modules::DefKind::Struct => PortableExportKind::Struct,
        harn_modules::DefKind::Enum => PortableExportKind::Enum,
        harn_modules::DefKind::Interface => PortableExportKind::Interface,
        harn_modules::DefKind::Type => PortableExportKind::Type,
        harn_modules::DefKind::Variable | harn_modules::DefKind::Parameter => {
            PortableExportKind::Variable
        }
    }
}

fn render_diagnostics(diagnostics: Vec<harn_kernel::Diagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>()
        .join("\n")
}
