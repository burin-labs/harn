//! Language-neutral executable tool-composition contract.
//!
//! A composition run is a tiny program over already-typed tool bindings. The
//! runtime must expose it as a parent run with child tool operations, not as an
//! opaque "execute code" blob, so policy, transcript, replay, and host approval
//! surfaces can keep reasoning about each child call normally.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus, ToolExecutor};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

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

/// Stable digest for the prompt-visible snippet body.
pub fn composition_snippet_hash(language: &str, snippet: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"harn.composition.snippet.v1\0");
    hasher.update(language.as_bytes());
    hasher.update(b"\0");
    hasher.update(snippet.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Stable digest for a binding manifest value. Producers should build
/// manifests with deterministic object key order before hashing.
pub fn binding_manifest_hash(manifest: &Value) -> Result<String, serde_json::Error> {
    let canonical = serde_json::to_vec(manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(b"harn.composition.binding_manifest.v1\0");
    hasher.update(&canonical);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub const BINDING_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const COMPOSITION_EXECUTION_SCHEMA_VERSION: u32 = 1;

/// Policy disposition for a binding projected into a composition manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPolicyDisposition {
    Allowed,
    Gated,
    Denied,
}

/// Policy metadata attached to a manifest binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingPolicyStatus {
    pub disposition: BindingPolicyDisposition,
    pub reason: Option<String>,
}

impl Default for BindingPolicyStatus {
    fn default() -> Self {
        Self {
            disposition: BindingPolicyDisposition::Allowed,
            reason: None,
        }
    }
}

/// Prompt-visible description of one callable binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingManifestEntry {
    /// Canonical runtime tool name.
    pub name: String,
    /// Harn identifier injected into the composition snippet.
    pub binding: String,
    pub namespace: Option<String>,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: ToolAnnotations,
    pub side_effect_level: SideEffectLevel,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub path_args: Vec<String>,
    pub examples: Vec<Value>,
    /// `harn`, `host_bridge`, `mcp_server`, `provider_native`, `deferred`,
    /// or another forward-compatible source label.
    pub source: String,
    pub deferred: bool,
    pub policy: BindingPolicyStatus,
    pub metadata: Value,
}

impl Default for BindingManifestEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            binding: String::new(),
            namespace: None,
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            annotations: ToolAnnotations::default(),
            side_effect_level: SideEffectLevel::None,
            capabilities: BTreeMap::new(),
            path_args: Vec::new(),
            examples: Vec::new(),
            source: "harn".to_string(),
            deferred: false,
            policy: BindingPolicyStatus::default(),
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

/// Stable prompt-visible manifest for a composition run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindingManifest {
    pub schema_version: u32,
    pub bindings: Vec<BindingManifestEntry>,
    pub side_effect_ceiling: SideEffectLevel,
    pub metadata: Value,
}

impl Default for BindingManifest {
    fn default() -> Self {
        Self {
            schema_version: BINDING_MANIFEST_SCHEMA_VERSION,
            bindings: Vec::new(),
            side_effect_ceiling: SideEffectLevel::ReadOnly,
            metadata: Value::Object(serde_json::Map::new()),
        }
    }
}

impl BindingManifest {
    pub fn new(mut bindings: Vec<BindingManifestEntry>, ceiling: SideEffectLevel) -> Self {
        bindings.sort_by(|a, b| a.binding.cmp(&b.binding).then(a.name.cmp(&b.name)));
        Self {
            bindings,
            side_effect_ceiling: ceiling,
            ..Self::default()
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"bindings": []}))
    }

    pub fn to_compact_value(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            (
                "schema_version".to_string(),
                Value::Number(self.schema_version.into()),
            ),
            (
                "side_effect_ceiling".to_string(),
                serde_json::json!(self.side_effect_ceiling),
            ),
            (
                "bindings".to_string(),
                Value::Array(
                    self.bindings
                        .iter()
                        .map(|binding| {
                            serde_json::json!({
                                "name": binding.name,
                                "binding": binding.binding,
                                "namespace": binding.namespace,
                                "description": binding.description,
                                "side_effect_level": binding.side_effect_level,
                                "policy": binding.policy,
                                "source": binding.source,
                                "deferred": binding.deferred,
                                "examples": binding.examples,
                            })
                        })
                        .collect(),
                ),
            ),
        ]))
    }

    pub fn hash(&self) -> Result<String, serde_json::Error> {
        binding_manifest_hash(&self.to_value())
    }

    pub fn find_by_binding(&self, binding: &str) -> Option<&BindingManifestEntry> {
        self.bindings.iter().find(|entry| entry.binding == binding)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&BindingManifestEntry> {
        self.bindings.iter().find(|entry| entry.name == name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingManifestOptions {
    pub side_effect_ceiling: SideEffectLevel,
    pub include_denied: bool,
    pub denied_tools: BTreeSet<String>,
    pub gated_tools: BTreeSet<String>,
}

impl Default for BindingManifestOptions {
    fn default() -> Self {
        Self {
            side_effect_ceiling: SideEffectLevel::ReadOnly,
            include_denied: false,
            denied_tools: BTreeSet::new(),
            gated_tools: BTreeSet::new(),
        }
    }
}

/// Build a binding manifest from a Harn tool registry, MCP `tools/list`
/// payload, or provider-native tool array.
pub fn binding_manifest_from_tool_surface(
    tools: &Value,
    options: BindingManifestOptions,
) -> BindingManifest {
    let mut used_bindings = BTreeSet::new();
    let annotations_by_name = crate::tool_surface::tool_annotations_from_spec(tools);
    let mut entries = Vec::new();
    for tool in tool_surface_entries(tools) {
        let Some(name) = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let annotations = tool
            .get("annotations")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolAnnotations>(value).ok())
            .or_else(|| annotations_by_name.get(name).cloned())
            .unwrap_or_default();
        let side_effect_level = annotations.side_effect_level;
        let mut policy = BindingPolicyStatus::default();
        if options.denied_tools.contains(name) {
            policy.disposition = BindingPolicyDisposition::Denied;
            policy.reason = Some("denied by active tool policy".to_string());
        } else if side_effect_level.rank() > options.side_effect_ceiling.rank() {
            policy.disposition = BindingPolicyDisposition::Denied;
            policy.reason = Some(format!(
                "requires side-effect level '{}' above composition ceiling '{}'",
                side_effect_level.as_str(),
                options.side_effect_ceiling.as_str()
            ));
        } else if options.gated_tools.contains(name) {
            policy.disposition = BindingPolicyDisposition::Gated;
            policy.reason = Some("requires host approval before dispatch".to_string());
        }
        if !options.include_denied && policy.disposition == BindingPolicyDisposition::Denied {
            continue;
        }
        let binding = unique_binding_identifier(name, &mut used_bindings);
        let source = binding_source(&tool);
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .or_else(|| {
                tool.get("function")
                    .and_then(|function| function.get("defer_loading"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(source == "deferred");
        let input_schema = tool
            .get("inputSchema")
            .or_else(|| tool.get("input_schema"))
            .or_else(|| tool.get("parameters"))
            .or_else(|| tool.get("function").and_then(|f| f.get("parameters")))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        let output_schema = tool
            .get("outputSchema")
            .or_else(|| tool.get("output_schema"))
            .or_else(|| tool.get("returns"))
            .or_else(|| {
                tool.get("function")
                    .and_then(|f| f.get("x-harn-output-schema"))
            })
            .cloned();
        let examples = tool
            .get("examples")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        entries.push(BindingManifestEntry {
            name: name.to_string(),
            binding,
            namespace: tool
                .get("namespace")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            description: tool
                .get("description")
                .or_else(|| tool.get("function").and_then(|f| f.get("description")))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            input_schema,
            output_schema,
            side_effect_level,
            capabilities: annotations.capabilities.clone(),
            path_args: annotations.arg_schema.path_params.clone(),
            annotations,
            examples,
            source,
            deferred,
            policy,
            metadata: tool
                .get("metadata")
                .or_else(|| tool.get("_meta"))
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        });
    }
    BindingManifest::new(entries, options.side_effect_ceiling)
}

fn tool_surface_entries(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("tools") {
                return items.clone();
            }
            if map.get("name").and_then(Value::as_str).is_some() {
                return vec![value.clone()];
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn binding_source(tool: &Value) -> String {
    if tool
        .get("defer_loading")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "deferred".to_string();
    }
    if let Some(executor) = tool.get("executor").and_then(Value::as_str) {
        return executor.to_string();
    }
    if tool.get("_mcp_server").is_some() || tool.get("mcp_server").is_some() {
        return "mcp_server".to_string();
    }
    if tool.get("function").is_some() {
        return "provider_native".to_string();
    }
    "harn".to_string()
}

fn unique_binding_identifier(name: &str, used: &mut BTreeSet<String>) -> String {
    let base = sanitize_binding_identifier(name);
    if used.insert(base.clone()) {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}_{index}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded identifier suffix search")
}

fn sanitize_binding_identifier(name: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in name.chars().enumerate() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            if idx == 0 && ch.is_ascii_digit() {
                out.push_str("tool_");
            }
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    let out = if out.is_empty() {
        "tool".to_string()
    } else {
        out
    };
    if HARN_KEYWORDS.contains(&out.as_str()) {
        format!("tool_{out}")
    } else {
        out
    }
}

const HARN_KEYWORDS: &[&str] = &[
    "agent",
    "as",
    "await",
    "break",
    "catch",
    "continue",
    "defer",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "import",
    "in",
    "interface",
    "let",
    "match",
    "nil",
    "pipeline",
    "pub",
    "return",
    "skill",
    "spawn",
    "struct",
    "throw",
    "true",
    "try",
    "type",
    "var",
    "while",
];

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

#[async_trait::async_trait(?Send)]
pub trait CompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput;
}

struct ExecutionState {
    request: CompositionExecutionRequest,
    calls: Vec<CompositionChildCall>,
    results: Vec<CompositionChildResult>,
    clock: Arc<dyn harn_clock::Clock>,
    started_ms: i64,
}

impl ExecutionState {
    fn next_call(
        &mut self,
        tool_name: &str,
        input: Value,
    ) -> Result<(BindingManifestEntry, CompositionChildCall), VmError> {
        if self.results.len() as u64 >= self.request.limits.max_operations {
            return Err(VmError::Runtime(format!(
                "composition exceeded max_operations={}",
                self.request.limits.max_operations
            )));
        }
        if let Some(timeout_ms) = self.request.limits.timeout_ms {
            if elapsed_ms(&*self.clock, self.started_ms) > timeout_ms {
                return Err(VmError::Runtime(format!(
                    "composition exceeded timeout_ms={timeout_ms}"
                )));
            }
        }
        let binding = self
            .request
            .manifest
            .find_by_name(tool_name)
            .or_else(|| self.request.manifest.find_by_binding(tool_name))
            .cloned()
            .ok_or_else(|| {
                VmError::Runtime(format!("composition binding '{tool_name}' not found"))
            })?;
        let call = self.push_call(&binding, input);
        if binding.policy.disposition == BindingPolicyDisposition::Denied {
            let message = format!(
                "composition binding '{}' denied{}",
                binding.name,
                binding
                    .policy
                    .reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        if binding.policy.disposition == BindingPolicyDisposition::Gated {
            let message = format!(
                "composition binding '{}' requires approval and cannot run in read-only mode",
                binding.name
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        if binding.side_effect_level.rank() > self.request.requested_side_effect_ceiling.rank() {
            let message = format!(
                "composition binding '{}' requires side-effect level '{}' above requested ceiling '{}'",
                binding.name,
                binding.side_effect_level.as_str(),
                self.request.requested_side_effect_ceiling.as_str()
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        Ok((binding, call))
    }

    fn push_call(&mut self, binding: &BindingManifestEntry, input: Value) -> CompositionChildCall {
        let operation_index = self.calls.len() as u64;
        let call = CompositionChildCall {
            run_id: self.request.run_id.clone(),
            tool_call_id: format!("{}:{operation_index}", self.request.run_id),
            tool_name: binding.name.clone(),
            operation_index,
            annotations: Some(binding.annotations.clone()),
            requested_side_effect_level: binding.side_effect_level,
            policy_context: serde_json::json!({
                "disposition": binding.policy.disposition,
                "reason": binding.policy.reason,
                "ceiling": self.request.requested_side_effect_ceiling,
            }),
            raw_input: input,
        };
        self.calls.push(call.clone());
        call
    }

    fn push_failed_result(
        &mut self,
        call: &CompositionChildCall,
        message: &str,
        category: ToolCallErrorCategory,
    ) {
        self.results.push(CompositionChildResult {
            run_id: call.run_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
            tool_name: call.tool_name.clone(),
            operation_index: call.operation_index,
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some(message.to_string()),
            error_category: Some(category),
            executor: Some(ToolExecutor::HarnBuiltin),
            duration_ms: Some(0),
            execution_duration_ms: Some(0),
        });
    }

    fn push_result(
        &mut self,
        call: &CompositionChildCall,
        output: &CompositionToolOutput,
        elapsed_ms: u64,
    ) {
        if self
            .results
            .iter()
            .any(|result| result.tool_call_id == call.tool_call_id)
        {
            return;
        }
        self.results.push(CompositionChildResult {
            run_id: call.run_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
            tool_name: call.tool_name.clone(),
            operation_index: call.operation_index,
            status: if output.error.is_some() {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            raw_output: output.value.clone(),
            error: output.error.clone(),
            error_category: output.error_category,
            executor: output.executor.clone(),
            duration_ms: Some(elapsed_ms),
            execution_duration_ms: Some(elapsed_ms),
        });
    }
}

/// Execute a read-only Harn-native composition snippet against a manifest.
pub async fn execute_harn_composition(
    mut request: CompositionExecutionRequest,
    host: Rc<dyn CompositionToolHost>,
) -> CompositionExecutionReport {
    if request.run_id.trim().is_empty() {
        request.run_id = uuid::Uuid::now_v7().to_string();
    }
    if request.language.trim().is_empty() {
        request.language = "harn".to_string();
    }
    let manifest_hash = request
        .manifest
        .hash()
        .unwrap_or_else(|_| "sha256:manifest_hash_error".to_string());
    let snippet_hash = composition_snippet_hash(&request.language, &request.snippet);
    let mut run = CompositionRunEnvelope::read_only(
        request.run_id.clone(),
        request.language.clone(),
        snippet_hash,
        manifest_hash,
    );
    let session_id = request.session_id.clone();
    run.requested_side_effect_ceiling = request.requested_side_effect_ceiling;
    run.metadata = request.metadata.clone();
    if !run.metadata.is_object() {
        run.metadata = Value::Object(serde_json::Map::new());
    }
    if let Some(session_id) = &session_id {
        run.metadata["session_id"] = Value::String(session_id.clone());
    }
    let clock = harn_clock::RealClock::arc();
    let started_ms = clock.monotonic_ms();

    let result = if request.language != "harn" {
        Err((
            CompositionFailureCategory::UnsupportedLanguage,
            format!("unsupported composition language '{}'", request.language),
            Vec::new(),
            Vec::new(),
        ))
    } else if request.requested_side_effect_ceiling.rank() > SideEffectLevel::ReadOnly.rank() {
        Err((
            CompositionFailureCategory::PolicyDenied,
            "read-only composition executor refuses side-effect ceilings above read_only"
                .to_string(),
            Vec::new(),
            Vec::new(),
        ))
    } else {
        execute_harn_composition_inner(request, host).await
    };

    let report = match result {
        Ok((value, stdout, calls, results)) => {
            run.result = Some(value);
            run.stdout = (!stdout.is_empty()).then_some(stdout);
            run.duration_ms = Some(elapsed_ms(&*clock, started_ms));
            CompositionExecutionReport {
                schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
                ok: true,
                summary: format!(
                    "composition completed with {} child operation(s)",
                    results.len()
                ),
                run,
                child_calls: calls,
                child_results: results,
            }
        }
        Err((category, error, calls, results)) => {
            run.failure_category = Some(category);
            run.error = Some(error.clone());
            run.duration_ms = Some(elapsed_ms(&*clock, started_ms));
            CompositionExecutionReport {
                schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
                ok: false,
                summary: error,
                run,
                child_calls: calls,
                child_results: results,
            }
        }
    };
    if let Some(session_id) = session_id {
        emit_composition_report_events(&session_id, &report);
    }
    report
}

pub fn composition_report_events(
    session_id: impl Into<String>,
    report: &CompositionExecutionReport,
) -> Vec<AgentEvent> {
    let session_id = session_id.into();
    let mut start_run = report.run.clone();
    start_run.stdout = None;
    start_run.stderr = None;
    start_run.artifacts = Vec::new();
    start_run.result = None;
    start_run.failure_category = None;
    start_run.error = None;
    start_run.duration_ms = None;

    let mut events = vec![AgentEvent::CompositionStart {
        session_id: session_id.clone(),
        run: start_run,
    }];
    for call in &report.child_calls {
        events.push(AgentEvent::CompositionChildCall {
            session_id: session_id.clone(),
            call: call.clone(),
        });
        for result in report
            .child_results
            .iter()
            .filter(|result| result.tool_call_id == call.tool_call_id)
        {
            events.push(AgentEvent::CompositionChildResult {
                session_id: session_id.clone(),
                result: result.clone(),
            });
        }
    }
    if report.ok {
        events.push(AgentEvent::CompositionFinish {
            session_id,
            run: report.run.clone(),
        });
    } else {
        events.push(AgentEvent::CompositionError {
            session_id,
            run: report.run.clone(),
        });
    }
    events
}

fn emit_composition_report_events(session_id: &str, report: &CompositionExecutionReport) {
    for event in composition_report_events(session_id, report) {
        crate::llm::emit_live_agent_event_sync(&event);
    }
}

async fn execute_harn_composition_inner(
    request: CompositionExecutionRequest,
    host: Rc<dyn CompositionToolHost>,
) -> Result<
    (
        Value,
        String,
        Vec<CompositionChildCall>,
        Vec<CompositionChildResult>,
    ),
    (
        CompositionFailureCategory,
        String,
        Vec<CompositionChildCall>,
        Vec<CompositionChildResult>,
    ),
> {
    let validation_source = composition_validation_source(&request.snippet);
    let validation_program = harn_parser::parse_source(&validation_source).map_err(|error| {
        (
            CompositionFailureCategory::SchemaValidation,
            format!("composition parse error: {error}"),
            Vec::new(),
            Vec::new(),
        )
    })?;
    validate_composition_program(&validation_program, &request.manifest).map_err(|error| {
        (
            CompositionFailureCategory::PolicyDenied,
            error,
            Vec::new(),
            Vec::new(),
        )
    })?;

    let source = composition_source(&request.manifest, &request.snippet);
    let program = harn_parser::parse_source(&source).map_err(|error| {
        (
            CompositionFailureCategory::SchemaValidation,
            format!("composition parse error: {error}"),
            Vec::new(),
            Vec::new(),
        )
    })?;
    let chunk = crate::Compiler::new()
        .compile_named(&program, "main")
        .map_err(|error| {
            (
                CompositionFailureCategory::SchemaValidation,
                format!("composition compile error: {error}"),
                Vec::new(),
                Vec::new(),
            )
        })?;

    let execution_clock = harn_clock::RealClock::arc();
    let execution_started_ms = execution_clock.monotonic_ms();
    let state = Rc::new(RefCell::new(ExecutionState {
        request,
        calls: Vec::new(),
        results: Vec::new(),
        clock: execution_clock,
        started_ms: execution_started_ms,
    }));
    let mut vm = Vm::new();
    crate::register_core_stdlib(&mut vm);
    register_composition_call_builtin(&mut vm, state.clone(), host);
    if let Some(timeout_ms) = state.borrow().request.limits.timeout_ms {
        vm.push_deadline_after(std::time::Duration::from_millis(timeout_ms));
    }
    vm.set_source_info("composition://snippet.harn", &source);
    match vm.execute(&chunk).await {
        Ok(value) => {
            let json = crate::llm::vm_value_to_json(&value);
            let stdout = vm.output().to_string();
            let state = state.borrow();
            let result_size = serde_json::to_vec(&json)
                .map(|bytes| bytes.len())
                .unwrap_or(0);
            let output_size = result_size.saturating_add(stdout.len());
            if output_size as u64 > state.request.limits.max_output_bytes {
                return Err((
                    CompositionFailureCategory::ExecutionError,
                    format!(
                        "composition output exceeded max_output_bytes={}",
                        state.request.limits.max_output_bytes
                    ),
                    state.calls.clone(),
                    state.results.clone(),
                ));
            }
            Ok((json, stdout, state.calls.clone(), state.results.clone()))
        }
        Err(error) => {
            let state = state.borrow();
            let category = if error.to_string().contains("denied")
                || error.to_string().contains("side-effect")
                || error.to_string().contains("approval")
            {
                CompositionFailureCategory::PolicyDenied
            } else if error.to_string().contains("Deadline exceeded")
                || error.to_string().contains("max_operations")
                || error.to_string().contains("timeout_ms")
                || error.to_string().contains("max_output_bytes")
            {
                CompositionFailureCategory::Timeout
            } else if state
                .results
                .iter()
                .any(|result| result.status == ToolCallStatus::Failed)
            {
                CompositionFailureCategory::ChildToolError
            } else {
                CompositionFailureCategory::ExecutionError
            };
            Err((
                category,
                error.to_string(),
                state.calls.clone(),
                state.results.clone(),
            ))
        }
    }
}

fn register_composition_call_builtin(
    vm: &mut Vm,
    state: Rc<RefCell<ExecutionState>>,
    host: Rc<dyn CompositionToolHost>,
) {
    vm.register_async_builtin("__composition_call", move |args| {
        let state = state.clone();
        let host = host.clone();
        async move {
            let tool_name = args
                .first()
                .map(VmValue::display)
                .ok_or_else(|| VmError::Runtime("__composition_call: missing tool name".into()))?;
            let input = args
                .get(1)
                .map(crate::llm::vm_value_to_json)
                .unwrap_or_else(|| serde_json::json!({}));
            let (binding, call, clock) = {
                let mut state = state.borrow_mut();
                let (binding, call) = state.next_call(&tool_name, input.clone())?;
                (binding, call, state.clock.clone())
            };
            let started_ms = clock.monotonic_ms();
            let output = host.call(&binding, input).await;
            {
                let mut state = state.borrow_mut();
                state.push_result(&call, &output, elapsed_ms(&*clock, started_ms));
            }
            if let Some(error) = output.error {
                return Err(VmError::Runtime(error));
            }
            Ok(crate::json_to_vm_value(
                &output.value.unwrap_or(Value::Null),
            ))
        }
    });
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

fn composition_validation_source(snippet: &str) -> String {
    let mut source = String::from("pipeline main() {\n");
    source.push_str(snippet);
    if !snippet.ends_with('\n') {
        source.push('\n');
    }
    source.push_str("}\n");
    source
}

fn composition_source(manifest: &BindingManifest, snippet: &str) -> String {
    let mut source = String::new();
    for binding in &manifest.bindings {
        source.push_str(&format!(
            "fn {}(args = {{}}) {{ return __composition_call(\"{}\", args) }}\n",
            binding.binding,
            escape_harn_string(&binding.name)
        ));
    }
    source.push_str("pipeline main() {\n");
    source.push_str(snippet);
    if !snippet.ends_with('\n') {
        source.push('\n');
    }
    source.push_str("}\n");
    source
}

fn escape_harn_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn validate_composition_program(
    program: &[harn_parser::SNode],
    manifest: &BindingManifest,
) -> Result<(), String> {
    use harn_parser::visit::walk_program;
    use harn_parser::Node;

    let bindings = manifest
        .bindings
        .iter()
        .map(|entry| entry.binding.clone())
        .collect::<BTreeSet<_>>();
    let mut local_functions = BTreeSet::from(["__composition_call".to_string()]);
    walk_program(program, &mut |node| {
        if let Node::FnDecl { name, .. } = &node.node {
            local_functions.insert(name.clone());
        }
    });

    let mut error = None;
    walk_program(program, &mut |node| {
        if error.is_some() {
            return;
        }
        match &node.node {
            Node::ImportDecl { .. } | Node::SelectiveImport { .. } => {
                error = Some("composition snippets cannot import modules".to_string());
            }
            Node::SpawnExpr { .. } | Node::Parallel { .. } => {
                error = Some("composition snippets cannot spawn or parallelize work".to_string());
            }
            Node::HitlExpr { .. } => {
                error = Some("composition snippets cannot request HITL directly".to_string());
            }
            Node::CostRoute { .. } => {
                error = Some("composition snippets cannot open LLM routing blocks".to_string());
            }
            Node::FunctionCall { name, .. } => {
                if DENIED_COMPOSITION_CALLS.contains(&name.as_str()) && !bindings.contains(name) {
                    error = Some(format!("composition snippets cannot call `{name}`"));
                } else if !bindings.contains(name)
                    && !local_functions.contains(name)
                    && !PURE_COMPOSITION_CALLS.contains(&name.as_str())
                {
                    error = Some(format!(
                        "composition call target `{name}` is not a manifest binding or pure helper"
                    ));
                }
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

const DENIED_COMPOSITION_CALLS: &[&str] = &[
    "append_file",
    "ask_user",
    "connector_call",
    "copy_file",
    "delete_file",
    "dual_control",
    "escalate_to",
    "event_log_emit",
    "event_log.emit",
    "exec",
    "host_call",
    "host_tool_call",
    "http_delete",
    "http_download",
    "http_get",
    "http_patch",
    "http_post",
    "http_put",
    "http_request",
    "llm_call",
    "mcp_call",
    "mcp_connect",
    "pg_execute",
    "pg_query",
    "request_approval",
    "secret_get",
    "write_file",
];

const PURE_COMPOSITION_CALLS: &[&str] = &[
    "Ok",
    "Err",
    "abs",
    "assert",
    "assert_eq",
    "assert_ne",
    "base64_decode",
    "base64_encode",
    "ceil",
    "contains",
    "dedup_by",
    "dirname",
    "entries",
    "ends_with",
    "flat_map",
    "floor",
    "format",
    "group_by",
    "hash_value",
    "hex_decode",
    "hex_encode",
    "is_err",
    "is_ok",
    "join",
    "jq",
    "jq_first",
    "json_extract",
    "json_parse",
    "json_pointer",
    "json_stringify",
    "keys",
    "len",
    "lower",
    "parse_float_or",
    "parse_int_or",
    "split",
    "starts_with",
    "to_float",
    "to_int",
    "to_string",
    "trim",
    "upper",
    "values",
];

pub struct StaticCompositionToolHost {
    outputs: BTreeMap<String, Value>,
}

impl StaticCompositionToolHost {
    pub fn new(outputs: BTreeMap<String, Value>) -> Self {
        Self { outputs }
    }
}

#[async_trait::async_trait(?Send)]
impl CompositionToolHost for StaticCompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        if let Some(value) = self.outputs.get(&binding.name) {
            return CompositionToolOutput::ok(value.clone());
        }
        if let Some(value) = binding.metadata.get("mock_output") {
            return CompositionToolOutput::ok(value.clone());
        }
        CompositionToolOutput::ok(serde_json::json!({
            "tool": binding.name,
            "input": input,
        }))
    }
}

/// Dispatches every binding call to a caller-supplied Harn closure.
///
/// The closure is compiled in the calling VM and receives
/// `(binding_name: string, input: dict)`. Returning a value yields a successful
/// child result; raising an error fails the child call. Each invocation runs
/// on a fresh clone of the outer VM so closure-side builtins (`host_call`,
/// pipeline imports, etc.) resolve normally — the inner composition VM only
/// sees the manifest bindings plus pure helpers.
pub struct ClosureCompositionToolHost {
    closure: crate::VmClosure,
    outer_vm: Vm,
}

impl ClosureCompositionToolHost {
    pub fn new(closure: crate::VmClosure, outer_vm: Vm) -> Self {
        Self { closure, outer_vm }
    }
}

#[async_trait::async_trait(?Send)]
impl CompositionToolHost for ClosureCompositionToolHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        let mut vm = self.outer_vm.child_vm();
        let args = vec![
            VmValue::String(Rc::from(binding.name.as_str())),
            crate::json_to_vm_value(&input),
        ];
        match vm.call_closure_pub(&self.closure, &args).await {
            Ok(value) => {
                let json = crate::llm::vm_value_to_json(&value);
                CompositionToolOutput::ok(json)
            }
            Err(error) => {
                CompositionToolOutput::error(error.to_string(), ToolCallErrorCategory::ToolError)
            }
        }
    }
}

pub fn composition_search_examples(query: &str, limit: usize) -> Value {
    let mut examples = vec![
        serde_json::json!({
            "id": "read-summarize",
            "title": "Read two files and return a compact summary",
            "language": "harn",
            "snippet": "let readme = read_file({path: \"README.md\"})\nlet spec = read_file({path: \"spec/HARN_SPEC.md\", limit: 80})\nreturn {readme: readme, spec_excerpt: spec}",
            "required_side_effect_level": "read_only",
            "tools": ["read_file"]
        }),
        serde_json::json!({
            "id": "search-then-read",
            "title": "Search first, then read the best candidate",
            "language": "harn",
            "snippet": "let hits = search({query: \"CompositionRunEnvelope\"})\nreturn hits",
            "required_side_effect_level": "read_only",
            "tools": ["search"]
        }),
    ];
    if !query.trim().is_empty() {
        let q = query.to_ascii_lowercase();
        examples.retain(|example| {
            example
                .to_string()
                .to_ascii_lowercase()
                .contains(q.as_str())
        });
    }
    examples.truncate(limit.max(1));
    Value::Array(examples)
}

pub fn composition_typescript_declarations(manifest: &BindingManifest) -> String {
    let mut out = String::from(
        "export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };\n",
    );
    out.push_str("export type CompositionToolResult = JsonValue;\n\n");
    for binding in &manifest.bindings {
        if binding.policy.disposition != BindingPolicyDisposition::Allowed {
            continue;
        }
        let args_type = json_schema_to_typescript(&binding.input_schema);
        let result_type = binding
            .output_schema
            .as_ref()
            .map(json_schema_to_typescript)
            .unwrap_or_else(|| "CompositionToolResult".to_string());
        out.push_str(&format!(
            "export declare function {}(args: {}): Promise<{}>;\n",
            binding.binding, args_type, result_type
        ));
    }
    out
}

fn json_schema_to_typescript(schema: &Value) -> String {
    if let Some(shorthand) = schema.as_str() {
        return match shorthand {
            "string" => "string".to_string(),
            "int" | "integer" | "float" | "number" => "number".to_string(),
            "bool" | "boolean" => "boolean".to_string(),
            "list" | "array" => "JsonValue[]".to_string(),
            "dict" | "object" => "{ [key: string]: JsonValue }".to_string(),
            _ => "JsonValue".to_string(),
        };
    }
    let schema_type = schema.get("type").and_then(Value::as_str);
    match schema_type {
        Some("string") => enum_string_literals(schema).unwrap_or_else(|| "string".to_string()),
        Some("integer") | Some("number") => "number".to_string(),
        Some("boolean") => "boolean".to_string(),
        Some("array") => {
            let item_type = schema
                .get("items")
                .map(json_schema_to_typescript)
                .unwrap_or_else(|| "JsonValue".to_string());
            format!("{item_type}[]")
        }
        Some("object") | None if schema.get("properties").is_some() => {
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let mut fields = Vec::new();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, value) in properties {
                    let marker = if required.contains(name.as_str()) {
                        ""
                    } else {
                        "?"
                    };
                    fields.push(format!(
                        "{}{}: {}",
                        typescript_property_name(name),
                        marker,
                        json_schema_to_typescript(value)
                    ));
                }
            }
            if fields.is_empty() {
                "{ [key: string]: JsonValue }".to_string()
            } else {
                format!("{{ {} }}", fields.join("; "))
            }
        }
        None if schema.as_object().is_some() => {
            let fields = schema
                .as_object()
                .into_iter()
                .flat_map(|properties| properties.iter())
                .map(|(name, value)| {
                    let marker = if value
                        .get("required")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                    {
                        ""
                    } else {
                        "?"
                    };
                    format!(
                        "{}{}: {}",
                        typescript_property_name(name),
                        marker,
                        json_schema_to_typescript(value)
                    )
                })
                .collect::<Vec<_>>();
            if fields.is_empty() {
                "{ [key: string]: JsonValue }".to_string()
            } else {
                format!("{{ {} }}", fields.join("; "))
            }
        }
        Some("object") => "{ [key: string]: JsonValue }".to_string(),
        _ => "JsonValue".to_string(),
    }
}

fn enum_string_literals(schema: &Value) -> Option<String> {
    let variants = schema.get("enum")?.as_array()?;
    let strings = variants
        .iter()
        .map(|value| value.as_str().map(|text| format!("{text:?}")))
        .collect::<Option<Vec<_>>>()?;
    (!strings.is_empty()).then(|| strings.join(" | "))
}

fn typescript_property_name(name: &str) -> String {
    if name.chars().enumerate().all(|(idx, ch)| {
        ch == '_' || ch.is_ascii_alphanumeric() && (idx > 0 || !ch.is_ascii_digit())
    }) {
        name.to_string()
    } else {
        format!("{name:?}")
    }
}

pub fn composition_crystallization_trace(
    report: &CompositionExecutionReport,
    options: &Value,
) -> Value {
    let trace_id = options
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("composition_{}", report.run.run_id));
    let mut capabilities = BTreeSet::new();
    for call in &report.child_calls {
        if let Some(annotations) = &call.annotations {
            for (domain, ops) in &annotations.capabilities {
                for op in ops {
                    capabilities.insert(format!("{domain}.{op}"));
                }
            }
        }
    }
    let parent_parameters = serde_json::json!({
        "language": report.run.language,
        "snippet_hash": report.run.snippet_hash,
        "binding_manifest_hash": report.run.binding_manifest_hash,
        "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
    });
    let mut actions = vec![serde_json::json!({
        "id": "composition_parent",
        "kind": "composition_run",
        "name": "execute_composition",
        "inputs": parent_parameters,
        "parameters": parent_parameters,
        "output": report.run.result,
        "observed_output": report.run.result,
        "capabilities": capabilities.into_iter().collect::<Vec<_>>(),
        "side_effects": [],
        "duration_ms": report.run.duration_ms.unwrap_or(0),
        "deterministic": true,
        "fuzzy": false,
        "metadata": {
            "source_kind": "composition_parent_run",
            "composition_run_id": report.run.run_id,
            "composition_schema_version": report.schema_version,
            "child_count": report.child_calls.len(),
            "ok": report.ok,
            "failure_category": report.run.failure_category,
        }
    })];
    actions.extend(report.child_calls.iter().map(|call| {
        let result = report
            .child_results
            .iter()
            .find(|result| result.tool_call_id == call.tool_call_id);
        let capabilities = call
            .annotations
            .as_ref()
            .map(|annotations| {
                annotations
                    .capabilities
                    .iter()
                    .flat_map(|(domain, ops)| ops.iter().map(move |op| format!("{domain}.{op}")))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        serde_json::json!({
            "id": format!("composition_child_{}", call.operation_index),
            "kind": "tool_call",
            "name": call.tool_name,
            "inputs": call.raw_input,
            "parameters": call.raw_input,
            "output": result.and_then(|result| result.raw_output.clone()),
            "observed_output": result.and_then(|result| result.raw_output.clone()),
            "capabilities": capabilities,
            "side_effects": [],
            "duration_ms": result.and_then(|result| result.duration_ms).unwrap_or(0),
            "deterministic": true,
            "fuzzy": false,
            "metadata": {
                "source_kind": "composition_child_call",
                "composition_run_id": report.run.run_id,
                "composition_tool_call_id": call.tool_call_id,
                "requested_side_effect_level": call.requested_side_effect_level,
                "annotations": call.annotations,
                "policy_context": call.policy_context,
                "status": result.map(|result| result.status),
                "error_category": result.and_then(|result| result.error_category),
            }
        })
    }));
    let replay_run = composition_replay_run(report, &trace_id);
    serde_json::json!({
        "version": 1,
        "id": trace_id,
        "source": "composition_run",
        "source_hash": report.run.snippet_hash,
        "workflow_id": options.get("workflow_id").and_then(Value::as_str).unwrap_or("composition_candidate"),
        "flow": {
            "trace_id": report.run.run_id,
            "agent_run_id": options.get("agent_run_id").and_then(Value::as_str),
            "transcript_ref": options.get("transcript_ref").and_then(Value::as_str),
        },
        "actions": actions,
        "replay_run": replay_run,
        "replay_allowlist": [
            {
                "path": "/run_id",
                "reason": "run ids are allocated per execution"
            },
            {
                "path": "/effect_receipts/*/run_id",
                "reason": "composition receipts retain source run lineage"
            },
            {
                "path": "/effect_receipts/*/tool_call_id",
                "reason": "composition child call ids include the source run id"
            },
            {
                "path": "/policy_decisions/*/run_id",
                "reason": "composition policy decisions retain source run lineage"
            },
            {
                "path": "/policy_decisions/*/tool_call_id",
                "reason": "composition policy decision ids include the source run id"
            }
        ],
        "metadata": {
            "source_kind": "composition_run",
            "composition_schema_version": report.schema_version,
            "run_id": report.run.run_id,
            "snippet_hash": report.run.snippet_hash,
            "binding_manifest_hash": report.run.binding_manifest_hash,
            "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
            "ok": report.ok,
            "failure_category": report.run.failure_category,
            "child_count": report.child_calls.len(),
        },
    })
}

fn composition_replay_run(report: &CompositionExecutionReport, trace_id: &str) -> Value {
    let event_log_entries = composition_report_events(trace_id, report)
        .into_iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect::<Vec<_>>();
    let mut effect_receipts = vec![serde_json::json!({
        "kind": "composition_parent",
        "run_id": report.run.run_id,
        "schema_version": report.schema_version,
        "snippet_hash": report.run.snippet_hash,
        "binding_manifest_hash": report.run.binding_manifest_hash,
        "requested_side_effect_ceiling": report.run.requested_side_effect_ceiling,
        "ok": report.ok,
        "failure_category": report.run.failure_category,
        "result": report.run.result,
        "stdout": report.run.stdout,
    })];
    let mut policy_decisions = Vec::new();
    for call in &report.child_calls {
        let result = report
            .child_results
            .iter()
            .find(|result| result.tool_call_id == call.tool_call_id);
        effect_receipts.push(serde_json::json!({
            "kind": "composition_child",
            "run_id": report.run.run_id,
            "tool_call_id": call.tool_call_id,
            "tool_name": call.tool_name,
            "operation_index": call.operation_index,
            "requested_side_effect_level": call.requested_side_effect_level,
            "input": call.raw_input,
            "status": result.map(|result| result.status),
            "error_category": result.and_then(|result| result.error_category),
            "output": result.and_then(|result| result.raw_output.clone()),
        }));
        policy_decisions.push(serde_json::json!({
            "kind": "composition_child_policy",
            "run_id": report.run.run_id,
            "tool_call_id": call.tool_call_id,
            "tool_name": call.tool_name,
            "requested_side_effect_level": call.requested_side_effect_level,
            "policy_context": call.policy_context,
        }));
    }
    serde_json::json!({
        "run_id": report.run.run_id,
        "event_log_entries": event_log_entries,
        "effect_receipts": effect_receipts,
        "policy_decisions": policy_decisions,
    })
}

pub fn register_composition_builtins(vm: &mut Vm) {
    vm.register_builtin("composition_binding_manifest", |args, _out| {
        let tools = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(Value::Null);
        let options_json = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(Value::Null);
        let mut options = BindingManifestOptions::default();
        if let Some(ceiling) = options_json
            .get("side_effect_ceiling")
            .and_then(Value::as_str)
        {
            options.side_effect_ceiling = SideEffectLevel::parse(ceiling);
        }
        if let Some(include_denied) = options_json.get("include_denied").and_then(Value::as_bool) {
            options.include_denied = include_denied;
        }
        options.denied_tools = string_set_option(&options_json, "denied_tools");
        options.gated_tools = string_set_option(&options_json, "gated_tools");
        let manifest = binding_manifest_from_tool_surface(&tools, options);
        let value = if options_json.get("form").and_then(Value::as_str) == Some("compact") {
            manifest.to_compact_value()
        } else {
            manifest.to_value()
        };
        Ok(crate::json_to_vm_value(&value))
    });

    vm.register_builtin("composition_search_examples", |args, _out| {
        let query = args.first().map(VmValue::display).unwrap_or_default();
        let limit = args
            .get(1)
            .and_then(|value| match value {
                VmValue::Int(n) => Some((*n).max(1) as usize),
                _ => None,
            })
            .unwrap_or(10);
        Ok(crate::json_to_vm_value(&composition_search_examples(
            &query, limit,
        )))
    });

    vm.register_builtin("composition_typescript_declarations", |args, _out| {
        let manifest_value = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| {
                VmError::Runtime("composition_typescript_declarations: manifest is required".into())
            })?;
        let manifest: BindingManifest =
            serde_json::from_value(manifest_value).map_err(|error| {
                VmError::Runtime(format!(
                    "composition_typescript_declarations: invalid manifest: {error}"
                ))
            })?;
        Ok(VmValue::String(Rc::from(
            composition_typescript_declarations(&manifest),
        )))
    });

    vm.register_builtin("composition_crystallization_trace", |args, _out| {
        let report_value = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| {
                VmError::Runtime("composition_crystallization_trace: report is required".into())
            })?;
        let report: CompositionExecutionReport =
            serde_json::from_value(report_value).map_err(|error| {
                VmError::Runtime(format!(
                    "composition_crystallization_trace: invalid report: {error}"
                ))
            })?;
        let options = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        Ok(crate::json_to_vm_value(&composition_crystallization_trace(
            &report, &options,
        )))
    });

    vm.register_async_builtin("composition_execute", |args| async move {
        let snippet = args
            .first()
            .map(VmValue::display)
            .ok_or_else(|| VmError::Runtime("composition_execute: snippet is required".into()))?;
        let manifest_value = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| VmError::Runtime("composition_execute: manifest is required".into()))?;
        let dispatcher = args.get(2).and_then(|value| match value {
            VmValue::Closure(closure) => Some((**closure).clone()),
            VmValue::Dict(dict) => match dict.get("dispatcher") {
                Some(VmValue::Closure(closure)) => Some((**closure).clone()),
                _ => None,
            },
            _ => None,
        });
        let mut request = CompositionExecutionRequest {
            snippet,
            manifest: serde_json::from_value(manifest_value).map_err(|error| {
                VmError::Runtime(format!("composition_execute: invalid manifest: {error}"))
            })?,
            ..CompositionExecutionRequest::default()
        };
        if let Some(options) = args.get(2).map(crate::llm::vm_value_to_json) {
            if let Some(session_id) = options.get("session_id").and_then(Value::as_str) {
                request.session_id = Some(session_id.to_string());
            }
            if let Some(run_id) = options.get("run_id").and_then(Value::as_str) {
                request.run_id = run_id.to_string();
            }
            if let Some(max_operations) = options.get("max_operations").and_then(Value::as_u64) {
                request.limits.max_operations = max_operations;
            }
            if let Some(timeout_ms) = options.get("timeout_ms").and_then(Value::as_u64) {
                request.limits.timeout_ms = Some(timeout_ms);
            }
            if let Some(max_output_bytes) = options.get("max_output_bytes").and_then(Value::as_u64)
            {
                request.limits.max_output_bytes = max_output_bytes;
            }
        }
        let host: Rc<dyn CompositionToolHost> = match dispatcher {
            Some(closure) => {
                let outer_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
                    VmError::Runtime(
                        "composition_execute: dispatcher requires an async builtin VM context"
                            .into(),
                    )
                })?;
                Rc::new(ClosureCompositionToolHost::new(closure, outer_vm))
            }
            None => Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        };
        let report = execute_harn_composition(request, host).await;
        Ok(crate::json_to_vm_value(
            &serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({"ok": false})),
        ))
    });
}

fn string_set_option(value: &Value, key: &str) -> BTreeSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_hash_includes_language() {
        let harn = composition_snippet_hash("harn", "read_file(\"AGENTS.md\")");
        let ts = composition_snippet_hash("typescript", "read_file(\"AGENTS.md\")");
        assert_ne!(harn, ts);
        assert!(harn.starts_with("sha256:"));
    }

    #[test]
    fn binding_manifest_hash_is_stable_for_identical_values() {
        let manifest = serde_json::json!({
            "bindings": [
                {
                    "name": "read_file",
                    "annotations": {"side_effect_level": "read_only"}
                }
            ]
        });
        assert_eq!(
            binding_manifest_hash(&manifest).unwrap(),
            binding_manifest_hash(&manifest).unwrap()
        );
    }

    #[test]
    fn child_call_preserves_mutation_annotations() {
        let call = CompositionChildCall {
            run_id: "run-1".into(),
            tool_call_id: "tool-1".into(),
            tool_name: "write_file".into(),
            operation_index: 0,
            requested_side_effect_level: SideEffectLevel::WorkspaceWrite,
            annotations: Some(ToolAnnotations {
                side_effect_level: SideEffectLevel::WorkspaceWrite,
                ..ToolAnnotations::default()
            }),
            raw_input: serde_json::json!({"path": "src/lib.rs"}),
            ..CompositionChildCall::default()
        };
        let encoded = serde_json::to_value(&call).unwrap();
        assert_eq!(encoded["requested_side_effect_level"], "workspace_write");
        assert_eq!(
            encoded["annotations"]["side_effect_level"],
            "workspace_write"
        );
    }

    #[test]
    fn binding_manifest_projects_policy_and_stable_binding_names() {
        let tools = serde_json::json!({
            "_type": "tool_registry",
            "tools": [
                {
                    "name": "read.file",
                    "description": "Read a file",
                    "parameters": {"type": "object", "required": ["path"]},
                    "annotations": {
                        "kind": "read",
                        "side_effect_level": "read_only",
                        "arg_schema": {"path_params": ["path"]},
                        "capabilities": {"workspace": ["read_text"]},
                        "inline_result": true
                    }
                },
                {
                    "name": "write_file",
                    "parameters": {"type": "object"},
                    "annotations": {
                        "kind": "edit",
                        "side_effect_level": "workspace_write"
                    }
                },
                {
                    "name": "host.read",
                    "executor": "host_bridge",
                    "parameters": {"type": "object"},
                    "annotations": {
                        "kind": "read",
                        "side_effect_level": "read_only"
                    }
                },
                {
                    "name": "mcp.search",
                    "_mcp_server": "docs",
                    "parameters": {"type": "object"},
                    "annotations": {
                        "kind": "search",
                        "side_effect_level": "read_only"
                    }
                },
                {
                    "name": "rare.lookup",
                    "defer_loading": true,
                    "parameters": {"type": "object"},
                    "annotations": {
                        "kind": "search",
                        "side_effect_level": "read_only"
                    }
                }
            ]
        });
        let manifest = binding_manifest_from_tool_surface(
            &tools,
            BindingManifestOptions {
                side_effect_ceiling: SideEffectLevel::ReadOnly,
                ..BindingManifestOptions::default()
            },
        );
        let read = manifest.find_by_name("read.file").expect("read binding");
        assert_eq!(read.binding, "read_file");
        assert_eq!(read.path_args, vec!["path"]);
        assert_eq!(read.policy.disposition, BindingPolicyDisposition::Allowed);
        assert!(manifest.find_by_name("write_file").is_none());
        assert_eq!(
            manifest
                .find_by_name("host.read")
                .expect("host binding")
                .source,
            "host_bridge"
        );
        assert_eq!(
            manifest
                .find_by_name("mcp.search")
                .expect("mcp binding")
                .source,
            "mcp_server"
        );
        let deferred = manifest
            .find_by_name("rare.lookup")
            .expect("deferred binding");
        assert!(deferred.deferred);
        assert_eq!(deferred.source, "deferred");
        let manifest_with_denied = binding_manifest_from_tool_surface(
            &tools,
            BindingManifestOptions {
                side_effect_ceiling: SideEffectLevel::ReadOnly,
                include_denied: true,
                ..BindingManifestOptions::default()
            },
        );
        let write = manifest_with_denied
            .find_by_name("write_file")
            .expect("write binding");
        assert_eq!(write.policy.disposition, BindingPolicyDisposition::Denied);
        assert!(manifest.hash().unwrap().starts_with("sha256:"));
    }

    #[test]
    fn manifest_compact_form_and_typescript_declarations_are_stable() {
        let tools = serde_json::json!([
            {
                "name": "read.file",
                "parameters": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string"},
                        "limit": {"type": "integer"}
                    }
                },
                "returns": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}}
                },
                "annotations": {"kind": "read", "side_effect_level": "read_only"}
            }
        ]);
        let manifest =
            binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
        let compact = manifest.to_compact_value();
        assert_eq!(compact["bindings"][0]["binding"], "read_file");
        assert!(compact["bindings"][0].get("input_schema").is_none());
        let declarations = composition_typescript_declarations(&manifest);
        assert!(declarations.contains("export declare function read_file"));
        assert!(declarations.contains("path: string"));
        assert!(declarations.contains("limit?: number"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_executes_read_only_binding_and_records_child_trace() {
        let tools = serde_json::json!([
            {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {"type": "object", "required": ["path"]},
                "annotations": {
                    "kind": "read",
                    "side_effect_level": "read_only",
                    "arg_schema": {"path_params": ["path"]},
                    "capabilities": {"workspace": ["read_text"]},
                    "inline_result": true
                },
                "metadata": {"mock_output": {"text": "hello"}}
            }
        ]);
        let manifest =
            binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-test".to_string(),
                snippet: "let file = read_file({path: \"README.md\"})\nreturn {text: file.text}"
                    .to_string(),
                manifest,
                ..CompositionExecutionRequest::default()
            },
            Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        )
        .await;
        assert!(report.ok, "{}", report.summary);
        assert_eq!(report.child_calls.len(), 1);
        assert_eq!(report.child_results[0].status, ToolCallStatus::Completed);
        assert_eq!(report.run.result.unwrap()["text"], "hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_denies_mutating_binding_calls() {
        let tools = serde_json::json!([
            {
                "name": "write_file",
                "parameters": {"type": "object"},
                "annotations": {
                    "kind": "edit",
                    "side_effect_level": "workspace_write"
                }
            }
        ]);
        let manifest =
            binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-deny".to_string(),
                snippet: "return write_file({path: \"x\", content: \"bad\"})".to_string(),
                manifest,
                ..CompositionExecutionRequest::default()
            },
            Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        )
        .await;
        assert!(!report.ok);
        assert_eq!(
            report.run.failure_category,
            Some(CompositionFailureCategory::PolicyDenied)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_records_denied_manifest_binding_as_child_failure() {
        let tools = serde_json::json!([
            {
                "name": "write_file",
                "parameters": {"type": "object"},
                "annotations": {
                    "kind": "edit",
                    "side_effect_level": "workspace_write"
                }
            }
        ]);
        let manifest = binding_manifest_from_tool_surface(
            &tools,
            BindingManifestOptions {
                include_denied: true,
                ..BindingManifestOptions::default()
            },
        );
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-denied-child".to_string(),
                snippet: "return write_file({path: \"x\", content: \"bad\"})".to_string(),
                manifest,
                ..CompositionExecutionRequest::default()
            },
            Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        )
        .await;
        assert!(!report.ok);
        assert_eq!(report.child_calls.len(), 1);
        assert_eq!(report.child_results[0].status, ToolCallStatus::Failed);
        assert_eq!(
            report.child_results[0].error_category,
            Some(ToolCallErrorCategory::PermissionDenied)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_enforces_child_call_cap() {
        let tools = serde_json::json!([
            {
                "name": "read_file",
                "parameters": {"type": "object"},
                "annotations": {"kind": "read", "side_effect_level": "read_only"},
                "metadata": {"mock_output": {"text": "hello"}}
            }
        ]);
        let manifest =
            binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-cap".to_string(),
                snippet: "let _a = read_file({path: \"a\"})\nreturn read_file({path: \"b\"})"
                    .to_string(),
                manifest,
                limits: CompositionExecutionLimits {
                    max_operations: 1,
                    ..CompositionExecutionLimits::default()
                },
                ..CompositionExecutionRequest::default()
            },
            Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        )
        .await;
        assert!(!report.ok);
        assert_eq!(
            report.run.failure_category,
            Some(CompositionFailureCategory::Timeout)
        );
        assert_eq!(report.child_calls.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_dispatcher_closure_receives_real_inputs_and_returns_outputs() {
        use std::cell::RefCell;
        let tools = serde_json::json!([
            {
                "name": "read_file",
                "parameters": {"type": "object", "required": ["path"]},
                "annotations": {"kind": "read", "side_effect_level": "read_only"},
            }
        ]);
        let manifest =
            binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());

        struct CapturingHost {
            calls: RefCell<Vec<(String, Value)>>,
        }
        #[async_trait::async_trait(?Send)]
        impl CompositionToolHost for CapturingHost {
            async fn call(
                &self,
                binding: &BindingManifestEntry,
                input: Value,
            ) -> CompositionToolOutput {
                self.calls
                    .borrow_mut()
                    .push((binding.name.clone(), input.clone()));
                CompositionToolOutput::ok(serde_json::json!({
                    "path": input.get("path").cloned().unwrap_or(Value::Null),
                    "text": "real-file-bytes",
                }))
            }
        }
        let host = Rc::new(CapturingHost {
            calls: RefCell::new(Vec::new()),
        });
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-dispatch".into(),
                snippet: "let f = read_file({path: \"README.md\"})\nreturn f.text".into(),
                manifest,
                ..CompositionExecutionRequest::default()
            },
            host.clone(),
        )
        .await;
        assert!(report.ok, "{}", report.summary);
        assert_eq!(host.calls.borrow().len(), 1);
        assert_eq!(host.calls.borrow()[0].0, "read_file");
        assert_eq!(
            host.calls.borrow()[0].1.get("path").and_then(Value::as_str),
            Some("README.md")
        );
        assert_eq!(
            report.run.result.as_ref().and_then(Value::as_str),
            Some("real-file-bytes")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_composition_enforces_output_cap() {
        let report = execute_harn_composition(
            CompositionExecutionRequest {
                run_id: "run-output-cap".to_string(),
                snippet: "return \"0123456789\"".to_string(),
                limits: CompositionExecutionLimits {
                    max_output_bytes: 4,
                    ..CompositionExecutionLimits::default()
                },
                ..CompositionExecutionRequest::default()
            },
            Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        )
        .await;
        assert!(!report.ok);
        assert!(report.summary.contains("max_output_bytes"));
    }

    #[test]
    fn composition_report_can_be_projected_to_crystallization_trace() {
        let report = CompositionExecutionReport {
            schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
            ok: true,
            run: CompositionRunEnvelope::read_only(
                "run-crystal",
                "harn",
                "sha256:snippet",
                "sha256:manifest",
            ),
            child_calls: vec![CompositionChildCall {
                run_id: "run-crystal".into(),
                tool_call_id: "run-crystal:0".into(),
                tool_name: "read_file".into(),
                operation_index: 0,
                requested_side_effect_level: SideEffectLevel::ReadOnly,
                annotations: Some(ToolAnnotations {
                    capabilities: BTreeMap::from([(
                        "workspace".to_string(),
                        vec!["read_text".to_string()],
                    )]),
                    ..ToolAnnotations::default()
                }),
                raw_input: serde_json::json!({"path": "README.md"}),
                ..CompositionChildCall::default()
            }],
            child_results: vec![CompositionChildResult {
                run_id: "run-crystal".into(),
                tool_call_id: "run-crystal:0".into(),
                tool_name: "read_file".into(),
                operation_index: 0,
                status: ToolCallStatus::Completed,
                raw_output: Some(serde_json::json!({"text": "hello"})),
                ..CompositionChildResult::default()
            }],
            summary: "ok".into(),
        };
        let trace = composition_crystallization_trace(&report, &serde_json::json!({}));
        assert_eq!(trace["source"], "composition_run");
        assert_eq!(trace["actions"][0]["name"], "execute_composition");
        assert_eq!(trace["actions"][1]["name"], "read_file");
        assert_eq!(trace["replay_run"]["run_id"], "run-crystal");
        assert_eq!(
            trace["replay_run"]["effect_receipts"][0]["kind"],
            "composition_parent"
        );
        assert_eq!(
            trace["replay_run"]["effect_receipts"][1]["kind"],
            "composition_child"
        );
        assert_eq!(
            trace["replay_run"]["effect_receipts"][1]["tool_call_id"],
            "run-crystal:0"
        );
        assert_eq!(
            trace["actions"][0]["capabilities"][0],
            "workspace.read_text"
        );
    }

    #[test]
    fn composition_report_projects_stable_agent_event_graph() {
        let report = CompositionExecutionReport {
            schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
            ok: true,
            run: CompositionRunEnvelope::read_only(
                "run-events",
                "harn",
                "sha256:snippet",
                "sha256:manifest",
            ),
            child_calls: vec![CompositionChildCall {
                run_id: "run-events".into(),
                tool_call_id: "run-events:0".into(),
                tool_name: "read_file".into(),
                operation_index: 0,
                ..CompositionChildCall::default()
            }],
            child_results: vec![CompositionChildResult {
                run_id: "run-events".into(),
                tool_call_id: "run-events:0".into(),
                tool_name: "read_file".into(),
                operation_index: 0,
                status: ToolCallStatus::Completed,
                ..CompositionChildResult::default()
            }],
            summary: "ok".into(),
        };
        let events = composition_report_events("session-events", &report);
        assert!(matches!(events[0], AgentEvent::CompositionStart { .. }));
        assert!(matches!(events[1], AgentEvent::CompositionChildCall { .. }));
        assert!(matches!(
            events[2],
            AgentEvent::CompositionChildResult { .. }
        ));
        assert!(matches!(events[3], AgentEvent::CompositionFinish { .. }));
    }
}
