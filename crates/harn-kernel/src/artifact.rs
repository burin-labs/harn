use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Chunk, Compiler, CompilerOptions};

use self::wire::{encode_wire_program, ArtifactReader, WireProgram};

mod validation;
mod wire;

const MAGIC: &[u8; 8] = b"HARNPK01";
pub const ARTIFACT_VERSION: u16 = 2;
/// Maximum UTF-8 source size accepted by every portable compiler adapter.
pub const PORTABLE_SOURCE_MAX_BYTES: usize = 1024 * 1024;
const HEADER_BYTES: usize = 8 + 2 + 2 + 4 + 32;
const SEMANTIC_ABI_DOMAIN: &[u8] = b"harn-portable-kernel-semantic-abi-v1\0";

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
        let image = wire.validate_and_build(limits)?;
        Ok(Self {
            bytes: Arc::from(bytes),
            digest,
            image: Arc::new(image),
            entry: wire.entry,
            entry_kind: wire.entry_kind,
            expects_harness: wire.expects_harness,
        })
    }
}

pub fn compile_program(
    source: &str,
    entry: &str,
    entry_kind: EntryKind,
) -> Result<ProgramArtifact, Vec<Diagnostic>> {
    if source.len() > PORTABLE_SOURCE_MAX_BYTES {
        return Err(vec![Diagnostic::new(
            "source_too_large",
            "source exceeds the portable compiler's 1 MiB limit",
        )]);
    }
    let program = harn_parser::check_source_strict(source).map_err(|error| {
        vec![Diagnostic {
            code: "compile_frontend".to_string(),
            message: error.to_string(),
            line: None,
            column: None,
        }]
    })?;
    let portable_diagnostics = portable_frontend_diagnostics(&program);
    if !portable_diagnostics.is_empty() {
        return Err(portable_diagnostics);
    }
    let compiled = match entry_kind {
        EntryKind::Function => Compiler::with_options(CompilerOptions::optimized())
            .compile_named_function_entry(&program, entry),
        EntryKind::Pipeline => Compiler::with_options(CompilerOptions::optimized())
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
    let artifact =
        ProgramArtifact::decode(&bytes, ArtifactLimits::default()).map_err(|error| vec![error])?;
    Ok(artifact)
}

/// Reject frontend constructs whose canonical bytecode currently delegates a
/// semantic check to a host-only VM builtin. Keeping this boundary structural
/// prevents a compiled artifact from failing later with a misleading missing-
/// builtin error and gives native and Wasm callers the same exact diagnostic.
fn portable_frontend_diagnostics(program: &[harn_parser::SNode]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    harn_parser::visit::walk_program(program, &mut |node| {
        let (callable, params) = match &node.node {
            harn_parser::Node::Pipeline { name, params, .. }
            | harn_parser::Node::FnDecl { name, params, .. }
            | harn_parser::Node::ToolDecl { name, params, .. } => {
                (name.as_str(), params.as_slice())
            }
            harn_parser::Node::Closure { params, .. } => ("<closure>", params.as_slice()),
            _ => return,
        };
        for parameter in params {
            if parameter.type_expr.is_some() && parameter.default_value.is_some() {
                diagnostics.push(Diagnostic {
                    code: "unsupported_portable_typed_default".to_string(),
                    message: format!(
                        "typed default parameter `{}.{}` is outside Portable Kernel v1; use an untyped default or initialize and validate it in the function body",
                        callable, parameter.name
                    ),
                    line: Some(parameter.span.line.try_into().unwrap_or(u32::MAX)),
                    column: Some(parameter.span.column.try_into().unwrap_or(u32::MAX)),
                });
            }
        }
    });
    diagnostics
}

#[cfg(test)]
mod tests;
