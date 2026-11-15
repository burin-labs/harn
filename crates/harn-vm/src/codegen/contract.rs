use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::tool_registry::{
    McpOutputEnvelope, ToolCliSpec, ToolGovernance, ToolPolicy, ToolTaskSupport,
};
use crate::wire::{WireSourceRef, WireType, WireTypeId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub enum CodegenSchemaVersion {
    #[serde(rename = "harn-codegen/1.0")]
    V1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub enum CodegenRequestVersion {
    #[serde(rename = "harn-codegen-request/1.0")]
    V1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub enum CodegenResponseVersion {
    #[serde(rename = "harn-codegen-response/1.0")]
    V1,
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum CodegenFeature {
    ExactJsonInteger,
    Base64Bytes,
    CanonicalDecimal,
    Sets,
    Tuples,
    StringMaps,
    OptionalFields,
    NominalTypes,
    RecursiveTypes,
    DisjointUnions,
    DeclaredApplicationErrors,
    McpTasks,
    RequestCancellation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum CodegenRole {
    Client,
    Server,
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenIdentity {
    pub catalog_digest: String,
    pub binding_digest: String,
    pub ir_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenPackage {
    pub id: String,
    pub name: String,
    pub source: WireSourceRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodegenOutput {
    Missing,
    Value {
        type_id: WireTypeId,
        mcp_envelope: McpOutputEnvelope,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodegenBindingKind {
    HarnExport,
    Registry,
    OpenApi,
    Mcp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenBinding {
    pub kind: CodegenBindingKind,
    pub operation: String,
    pub source: WireSourceRef,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodegenCancellation {
    McpRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenExecution {
    pub task_support: ToolTaskSupport,
    pub cancellation: CodegenCancellation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenOperation {
    pub id: String,
    pub name: String,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
    pub input: WireTypeId,
    pub output: CodegenOutput,
    pub application_error: Option<WireTypeId>,
    pub execution: CodegenExecution,
    pub governance: ToolGovernance,
    pub policy: Option<ToolPolicy>,
    pub cli: ToolCliSpec,
    pub binding: CodegenBinding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenIr {
    pub schema_version: CodegenSchemaVersion,
    pub required_features: Vec<CodegenFeature>,
    pub identity: CodegenIdentity,
    /// Canonical commitment payload. Only Harn interprets this catalog.
    pub catalog_canonical_json: String,
    /// Complete source authority, retained even when graph nodes are shared.
    pub sources: Vec<WireSourceRef>,
    pub package: CodegenPackage,
    pub types: Vec<WireType>,
    pub operations: Vec<CodegenOperation>,
    /// Forwarded unchanged for MCP discovery, never a backend's typing input.
    #[ts(type = "ReadonlyArray<JsonValue>")]
    pub mcp_tools: Vec<serde_json::Value>,
}

impl CodegenIr {
    /// Source authority is typed IR data, never an arbitrary JSON metadata walk.
    pub fn source_refs(&self) -> Vec<&WireSourceRef> {
        self.sources.iter().collect()
    }

    pub(crate) fn declaration_source_refs(&self) -> Vec<&WireSourceRef> {
        let mut sources = vec![&self.package.source];
        for ty in &self.types {
            sources.push(&ty.source);
            match &ty.shape {
                crate::wire::WireShape::Record { fields, .. } => {
                    sources.extend(fields.iter().map(|field| &field.source));
                }
                crate::wire::WireShape::Enum { variants, .. } => {
                    for variant in variants {
                        sources.push(&variant.source);
                        sources.extend(variant.fields.iter().map(|field| &field.source));
                    }
                }
                _ => {}
            }
        }
        for operation in &self.operations {
            sources.extend([&operation.source, &operation.binding.source]);
        }
        sources
    }
}

/// Plugins receive data only; source access and filesystem effects stay in Harn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenRequest {
    pub schema_version: CodegenRequestVersion,
    pub role: CodegenRole,
    pub ir: CodegenIr,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GeneratedRange {
    pub start: u32,
    pub end: u32,
    pub source: WireSourceRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct GeneratedArtifact {
    pub path: String,
    pub content: String,
    pub source_mappings: Vec<GeneratedRange>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenResponse {
    pub schema_version: CodegenResponseVersion,
    pub identity: CodegenIdentity,
    pub supported_features: Vec<CodegenFeature>,
    pub artifacts: Vec<GeneratedArtifact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodegenDiagnosticCode {
    InvalidJson,
    InvalidIr,
    UnsupportedVersion,
    UnsupportedFeature,
    InvalidDigest,
    DuplicateIdentity,
    DanglingType,
    InvalidSelection,
    MissingOutput,
    UnsupportedWireType,
    InvalidMcpProjection,
    ResponseIdentityMismatch,
    MalformedPluginResponse,
    PluginSpawnFailed,
    PluginTimedOut,
    PluginOutputLimit,
    PluginFailed,
    NonDeterministicOutput,
    UnsafeArtifactPath,
    DuplicateArtifactPath,
    InvalidSourceMapping,
    ArtifactWriteFailed,
    ArtifactDrift,
    SourceUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CodegenDiagnostic {
    pub code: CodegenDiagnosticCode,
    pub operation: Option<String>,
    pub type_id: Option<WireTypeId>,
    pub source: Option<WireSourceRef>,
    pub structural_path: String,
}

impl CodegenDiagnostic {
    pub fn new(code: CodegenDiagnosticCode) -> Self {
        Self {
            code,
            operation: None,
            type_id: None,
            source: None,
            structural_path: String::new(),
        }
    }
}

impl std::fmt::Display for CodegenDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "codegen {:?}", self.code)?;
        if !self.structural_path.is_empty() {
            write!(formatter, " at {}", self.structural_path)?;
        }
        Ok(())
    }
}

impl std::error::Error for CodegenDiagnostic {}

#[derive(Clone, Debug)]
pub struct ValidatedCodegenIr {
    pub(crate) ir: CodegenIr,
}

impl ValidatedCodegenIr {
    pub fn ir(&self) -> &CodegenIr {
        &self.ir
    }

    pub fn into_ir(self) -> CodegenIr {
        self.ir
    }

    pub fn canonical_json(&self) -> Vec<u8> {
        crate::canonical_json::to_vec(
            &serde_json::to_value(&self.ir).expect("validated codegen records serialize as JSON"),
        )
    }
}
