use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_events::{ToolCallErrorCategory, ToolCallStatus, ToolExecutor};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

use super::manifest::{BindingManifest, BindingManifestEntry};

pub const COMPOSITION_EXECUTION_SCHEMA_VERSION: u32 = 1;

/// Stable failure taxonomy for a composition run. Tool-level failures stay on
/// [`CompositionChildResult`]; this classifies why the parent composition
/// itself failed or stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionFailureCategory {
    /// The snippet language is unknown or not enabled by the current host.
    UnsupportedLanguage,
    /// The snippet or manifest did not validate before execution.
    SchemaValidation,
    /// Capability policy rejected the requested side-effect ceiling or a child
    /// operation.
    PolicyDenied,
    /// A child binding returned an error.
    ChildToolError,
    /// The executor failed before it could attribute the error to a child call.
    ExecutionError,
    /// The run exceeded its time or step budget.
    Timeout,
    /// The host or caller cancelled the run.
    Cancelled,
    /// Fallback when a producer cannot classify the failure.
    Unknown,
}

impl CompositionFailureCategory {
    pub const ALL: [Self; 8] = [
        Self::UnsupportedLanguage,
        Self::SchemaValidation,
        Self::PolicyDenied,
        Self::ChildToolError,
        Self::ExecutionError,
        Self::Timeout,
        Self::Cancelled,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "unsupported_language",
            Self::SchemaValidation => "schema_validation",
            Self::PolicyDenied => "policy_denied",
            Self::ChildToolError => "child_tool_error",
            Self::ExecutionError => "execution_error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Identity and policy envelope for one composition run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositionRunEnvelope {
    /// Runtime-unique id used to correlate child calls and terminal events.
    pub run_id: String,
    /// Snippet frontend (`harn`, `typescript`, `javascript`, ...).
    pub language: String,
    /// `sha256:<hex>` digest over the language and snippet bytes.
    pub snippet_hash: String,
    /// `sha256:<hex>` digest over the binding manifest shown to the model.
    pub binding_manifest_hash: String,
    /// Highest side-effect level requested by the parent run.
    pub requested_side_effect_ceiling: SideEffectLevel,
    /// Captured stdout-like text emitted by the composition executor.
    pub stdout: Option<String>,
    /// Captured stderr-like text emitted by the composition executor.
    pub stderr: Option<String>,
    /// Artifact descriptors/handles emitted by the composition executor.
    pub artifacts: Vec<Value>,
    /// Structured result returned by the snippet.
    pub result: Option<Value>,
    /// Parent-run failure class, absent for successful finishes.
    pub failure_category: Option<CompositionFailureCategory>,
    /// Human-readable parent-run error, absent for successful finishes.
    pub error: Option<String>,
    /// Runtime wall-clock duration when a producer has measured it.
    pub duration_ms: Option<u64>,
    /// Forward-compatible producer metadata. Consumers must ignore unknown keys.
    pub metadata: Value,
}

impl Default for CompositionRunEnvelope {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            language: String::new(),
            snippet_hash: String::new(),
            binding_manifest_hash: String::new(),
            requested_side_effect_ceiling: SideEffectLevel::ReadOnly,
            stdout: None,
            stderr: None,
            artifacts: Vec::new(),
            result: None,
            failure_category: None,
            error: None,
            duration_ms: None,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

impl CompositionRunEnvelope {
    pub fn read_only(
        run_id: impl Into<String>,
        language: impl Into<String>,
        snippet_hash: impl Into<String>,
        binding_manifest_hash: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            language: language.into(),
            snippet_hash: snippet_hash.into(),
            binding_manifest_hash: binding_manifest_hash.into(),
            requested_side_effect_ceiling: SideEffectLevel::ReadOnly,
            ..Self::default()
        }
    }
}

/// Child tool call made by a composition snippet. This is intentionally close
/// to `AgentEvent::ToolCall`, but includes parent-run correlation and the
/// policy/annotation context the composition executor used when deciding
/// whether the call was allowed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositionChildCall {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub operation_index: u64,
    pub annotations: Option<ToolAnnotations>,
    pub requested_side_effect_level: SideEffectLevel,
    pub policy_context: Value,
    pub raw_input: Value,
}

impl Default for CompositionChildCall {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            operation_index: 0,
            annotations: None,
            requested_side_effect_level: SideEffectLevel::None,
            policy_context: Value::Object(serde_json::Map::new()),
            raw_input: Value::Null,
        }
    }
}

/// Terminal or intermediate result for a child binding operation. Consumers
/// should pair this with the corresponding [`CompositionChildCall`] to recover
/// the policy annotations and requested side-effect level for the operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositionChildResult {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub operation_index: u64,
    pub status: ToolCallStatus,
    pub raw_output: Option<Value>,
    pub error: Option<String>,
    pub error_category: Option<ToolCallErrorCategory>,
    pub executor: Option<ToolExecutor>,
    pub duration_ms: Option<u64>,
    pub execution_duration_ms: Option<u64>,
}

impl Default for CompositionChildResult {
    fn default() -> Self {
        Self {
            run_id: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            operation_index: 0,
            status: ToolCallStatus::Pending,
            raw_output: None,
            error: None,
            error_category: None,
            executor: None,
            duration_ms: None,
            execution_duration_ms: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositionExecutionLimits {
    pub max_operations: u64,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: u64,
}

impl Default for CompositionExecutionLimits {
    fn default() -> Self {
        Self {
            max_operations: 64,
            timeout_ms: Some(10_000),
            max_output_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CompositionExecutionRequest {
    pub session_id: Option<String>,
    pub run_id: String,
    pub language: String,
    pub snippet: String,
    pub manifest: BindingManifest,
    pub requested_side_effect_ceiling: SideEffectLevel,
    pub limits: CompositionExecutionLimits,
    pub metadata: Value,
}

impl Default for CompositionExecutionRequest {
    fn default() -> Self {
        Self {
            session_id: None,
            run_id: String::new(),
            language: "harn".to_string(),
            snippet: String::new(),
            manifest: BindingManifest::default(),
            requested_side_effect_ceiling: SideEffectLevel::ReadOnly,
            limits: CompositionExecutionLimits::default(),
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositionExecutionReport {
    pub schema_version: u32,
    pub ok: bool,
    pub run: CompositionRunEnvelope,
    pub child_calls: Vec<CompositionChildCall>,
    pub child_results: Vec<CompositionChildResult>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositionToolOutput {
    pub value: Option<Value>,
    pub error: Option<String>,
    pub error_category: Option<ToolCallErrorCategory>,
    pub executor: Option<ToolExecutor>,
}

impl CompositionToolOutput {
    pub fn ok(value: Value) -> Self {
        Self {
            value: Some(value),
            error: None,
            error_category: None,
            executor: Some(ToolExecutor::HarnBuiltin),
        }
    }

    pub fn error(message: impl Into<String>, category: ToolCallErrorCategory) -> Self {
        Self {
            value: None,
            error: Some(message.into()),
            error_category: Some(category),
            executor: Some(ToolExecutor::HarnBuiltin),
        }
    }
}

#[async_trait::async_trait]
pub trait CompositionToolHost: Send + Sync {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput;
}
