use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    Chunk, CompiledFunction, Compiler, CompilerOptions, PortableExportKind, PortableImport,
    PortableSourcePackage,
};

use self::wire::{encode_wire_program, ArtifactReader, WireProgram};

mod validation;
mod wire;

const MAGIC: &[u8; 8] = b"HARNPK01";
pub const ARTIFACT_VERSION: u16 = 4;
/// Maximum UTF-8 source size accepted by every portable compiler adapter.
const HEADER_BYTES: usize = 8 + 2 + 2 + 4 + 32;
const SEMANTIC_ABI_DOMAIN: &[u8] = b"harn-portable-kernel-semantic-abi-v4\0";

/// Hex fingerprint of every opcode, portable builtin, and capability contract
/// that contributes to artifact execution semantics.
pub fn semantic_abi_fingerprint_hex() -> String {
    validation::semantic_abi_fingerprint()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub struct ArtifactLimits {
    pub max_bytes: usize,
    pub max_chunks: usize,
    pub max_functions: usize,
    pub max_instructions: usize,
    pub max_constants: usize,
    pub max_string_bytes: usize,
    pub max_metadata_entries: usize,
    pub max_type_nodes: usize,
    pub max_type_depth: usize,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_chunks: 16_384,
            max_functions: 16_384,
            max_instructions: 4 * 1024 * 1024,
            max_constants: 1_048_576,
            max_string_bytes: 4 * 1024 * 1024,
            max_metadata_entries: 1_048_576,
            max_type_nodes: 262_144,
            max_type_depth: 128,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl Diagnostic {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            line: None,
            column: None,
        }
    }

    fn artifact(code: &str, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Function,
    Pipeline,
}

impl std::str::FromStr for EntryKind {
    type Err = Diagnostic;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "function" => Ok(Self::Function),
            "pipeline" => Ok(Self::Pipeline),
            _ => Err(Diagnostic::new(
                "entry_kind",
                format!("entry kind `{name}` is invalid; use `function` or `pipeline`"),
            )),
        }
    }
}

impl EntryKind {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Pipeline => "pipeline",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProgramArtifact {
    bytes: Arc<[u8]>,
    digest: [u8; 32],
    image: Arc<Chunk>,
    entry: String,
    entry_kind: EntryKind,
    expects_harness: bool,
    root_imports: Arc<[PortableImport]>,
    modules: Arc<[ProgramModule]>,
}

/// One compiled module embedded in a portable program package.
///
/// The chunks and functions are immutable and share their allocation across
/// native threads and worker instances. Runtime environments are created per
/// execution, so module state never leaks between dispatches.
#[derive(Debug, Clone)]
pub struct ProgramModule {
    id: String,
    imports: Arc<[PortableImport]>,
    init: Option<Arc<Chunk>>,
    functions: Arc<BTreeMap<String, Arc<CompiledFunction>>>,
    exports: Arc<BTreeMap<String, PortableExportKind>>,
}

impl ProgramModule {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn imports(&self) -> &[PortableImport] {
        &self.imports
    }

    pub(crate) fn init(&self) -> Option<&Arc<Chunk>> {
        self.init.as_ref()
    }

    pub(crate) fn functions(&self) -> &BTreeMap<String, Arc<CompiledFunction>> {
        &self.functions
    }

    pub(crate) fn exports(&self) -> &BTreeMap<String, PortableExportKind> {
        &self.exports
    }
}

impl ProgramArtifact {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn digest_hex(&self) -> String {
        self.digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    pub fn image(&self) -> &Arc<Chunk> {
        &self.image
    }
    pub fn entry(&self) -> &str {
        &self.entry
    }
    pub fn entry_kind(&self) -> EntryKind {
        self.entry_kind.clone()
    }
    pub fn expects_harness(&self) -> bool {
        self.expects_harness
    }

    /// Resolved import edges of the root source module.
    pub fn root_imports(&self) -> &[PortableImport] {
        &self.root_imports
    }

    /// Embedded module closure, excluding the root source module.
    pub fn modules(&self) -> &[ProgramModule] {
        &self.modules
    }

    pub fn decode(bytes: &[u8], limits: ArtifactLimits) -> Result<Self, Diagnostic> {
        if bytes.len() > limits.max_bytes {
            return Err(Diagnostic::artifact(
                "artifact_too_large",
                format!(
                    "artifact has {} bytes; limit is {}",
                    bytes.len(),
                    limits.max_bytes
                ),
            ));
        }
        if bytes.len() < HEADER_BYTES {
            return Err(Diagnostic::artifact(
                "artifact_truncated",
                "artifact header is truncated",
            ));
        }
        if &bytes[..8] != MAGIC {
            return Err(Diagnostic::artifact(
                "artifact_magic",
                "artifact magic does not identify a portable Harn program",
            ));
        }
        let version = u16::from_be_bytes([bytes[8], bytes[9]]);
        if version != ARTIFACT_VERSION {
            return Err(Diagnostic::artifact(
                "artifact_version",
                format!("artifact version {version} is not supported; expected {ARTIFACT_VERSION}"),
            ));
        }
        let flags = u16::from_be_bytes([bytes[10], bytes[11]]);
        if flags != 0 {
            return Err(Diagnostic::artifact(
                "artifact_features",
                format!("artifact uses unsupported feature bits 0x{flags:04x}"),
            ));
        }
        let payload_len =
            u32::from_be_bytes(bytes[12..16].try_into().expect("header length checked")) as usize;
        let total = HEADER_BYTES.checked_add(payload_len).ok_or_else(|| {
            Diagnostic::artifact("artifact_too_large", "artifact length overflow")
        })?;
        if total != bytes.len() {
            return Err(Diagnostic::artifact(
                if total > bytes.len() {
                    "artifact_truncated"
                } else {
                    "artifact_trailing_bytes"
                },
                format!(
                    "header declares {payload_len} payload bytes but {} are present",
                    bytes.len() - HEADER_BYTES
                ),
            ));
        }
        let expected_digest: [u8; 32] = bytes[16..48].try_into().expect("header length checked");
        let payload = &bytes[HEADER_BYTES..];
        let digest = *blake3::hash(payload).as_bytes();
        if digest != expected_digest {
            return Err(Diagnostic::artifact(
                "artifact_corrupt",
                "artifact payload digest does not match its header",
            ));
        }
        let wire = ArtifactReader::new(payload, limits).read_program()?;
        let built = wire.validate_and_build(limits)?;
        Ok(Self {
            bytes: Arc::from(bytes),
            digest,
            image: Arc::new(built.root),
            entry: wire.entry,
            entry_kind: wire.entry_kind,
            expects_harness: wire.expects_harness,
            root_imports: built.root_imports.into(),
            modules: built
                .modules
                .into_iter()
                .map(ProgramModule::from_built)
                .collect::<Vec<_>>()
                .into(),
        })
    }
}

impl ProgramModule {
    fn from_built(module: wire::BuiltModule) -> Self {
        Self {
            id: module.id,
            imports: module.imports.into(),
            init: module.init.map(Arc::new),
            functions: Arc::new(module.functions),
            exports: Arc::new(module.exports),
        }
    }
}

/// Parsed, graph-resolved source input for [`compile_program_package`].
///
/// A host (normally the CLI or a build service) owns path resolution and
/// typechecking. The kernel receives only deterministic ASTs and the narrow
/// import/export projections needed to link them, which keeps browser builds
/// free of filesystem and package-manager authority.
#[derive(Debug, Clone)]
pub struct PortableModuleSource {
    pub id: String,
    pub program: Vec<harn_parser::SNode>,
    pub imports: Vec<PortableImport>,
    pub exports: BTreeMap<String, PortableExportKind>,
    pub imported_enum_candidates: Vec<String>,
    pub source_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PortablePackageSource {
    pub root_program: Vec<harn_parser::SNode>,
    pub root_imports: Vec<PortableImport>,
    pub modules: Vec<PortableModuleSource>,
}

/// Parse a resolved source package manifest using the canonical Harn lexer and
/// parser, then compile it through [`compile_program_package`]. This keeps the
/// browser and native source-to-artifact path on one frontend; hosts still own
/// import resolution and typechecking before serializing the manifest.
pub fn compile_source_package(
    package: PortableSourcePackage,
    entry: &str,
    entry_kind: EntryKind,
) -> Result<ProgramArtifact, Vec<Diagnostic>> {
    crate::portable_builtin::install_source_contracts();
    let root_program = parse_source_module(&package.root_source, None)?;
    let mut modules = Vec::with_capacity(package.modules.len());
    for module in package.modules {
        let source_file = module.source_file;
        let program = parse_source_module(&module.source, source_file.as_deref())?;
        modules.push(PortableModuleSource {
            id: module.id,
            program,
            imports: module.imports,
            exports: module.exports,
            imported_enum_candidates: module.imported_enum_candidates,
            source_file,
        });
    }
    compile_program_package(
        PortablePackageSource {
            root_program,
            root_imports: package.root_imports,
            modules,
        },
        entry,
        entry_kind,
    )
}

fn parse_source_module(
    source: &str,
    source_file: Option<&str>,
) -> Result<Vec<harn_parser::SNode>, Vec<Diagnostic>> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|error| {
        vec![Diagnostic {
            code: "compile_frontend".to_string(),
            message: format!("{}: {error}", source_file.unwrap_or("portable source")),
            line: None,
            column: None,
        }]
    })?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser.parse().map_err(|error| {
        vec![Diagnostic {
            code: "compile_frontend".to_string(),
            message: format!("{}: {error}", source_file.unwrap_or("portable source")),
            line: None,
            column: None,
        }]
    })
}

/// Compile one source module into the current artifact shape. Imports are rejected
/// here because only [`compile_program_package`] has a resolved closure to
/// bind them against; this prevents a second, path-sensitive loader from
/// appearing in the browser adapter.
pub fn compile_program_package(
    package: PortablePackageSource,
    entry: &str,
    entry_kind: EntryKind,
) -> Result<ProgramArtifact, Vec<Diagnostic>> {
    crate::portable_builtin::install_source_contracts();
    let frontend_diagnostics = package_typecheck_diagnostics(&package);
    if !frontend_diagnostics.is_empty() {
        return Err(frontend_diagnostics);
    }
    let options = CompilerOptions::portable_artifact();
    let compiled = match entry_kind {
        EntryKind::Function => Compiler::with_options(options)
            .compile_named_function_entry(&package.root_program, entry),
        EntryKind::Pipeline => Compiler::with_options(options).compile_named_pipeline_entry(
            &package.root_program,
            entry,
            None,
        ),
    }
    .map_err(|error| {
        vec![Diagnostic {
            code: "compile_bytecode".to_string(),
            message: error.message,
            line: Some(error.line),
            column: None,
        }]
    })?;
    let mut compiled_modules = Vec::with_capacity(package.modules.len());
    for module in package.modules {
        let image = Compiler::with_options(options)
            .compile_portable_module(
                module.id,
                &module.program,
                module.imports,
                module.exports,
                &module.imported_enum_candidates,
                module.source_file,
            )
            .map_err(|error| {
                vec![Diagnostic {
                    code: "compile_bytecode".to_string(),
                    message: error.message,
                    line: Some(error.line),
                    column: None,
                }]
            })?;
        compiled_modules.push(image);
    }
    let wire = WireProgram::from_package(
        &compiled.bootstrap,
        &compiled_modules,
        package.root_imports,
        entry.to_string(),
        entry_kind,
        compiled.expects_harness,
    )
    .map_err(|diagnostic| vec![diagnostic])?;
    wire.validate_metadata(ArtifactLimits::default())
        .map_err(|error| vec![error])?;
    let payload = encode_wire_program(&wire).map_err(|error| vec![error])?;
    encode_artifact_payload(payload)
}

/// Type-check a closed, host-resolved package without consulting paths or a
/// package manager. This deliberately checks the requested root, matching
/// `harn check <entry>`: dependency declarations contribute signatures and
/// private supporting types, while dependency bodies remain their owning
/// modules' responsibility. Re-checking every transitive stdlib body here
/// would create a stricter, parallel frontend for portable builds.
fn package_typecheck_diagnostics(package: &PortablePackageSource) -> Vec<Diagnostic> {
    let mut module_ids = HashSet::with_capacity(package.modules.len());
    for module in &package.modules {
        if module.id.is_empty() || !module_ids.insert(module.id.as_str()) {
            return vec![Diagnostic::artifact(
                "artifact_invalid_module",
                "package contains an empty or duplicate module id",
            )];
        }
    }
    if let Err(diagnostic) =
        wire::validate_import_targets(&package.root_imports, &module_ids, "root")
    {
        return vec![diagnostic];
    }
    for module in &package.modules {
        if let Err(diagnostic) =
            wire::validate_import_targets(&module.imports, &module_ids, &module.id)
        {
            return vec![diagnostic];
        }
    }
    let modules = package
        .modules
        .iter()
        .map(|module| (module.id.as_str(), module))
        .collect::<BTreeMap<_, _>>();
    let mut diagnostics = Vec::new();
    typecheck_package_module(
        "root",
        &package.root_program,
        &package.root_imports,
        &modules,
        &mut diagnostics,
    );
    diagnostics
}

fn typecheck_package_module(
    owner: &str,
    program: &[harn_parser::SNode],
    imports: &[PortableImport],
    modules: &BTreeMap<&str, &PortableModuleSource>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut imported_names = HashSet::new();
    let mut imported_declarations = Vec::new();
    let mut imported_declaration_names = HashSet::new();
    let mut namespace_imports = Vec::new();
    for import in imports {
        let Some(target) = modules.get(import.target.as_str()).copied() else {
            // Artifact metadata validation owns the stable missing-target
            // diagnostic. Avoid manufacturing a second checker error here.
            continue;
        };
        if let Some(alias) = &import.namespace_alias {
            imported_names.insert(alias.clone());
            namespace_imports.push((
                alias.clone(),
                harn_parser::NamespaceImportBinding {
                    module_path: import.path.clone(),
                    members: target.exports.keys().cloned().collect::<BTreeSet<_>>(),
                    // Members stay gradual on the artifact path. Lowering a
                    // signature needs the defining module's type declarations
                    // to inline named types (see
                    // `harn-modules::namespace_member_signatures`), and an
                    // artifact carries resolved exports rather than that
                    // declaration graph. Empty preserves the pre-#6172
                    // behavior here instead of checking against a guess.
                    member_types: std::collections::BTreeMap::new(),
                    member_param_names: std::collections::BTreeMap::new(),
                    member_required_params: std::collections::BTreeMap::new(),
                    member_type_predicates: std::collections::BTreeMap::new(),
                },
            ));
            continue;
        }
        let names = import
            .selected_names
            .clone()
            .unwrap_or_else(|| target.exports.keys().cloned().collect());
        for name in names {
            imported_names.insert(name.clone());
            if let Some((declaration_owner, declaration)) =
                resolve_export_declaration(target, &name, modules, &mut BTreeSet::new())
            {
                if imported_declaration_names.insert(name) {
                    imported_declarations.push(declaration);
                }
                collect_private_type_declarations(
                    declaration_owner,
                    &mut imported_declaration_names,
                    &mut imported_declarations,
                );
            }
        }
        // Imported callable signatures may refer to types that are private to
        // their defining module. The canonical module graph makes those types
        // visible to the checker (but not to source-level name lookup); carry
        // the same declaration context in this path-independent projection.
        collect_private_type_declarations(
            target,
            &mut imported_declaration_names,
            &mut imported_declarations,
        );
    }

    let checker = harn_parser::TypeChecker::new()
        .with_imported_names(imported_names)
        .with_imported_type_decls(imported_declarations.clone())
        .with_imported_callable_decls(imported_declarations)
        .with_namespace_imports(namespace_imports);
    for error in checker
        .check(program)
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == harn_parser::DiagnosticSeverity::Error)
    {
        diagnostics.push(Diagnostic {
            code: error.code.as_str().to_string(),
            message: format!("{owner}: {}", error.message),
            line: error
                .span
                .as_ref()
                .map(|span| span.line.try_into().unwrap_or(u32::MAX)),
            column: error
                .span
                .as_ref()
                .map(|span| span.column.try_into().unwrap_or(u32::MAX)),
        });
    }
}

fn resolve_export_declaration<'a>(
    module: &'a PortableModuleSource,
    name: &str,
    modules: &BTreeMap<&str, &'a PortableModuleSource>,
    visiting: &mut BTreeSet<String>,
) -> Option<(&'a PortableModuleSource, harn_parser::SNode)> {
    if !module.exports.contains_key(name) || !visiting.insert(module.id.clone()) {
        return None;
    }
    if let Some(declaration) = module
        .program
        .iter()
        .find(|node| declaration_name(node).is_some_and(|candidate| candidate == name))
        .cloned()
    {
        visiting.remove(&module.id);
        return Some((module, declaration));
    }
    for import in module.imports.iter().filter(|import| import.is_pub) {
        if import
            .namespace_alias
            .as_deref()
            .is_some_and(|alias| alias == name)
        {
            continue;
        }
        if import
            .selected_names
            .as_ref()
            .is_some_and(|names| !names.iter().any(|candidate| candidate == name))
        {
            continue;
        }
        let Some(target) = modules.get(import.target.as_str()).copied() else {
            continue;
        };
        if let Some(declaration) = resolve_export_declaration(target, name, modules, visiting) {
            visiting.remove(&module.id);
            return Some(declaration);
        }
    }
    visiting.remove(&module.id);
    None
}

fn collect_private_type_declarations(
    module: &PortableModuleSource,
    names: &mut HashSet<String>,
    declarations: &mut Vec<harn_parser::SNode>,
) {
    for declaration in module
        .program
        .iter()
        .filter(|node| is_type_declaration(node))
    {
        let Some(name) = declaration_name(declaration).map(ToOwned::to_owned) else {
            continue;
        };
        if names.insert(name) {
            declarations.push(declaration.clone());
        }
    }
}

fn declaration_name(node: &harn_parser::SNode) -> Option<&str> {
    use harn_parser::{BindingPattern, Node};

    let node = match &node.node {
        Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => node,
    };
    match &node.node {
        Node::FnDecl { name, .. }
        | Node::Pipeline { name, .. }
        | Node::ToolDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. }
        | Node::TypeDecl { name, .. } => Some(name),
        Node::SkillDecl { name, .. } => Some(name),
        Node::EvalPackDecl { binding_name, .. } => Some(binding_name),
        Node::LetBinding {
            pattern: BindingPattern::Identifier(name),
            ..
        }
        | Node::ConstBinding {
            pattern: BindingPattern::Identifier(name),
            ..
        } => Some(name),
        _ => None,
    }
}

fn is_type_declaration(node: &harn_parser::SNode) -> bool {
    let node = match &node.node {
        harn_parser::Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => node,
    };
    matches!(
        node.node,
        harn_parser::Node::StructDecl { .. }
            | harn_parser::Node::EnumDecl { .. }
            | harn_parser::Node::InterfaceDecl { .. }
            | harn_parser::Node::TypeDecl { .. }
    )
}

pub fn compile_program(
    source: &str,
    entry: &str,
    entry_kind: EntryKind,
) -> Result<ProgramArtifact, Vec<Diagnostic>> {
    if source.len() > crate::PORTABLE_MAX_SOURCE_BYTES {
        return Err(vec![Diagnostic::new(
            "source_too_large",
            "source exceeds the portable compiler's 1 MiB limit",
        )]);
    }
    crate::portable_builtin::install_source_contracts();
    let program = harn_parser::check_source_strict(source).map_err(|error| {
        vec![Diagnostic {
            code: "compile_frontend".to_string(),
            message: error.to_string(),
            line: None,
            column: None,
        }]
    })?;
    let compiled = match entry_kind {
        EntryKind::Function => Compiler::with_options(CompilerOptions::portable_artifact())
            .compile_named_function_entry(&program, entry),
        EntryKind::Pipeline => Compiler::with_options(CompilerOptions::portable_artifact())
            .compile_named_pipeline_entry(&program, entry, None),
    }
    .map_err(|error| {
        vec![Diagnostic {
            code: "compile_bytecode".to_string(),
            message: error.message,
            line: Some(error.line),
            column: None,
        }]
    })?;
    let wire = WireProgram::from_image(
        &compiled.bootstrap,
        entry.to_string(),
        entry_kind,
        compiled.expects_harness,
    )
    .map_err(|diagnostic| vec![diagnostic])?;
    wire.validate_metadata(ArtifactLimits::default())
        .map_err(|error| vec![error])?;
    let payload = encode_wire_program(&wire).map_err(|error| vec![error])?;
    encode_artifact_payload(payload)
}

fn encode_artifact_payload(payload: Vec<u8>) -> Result<ProgramArtifact, Vec<Diagnostic>> {
    if payload.len() > u32::MAX as usize {
        return Err(vec![Diagnostic::artifact(
            "artifact_too_large",
            "artifact payload exceeds the format's u32 length",
        )]);
    }
    let digest = *blake3::hash(&payload).as_bytes();
    let mut bytes = Vec::with_capacity(HEADER_BYTES + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&ARTIFACT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&digest);
    bytes.extend_from_slice(&payload);
    ProgramArtifact::decode(&bytes, ArtifactLimits::default()).map_err(|error| vec![error])
}

#[cfg(test)]
mod tests;
