//! Identity and capabilities of a generated ordinary MCP worker.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use ts_rs::TS;

use super::ToolCatalogSchemaVersion;
use crate::codegen::{CodegenCancellation, CodegenIdentity};

pub const HARN_NATIVE_BINDING_META_KEY: &str = "com.harnlang/nativeBinding";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub enum NativeMcpBindingVersion {
    #[serde(rename = "harn-tools-mcp/1.0")]
    V1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct NativeMcpOperationBinding {
    pub operation_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct NativeMcpBindingProfile {
    pub schema_version: NativeMcpBindingVersion,
    pub catalog_schema_version: ToolCatalogSchemaVersion,
    pub identity: CodegenIdentity,
    pub operations: Vec<NativeMcpOperationBinding>,
    #[schemars(range(min = 1))]
    pub max_concurrency: u32,
    pub cancellation: CodegenCancellation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMcpBindingViolation {
    Identity,
    OperationCensus,
    Concurrency,
    Discovery,
}

impl std::fmt::Display for NativeMcpBindingViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native MCP binding {self:?} mismatch")
    }
}

impl std::error::Error for NativeMcpBindingViolation {}

impl NativeMcpBindingProfile {
    /// The independent Harn contract owns every worker handshake fact.
    pub fn for_ir(ir: &crate::codegen::ValidatedCodegenIr, max_concurrency: NonZeroU32) -> Self {
        Self {
            schema_version: NativeMcpBindingVersion::V1,
            catalog_schema_version: ToolCatalogSchemaVersion::V2,
            identity: ir.ir().identity.clone(),
            operations: ir
                .ir()
                .operations
                .iter()
                .map(|operation| NativeMcpOperationBinding {
                    operation_id: operation.id.clone(),
                    name: operation.name.clone(),
                })
                .collect(),
            max_concurrency: max_concurrency.get(),
            cancellation: CodegenCancellation::McpRequest,
        }
    }

    /// Verify identity, capabilities and the exact opaque unary MCP projection
    /// before any native handler can receive an invocation.
    pub fn verify(
        &self,
        ir: &crate::codegen::ValidatedCodegenIr,
        max_concurrency: NonZeroU32,
        tools: &[serde_json::Value],
    ) -> Result<(), NativeMcpBindingViolation> {
        let expected = Self::for_ir(ir, max_concurrency);
        if self.identity != expected.identity {
            return Err(NativeMcpBindingViolation::Identity);
        }
        if self.max_concurrency != expected.max_concurrency {
            return Err(NativeMcpBindingViolation::Concurrency);
        }
        fn census(operations: &[NativeMcpOperationBinding]) -> Option<BTreeMap<&str, &str>> {
            let map: BTreeMap<_, _> = operations
                .iter()
                .map(|operation| (operation.name.as_str(), operation.operation_id.as_str()))
                .collect();
            (map.len() == operations.len()).then_some(map)
        }
        if !matches!((census(&self.operations), census(&expected.operations)),
            (Some(actual), Some(expected)) if actual == expected)
        {
            return Err(NativeMcpBindingViolation::OperationCensus);
        }
        fn discovery(tools: &[serde_json::Value]) -> Option<BTreeMap<&str, &serde_json::Value>> {
            let entries: Option<BTreeMap<_, _>> = tools
                .iter()
                .map(|tool| Some((tool.get("name")?.as_str()?, tool)))
                .collect();
            entries.filter(|entries| entries.len() == tools.len())
        }
        if !matches!((discovery(tools), discovery(&ir.ir().mcp_tools)),
            (Some(actual), Some(expected)) if actual == expected)
        {
            return Err(NativeMcpBindingViolation::Discovery);
        }
        Ok(())
    }
}
