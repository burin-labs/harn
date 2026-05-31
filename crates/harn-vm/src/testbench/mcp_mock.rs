//! Deterministic MCP mock, cassette, and stateful-world primitives.
//!
//! This module is intentionally transport-light: it owns the artifact
//! schemas and pure JSON-RPC behavior, while `harn-cli` owns stdio
//! proxy/server loops. That keeps record/replay and simulated-world evals
//! reusable from conformance tests, harn-cloud, and downstream harnesses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use crate::mcp_protocol::{
    completions_capability, server_discover_result, tasks_capability, DRAFT_PROTOCOL_VERSION,
    PROTOCOL_VERSION,
};

pub const MCP_CASSETTE_SCHEMA_VERSION: u32 = 1;
pub const MCP_WORLD_SCHEMA_VERSION: u32 = 1;
pub const MCP_WORLD_EVAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCassette {
    #[serde(default = "default_cassette_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_cassette_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub interactions: Vec<McpInteraction>,
}

fn default_cassette_schema_version() -> u32 {
    MCP_CASSETTE_SCHEMA_VERSION
}

fn default_cassette_protocol() -> String {
    "mcp-jsonrpc-stdio".to_string()
}

impl Default for McpCassette {
    fn default() -> Self {
        Self {
            schema_version: MCP_CASSETTE_SCHEMA_VERSION,
            protocol: default_cassette_protocol(),
            interactions: Vec::new(),
        }
    }
}

impl McpCassette {
    pub fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|error| format!("read cassette {}: {error}", path.display()))?;
        let cassette: Self = serde_json::from_str(&body)
            .map_err(|error| format!("parse cassette {}: {error}", path.display()))?;
        if cassette.schema_version > MCP_CASSETTE_SCHEMA_VERSION {
            return Err(format!(
                "cassette {} declares schema_version {} but this runtime supports up to {MCP_CASSETTE_SCHEMA_VERSION}",
                path.display(),
                cassette.schema_version
            ));
        }
        Ok(cassette)
    }

    pub fn persist(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
            }
        }
        let body = serde_json::to_string_pretty(self)
            .map_err(|error| format!("serialize cassette: {error}"))?;
        std::fs::write(path, format!("{body}\n"))
            .map_err(|error| format!("write cassette {}: {error}", path.display()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpInteraction {
    pub seq: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub notification: bool,
    pub match_digest: String,
    pub request_digest: String,
    pub response_digest: String,
    pub latency_ms: u64,
    pub request: JsonValue,
    pub response: JsonValue,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_metadata: Vec<McpRecordedToolMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRecordedToolMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations_digest: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn record_interaction(
    seq: u64,
    request: &JsonValue,
    response: &JsonValue,
    latency_ms: u64,
) -> McpInteraction {
    let policy = crate::redact::current_policy();
    let request = policy.redact_json(request);
    let response = policy.redact_json(response);
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();
    let notification = request.get("id").is_none();
    let match_digest = json_digest(&request_match_surface(&request));
    let request_digest = json_digest(&without_jsonrpc_id(&request));
    let response_digest = json_digest(&without_jsonrpc_id(&response));
    let tool_metadata = extract_tool_metadata(&method, &response);
    let structured_content = extract_structured_content(&method, &response);

    McpInteraction {
        seq,
        method,
        notification,
        match_digest,
        request_digest,
        response_digest,
        latency_ms,
        request,
        response,
        tool_metadata,
        structured_content,
    }
}

#[derive(Debug, Default)]
pub struct McpCassetteRecorder {
    next_seq: AtomicU64,
    interactions: Mutex<Vec<McpInteraction>>,
}

impl McpCassetteRecorder {
    pub fn record(&self, request: &JsonValue, response: &JsonValue, latency_ms: u64) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let interaction = record_interaction(seq, request, response, latency_ms);
        self.interactions
            .lock()
            .expect("MCP cassette recorder mutex poisoned")
            .push(interaction);
    }

    pub fn snapshot(&self) -> McpCassette {
        McpCassette {
            interactions: self
                .interactions
                .lock()
                .expect("MCP cassette recorder mutex poisoned")
                .clone(),
            ..McpCassette::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpCassetteReplayer {
    cassette: McpCassette,
    next: usize,
}

impl McpCassetteReplayer {
    pub fn new(cassette: McpCassette) -> Self {
        Self { cassette, next: 0 }
    }

    pub fn replay_request(
        &mut self,
        request: &JsonValue,
    ) -> Result<Option<JsonValue>, McpReplayError> {
        let index = self.next;
        let Some(interaction) = self.cassette.interactions.get(index) else {
            return Err(McpReplayError {
                index,
                message: "cassette exhausted before request".to_string(),
            });
        };

        let actual_method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let actual_digest = json_digest(&request_match_surface(
            &crate::redact::current_policy().redact_json(request),
        ));
        if interaction.method != actual_method || interaction.match_digest != actual_digest {
            return Err(McpReplayError {
                index,
                message: format!(
                    "request mismatch: expected method={} digest={} got method={} digest={}",
                    interaction.method, interaction.match_digest, actual_method, actual_digest
                ),
            });
        }

        self.next += 1;
        if interaction.notification {
            return Ok(None);
        }
        let mut response = interaction.response.clone();
        rewrite_jsonrpc_id(&mut response, request.get("id").cloned());
        Ok(Some(response))
    }

    pub fn is_finished(&self) -> bool {
        self.next >= self.cassette.interactions.len()
    }

    pub fn remaining(&self) -> usize {
        self.cassette.interactions.len().saturating_sub(self.next)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpReplayError {
    pub index: usize,
    pub message: String,
}

impl std::fmt::Display for McpReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCP cassette replay mismatch at #{}: {}",
            self.index, self.message
        )
    }
}

impl std::error::Error for McpReplayError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpCassetteVerifyReport {
    pub schema_version: u32,
    pub passed: bool,
    pub recorded_interactions: usize,
    pub candidate_interactions: usize,
    pub divergences: Vec<McpCassetteDivergence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpCassetteDivergence {
    pub index: usize,
    pub category: String,
    pub message: String,
}

pub fn verify_cassettes(
    recorded: &McpCassette,
    candidate: &McpCassette,
) -> McpCassetteVerifyReport {
    let mut divergences = Vec::new();
    let max = recorded
        .interactions
        .len()
        .max(candidate.interactions.len());
    for index in 0..max {
        match (
            recorded.interactions.get(index),
            candidate.interactions.get(index),
        ) {
            (Some(expected), Some(actual)) => {
                push_interaction_divergences(index, expected, actual, &mut divergences);
            }
            (Some(expected), None) => divergences.push(McpCassetteDivergence {
                index,
                category: "missing_candidate_interaction".to_string(),
                message: format!(
                    "candidate ended before recorded interaction method={}",
                    expected.method
                ),
            }),
            (None, Some(actual)) => divergences.push(McpCassetteDivergence {
                index,
                category: "extra_candidate_interaction".to_string(),
                message: format!(
                    "candidate produced extra interaction method={}",
                    actual.method
                ),
            }),
            (None, None) => {}
        }
    }

    McpCassetteVerifyReport {
        schema_version: MCP_CASSETTE_SCHEMA_VERSION,
        passed: divergences.is_empty(),
        recorded_interactions: recorded.interactions.len(),
        candidate_interactions: candidate.interactions.len(),
        divergences,
    }
}

fn push_interaction_divergences(
    index: usize,
    expected: &McpInteraction,
    actual: &McpInteraction,
    divergences: &mut Vec<McpCassetteDivergence>,
) {
    if expected.method != actual.method {
        divergences.push(McpCassetteDivergence {
            index,
            category: "method".to_string(),
            message: format!(
                "method diverged: recorded={} candidate={}",
                expected.method, actual.method
            ),
        });
    }
    if expected.match_digest != actual.match_digest {
        divergences.push(McpCassetteDivergence {
            index,
            category: "request".to_string(),
            message: format!(
                "request match surface diverged: recorded={} candidate={}",
                expected.match_digest, actual.match_digest
            ),
        });
    }
    if expected.response_digest != actual.response_digest {
        divergences.push(McpCassetteDivergence {
            index,
            category: "response".to_string(),
            message: format!(
                "response digest diverged: recorded={} candidate={}",
                expected.response_digest, actual.response_digest
            ),
        });
    }
    if expected.tool_metadata != actual.tool_metadata {
        divergences.push(McpCassetteDivergence {
            index,
            category: "tool_metadata".to_string(),
            message: "tool outputSchema or annotations diverged".to_string(),
        });
    }
    if expected.structured_content != actual.structured_content {
        divergences.push(McpCassetteDivergence {
            index,
            category: "structured_content".to_string(),
            message: "tools/call structuredContent diverged".to_string(),
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpWorldSpec {
    #[serde(default = "default_world_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_world_name")]
    pub name: String,
    #[serde(default = "default_world_version")]
    pub version: String,
    #[serde(default)]
    pub seed: u64,
    #[serde(default = "default_json_object")]
    pub initial_state: JsonValue,
    #[serde(default)]
    pub goal_state: Option<JsonValue>,
    #[serde(default)]
    pub tools: Vec<McpWorldToolSpec>,
    #[serde(default)]
    pub faults: Vec<McpWorldFaultSpec>,
}

impl Default for McpWorldSpec {
    fn default() -> Self {
        Self {
            schema_version: MCP_WORLD_SCHEMA_VERSION,
            name: default_world_name(),
            version: default_world_version(),
            seed: 0,
            initial_state: default_json_object(),
            goal_state: None,
            tools: Vec::new(),
            faults: Vec::new(),
        }
    }
}

fn default_world_schema_version() -> u32 {
    MCP_WORLD_SCHEMA_VERSION
}

fn default_world_name() -> String {
    "harn-mcp-world".to_string()
}

fn default_world_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_json_object() -> JsonValue {
    json!({})
}

impl McpWorldSpec {
    pub fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|error| format!("read world spec {}: {error}", path.display()))?;
        let spec: Self = serde_json::from_str(&body)
            .map_err(|error| format!("parse world spec {}: {error}", path.display()))?;
        if spec.schema_version > MCP_WORLD_SCHEMA_VERSION {
            return Err(format!(
                "world spec {} declares schema_version {} but this runtime supports up to {MCP_WORLD_SCHEMA_VERSION}",
                path.display(),
                spec.schema_version
            ));
        }
        Ok(spec)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpWorldToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_input_schema", alias = "inputSchema")]
    pub input_schema: JsonValue,
    #[serde(
        default,
        alias = "outputSchema",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<JsonValue>,
    pub operation: McpWorldOperation,
}

fn default_input_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": true
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpWorldOperation {
    Read {
        path: String,
    },
    Set {
        path: String,
        value_arg: String,
    },
    Merge {
        path: String,
        object_arg: String,
    },
    Append {
        path: String,
        value_arg: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id_prefix: Option<String>,
    },
    Delete {
        path: String,
    },
    Noop {
        #[serde(default = "default_json_object")]
        result: JsonValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpWorldFaultSpec {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_call: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    pub fault: McpWorldFault,
}

impl McpWorldFaultSpec {
    fn applies_to(&self, tool: &str, call_number: u64) -> bool {
        if self.tool != tool && self.tool != "*" {
            return false;
        }
        if self.at_call == Some(call_number) {
            return true;
        }
        self.every
            .is_some_and(|every| every > 0 && call_number.is_multiple_of(every))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpWorldFault {
    JsonRpcError {
        code: i64,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<JsonValue>,
    },
    ToolError {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured_content: Option<JsonValue>,
    },
    Timeout {
        #[serde(default)]
        message: Option<String>,
    },
    PartialWrite {
        path: String,
        value: JsonValue,
        #[serde(default = "default_partial_write_message")]
        message: String,
    },
}

fn default_partial_write_message() -> String {
    "simulated partial write".to_string()
}

#[derive(Debug, Clone)]
pub struct McpWorldRuntime {
    spec: McpWorldSpec,
    state: JsonValue,
    tool_calls: BTreeMap<String, u64>,
    total_tool_calls: u64,
}

impl McpWorldRuntime {
    pub fn new(spec: McpWorldSpec) -> Self {
        let state = spec.initial_state.clone();
        Self {
            spec,
            state,
            tool_calls: BTreeMap::new(),
            total_tool_calls: 0,
        }
    }

    pub fn state(&self) -> &JsonValue {
        &self.state
    }

    pub fn into_state(self) -> JsonValue {
        self.state
    }

    pub fn handle_json_rpc(&mut self, request: JsonValue) -> Option<JsonValue> {
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        let id = id?;

        Some(match method {
            "initialize" => self.handle_initialize(id),
            crate::mcp_protocol::METHOD_SERVER_DISCOVER => self.handle_server_discover(id),
            "ping" => crate::jsonrpc::response(id, json!({})),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(id, &params),
            "resources/list" => self.handle_resources_list(id),
            "resources/read" => self.handle_resources_read(id, &params),
            other => {
                crate::jsonrpc::error_response(id, -32601, &format!("Method not found: {other}"))
            }
        })
    }

    fn handle_initialize(&self, id: JsonValue) -> JsonValue {
        crate::jsonrpc::response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": self.capabilities(),
                "serverInfo": self.server_info(),
                "instructions": "Deterministic Harn MCP simulated world.",
            }),
        )
    }

    fn handle_server_discover(&self, id: JsonValue) -> JsonValue {
        crate::jsonrpc::response(
            id,
            server_discover_result(
                self.capabilities(),
                self.server_info(),
                Some("Deterministic Harn MCP simulated world."),
            ),
        )
    }

    fn capabilities(&self) -> JsonValue {
        json!({
            "tools": { "listChanged": false },
            "resources": { "listChanged": false },
            "tasks": tasks_capability(),
            "completions": completions_capability(),
        })
    }

    fn server_info(&self) -> JsonValue {
        json!({
            "name": self.spec.name,
            "version": self.spec.version,
            "protocolVersion": DRAFT_PROTOCOL_VERSION,
        })
    }

    fn handle_tools_list(&self, id: JsonValue) -> JsonValue {
        let tools: Vec<JsonValue> = self
            .spec
            .tools
            .iter()
            .map(|tool| {
                let mut entry = json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                });
                if let Some(output_schema) = &tool.output_schema {
                    entry["outputSchema"] = output_schema.clone();
                }
                if let Some(annotations) = &tool.annotations {
                    entry["annotations"] = annotations.clone();
                }
                entry
            })
            .collect();
        crate::jsonrpc::response(id, json!({ "tools": tools }))
    }

    fn handle_tools_call(&mut self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let Some(name) = params.get("name").and_then(JsonValue::as_str) else {
            return crate::jsonrpc::error_response(id, -32602, "tools/call requires params.name");
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(tool) = self
            .spec
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
        else {
            return crate::jsonrpc::error_response(
                id,
                -32602,
                &format!("unknown simulated-world tool: {name}"),
            );
        };

        self.total_tool_calls += 1;
        let call_number = {
            let entry = self.tool_calls.entry(name.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        if let Some(fault) = self
            .spec
            .faults
            .iter()
            .find(|fault| fault.applies_to(name, call_number))
            .cloned()
        {
            return self.apply_fault(id, &fault.fault, &arguments);
        }

        match self.apply_operation(&tool.operation, &arguments) {
            Ok(structured) => successful_tool_response(id, structured),
            Err(message) => tool_error_response(id, message, None),
        }
    }

    fn apply_fault(
        &mut self,
        id: JsonValue,
        fault: &McpWorldFault,
        arguments: &JsonValue,
    ) -> JsonValue {
        match fault {
            McpWorldFault::JsonRpcError {
                code,
                message,
                data,
            } => match data {
                Some(data) => {
                    crate::jsonrpc::error_response_with_data(id, *code, message, data.clone())
                }
                None => crate::jsonrpc::error_response(id, *code, message),
            },
            McpWorldFault::ToolError {
                message,
                structured_content,
            } => tool_error_response(id, message.clone(), structured_content.clone()),
            McpWorldFault::Timeout { message } => crate::jsonrpc::error_response_with_data(
                id,
                -32070,
                message
                    .as_deref()
                    .unwrap_or("simulated MCP timeout without sleeping"),
                json!({ "type": "timeout" }),
            ),
            McpWorldFault::PartialWrite {
                path,
                value,
                message,
            } => {
                let rendered = render_state_path(path, arguments).unwrap_or_else(|_| path.clone());
                let _ = set_state_value(&mut self.state, &rendered, value.clone());
                tool_error_response(
                    id,
                    message.clone(),
                    Some(json!({
                        "partialWrite": true,
                        "path": rendered,
                    })),
                )
            }
        }
    }

    fn apply_operation(
        &mut self,
        operation: &McpWorldOperation,
        arguments: &JsonValue,
    ) -> Result<JsonValue, String> {
        match operation {
            McpWorldOperation::Read { path } => {
                let path = render_state_path(path, arguments)?;
                let value = self
                    .state
                    .pointer(&path)
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                Ok(json!({ "value": value }))
            }
            McpWorldOperation::Set { path, value_arg } => {
                let path = render_state_path(path, arguments)?;
                let value = argument_value(arguments, value_arg)?;
                set_state_value(&mut self.state, &path, value.clone())?;
                Ok(json!({ "ok": true, "path": path, "value": value }))
            }
            McpWorldOperation::Merge { path, object_arg } => {
                let path = render_state_path(path, arguments)?;
                let patch = argument_value(arguments, object_arg)?;
                if !patch.is_object() {
                    return Err(format!("argument `{object_arg}` must be an object"));
                }
                merge_state_object(&mut self.state, &path, patch)?;
                Ok(
                    json!({ "ok": true, "path": path, "value": self.state.pointer(&path).cloned().unwrap_or(JsonValue::Null) }),
                )
            }
            McpWorldOperation::Append {
                path,
                value_arg,
                id_field,
                id_prefix,
            } => {
                let path = render_state_path(path, arguments)?;
                let mut value = argument_value(arguments, value_arg)?;
                if let (Some(field), Some(object)) = (id_field, value.as_object_mut()) {
                    if !object.contains_key(field) {
                        object.insert(
                            field.clone(),
                            JsonValue::String(seeded_id(
                                self.spec.seed,
                                self.total_tool_calls,
                                id_prefix.as_deref().unwrap_or("id"),
                            )),
                        );
                    }
                }
                append_state_value(&mut self.state, &path, value.clone())?;
                Ok(json!({ "ok": true, "path": path, "value": value }))
            }
            McpWorldOperation::Delete { path } => {
                let path = render_state_path(path, arguments)?;
                delete_state_value(&mut self.state, &path)?;
                Ok(json!({ "ok": true, "path": path }))
            }
            McpWorldOperation::Noop { result } => Ok(result.clone()),
        }
    }

    fn handle_resources_list(&self, id: JsonValue) -> JsonValue {
        crate::jsonrpc::response(
            id,
            json!({
                "resources": [{
                    "uri": "world://state",
                    "name": "simulated-world-state",
                    "description": "Current deterministic simulated-world state.",
                    "mimeType": "application/json"
                }]
            }),
        )
    }

    fn handle_resources_read(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let uri = params.get("uri").and_then(JsonValue::as_str).unwrap_or("");
        if uri != "world://state" {
            return crate::jsonrpc::error_response(
                id,
                -32602,
                "only world://state is available in the simulated-world mock",
            );
        }
        crate::jsonrpc::response(
            id,
            json!({
                "contents": [{
                    "uri": "world://state",
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&self.state).unwrap_or_else(|_| "{}".to_string())
                }]
            }),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpWorldEvalReport {
    pub schema_version: u32,
    pub passed: bool,
    pub run_count: usize,
    pub pass_count: usize,
    pub pass_rate: f64,
    pub pass_power_k: f64,
    pub runs: Vec<McpWorldRunScore>,
}

impl McpWorldEvalReport {
    pub fn from_scores(runs: Vec<McpWorldRunScore>) -> Self {
        let run_count = runs.len();
        let pass_count = runs.iter().filter(|score| score.passed).count();
        let pass_rate = if run_count == 0 {
            0.0
        } else {
            pass_count as f64 / run_count as f64
        };
        let pass_power_k = pass_rate.powi(run_count as i32);
        Self {
            schema_version: MCP_WORLD_EVAL_SCHEMA_VERSION,
            passed: run_count > 0 && pass_count == run_count,
            run_count,
            pass_count,
            pass_rate,
            pass_power_k,
            runs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpWorldRunScore {
    pub passed: bool,
    pub goal_mismatches: Vec<McpWorldStateMismatch>,
    pub collateral_damage: Vec<McpWorldStateMismatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpWorldStateMismatch {
    pub path: String,
    pub expected: JsonValue,
    pub actual: JsonValue,
}

pub fn score_world_state(spec: &McpWorldSpec, final_state: &JsonValue) -> McpWorldRunScore {
    let goal = spec.goal_state.clone().unwrap_or_else(default_json_object);
    let goal_leaves = flatten_json_leaves(&goal);
    let mut goal_mismatches = Vec::new();
    for (path, expected) in &goal_leaves {
        let actual = final_state
            .pointer(path)
            .cloned()
            .unwrap_or(JsonValue::Null);
        if &actual != expected {
            goal_mismatches.push(McpWorldStateMismatch {
                path: path.clone(),
                expected: expected.clone(),
                actual,
            });
        }
    }

    let initial_leaves = flatten_json_leaves(&spec.initial_state);
    let final_leaves = flatten_json_leaves(final_state);
    let goal_paths: BTreeSet<String> = goal_leaves.keys().cloned().collect();
    let all_paths: BTreeSet<String> = initial_leaves
        .keys()
        .chain(final_leaves.keys())
        .cloned()
        .collect();
    let mut collateral_damage = Vec::new();
    for path in all_paths {
        if goal_paths.contains(&path) {
            continue;
        }
        let expected = initial_leaves
            .get(&path)
            .cloned()
            .unwrap_or(JsonValue::Null);
        let actual = final_leaves.get(&path).cloned().unwrap_or(JsonValue::Null);
        if expected != actual {
            collateral_damage.push(McpWorldStateMismatch {
                path,
                expected,
                actual,
            });
        }
    }

    McpWorldRunScore {
        passed: goal_mismatches.is_empty() && collateral_damage.is_empty(),
        goal_mismatches,
        collateral_damage,
    }
}

fn successful_tool_response(id: JsonValue, structured: JsonValue) -> JsonValue {
    crate::jsonrpc::response(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&structured).unwrap_or_else(|_| "{}".to_string())
            }],
            "structuredContent": structured,
            "isError": false
        }),
    )
}

fn tool_error_response(
    id: JsonValue,
    message: String,
    structured_content: Option<JsonValue>,
) -> JsonValue {
    let mut result = json!({
        "content": [{
            "type": "text",
            "text": message
        }],
        "isError": true
    });
    if let Some(structured_content) = structured_content {
        result["structuredContent"] = structured_content;
    }
    crate::jsonrpc::response(id, result)
}

fn argument_value(arguments: &JsonValue, name: &str) -> Result<JsonValue, String> {
    arguments
        .get(name)
        .cloned()
        .ok_or_else(|| format!("missing argument `{name}`"))
}

fn render_state_path(path: &str, arguments: &JsonValue) -> Result<String, String> {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            out.push(ch);
            continue;
        }
        let mut name = String::new();
        for next in chars.by_ref() {
            if next == '}' {
                break;
            }
            name.push(next);
        }
        let value = argument_value(arguments, &name)?;
        let rendered = match value {
            JsonValue::String(value) => value,
            JsonValue::Number(number) => number.to_string(),
            JsonValue::Bool(value) => value.to_string(),
            _ => {
                return Err(format!(
                    "path template argument `{name}` must be scalar, got {value}"
                ))
            }
        };
        out.push_str(&escape_json_pointer_segment(&rendered));
    }
    if out.is_empty() || out.starts_with('/') {
        Ok(out)
    } else {
        Ok(format!("/{out}"))
    }
}

fn set_state_value(state: &mut JsonValue, path: &str, value: JsonValue) -> Result<(), String> {
    if path.is_empty() {
        *state = value;
        return Ok(());
    }
    let segments = pointer_segments(path)?;
    let parent = ensure_parent_object(state, &segments)?;
    let key = segments
        .last()
        .ok_or_else(|| "state path must not be empty".to_string())?;
    parent.insert(key.clone(), value);
    Ok(())
}

fn merge_state_object(state: &mut JsonValue, path: &str, patch: JsonValue) -> Result<(), String> {
    let patch = patch
        .as_object()
        .cloned()
        .ok_or_else(|| "merge patch must be an object".to_string())?;
    if state.pointer(path).is_none() {
        set_state_value(state, path, json!({}))?;
    }
    let target = state
        .pointer_mut(path)
        .ok_or_else(|| format!("state path not found: {path}"))?;
    let Some(target) = target.as_object_mut() else {
        return Err(format!("state path is not an object: {path}"));
    };
    for (key, value) in patch {
        target.insert(key, value);
    }
    Ok(())
}

fn append_state_value(state: &mut JsonValue, path: &str, value: JsonValue) -> Result<(), String> {
    if state.pointer(path).is_none() {
        set_state_value(state, path, json!([]))?;
    }
    let target = state
        .pointer_mut(path)
        .ok_or_else(|| format!("state path not found: {path}"))?;
    let Some(array) = target.as_array_mut() else {
        return Err(format!("state path is not an array: {path}"));
    };
    array.push(value);
    Ok(())
}

fn delete_state_value(state: &mut JsonValue, path: &str) -> Result<(), String> {
    if path.is_empty() {
        *state = JsonValue::Null;
        return Ok(());
    }
    let segments = pointer_segments(path)?;
    let parent_path = if segments.len() == 1 {
        String::new()
    } else {
        format!(
            "/{}",
            segments[..segments.len() - 1]
                .iter()
                .map(|segment| escape_json_pointer_segment(segment))
                .collect::<Vec<_>>()
                .join("/")
        )
    };
    let key = segments
        .last()
        .ok_or_else(|| "state path must not be empty".to_string())?;
    let parent = if parent_path.is_empty() {
        state
    } else {
        state
            .pointer_mut(&parent_path)
            .ok_or_else(|| format!("state parent path not found: {parent_path}"))?
    };
    let Some(object) = parent.as_object_mut() else {
        return Err(format!("state parent path is not an object: {parent_path}"));
    };
    object.remove(key);
    Ok(())
}

fn ensure_parent_object<'a>(
    state: &'a mut JsonValue,
    segments: &[String],
) -> Result<&'a mut JsonMap<String, JsonValue>, String> {
    if segments.is_empty() {
        return Err("state path must not be empty".to_string());
    }
    let mut cursor = state;
    for segment in &segments[..segments.len() - 1] {
        if !cursor.is_object() {
            *cursor = json!({});
        }
        let object = cursor
            .as_object_mut()
            .ok_or_else(|| "state path parent is not an object".to_string())?;
        cursor = object.entry(segment.clone()).or_insert_with(|| json!({}));
    }
    if !cursor.is_object() {
        *cursor = json!({});
    }
    cursor
        .as_object_mut()
        .ok_or_else(|| "state path parent is not an object".to_string())
}

fn pointer_segments(path: &str) -> Result<Vec<String>, String> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if !path.starts_with('/') {
        return Err(format!("state path must be a JSON pointer, got `{path}`"));
    }
    Ok(path
        .trim_start_matches('/')
        .split('/')
        .map(unescape_json_pointer_segment)
        .collect())
}

fn escape_json_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn unescape_json_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

fn seeded_id(seed: u64, call_number: u64, prefix: &str) -> String {
    let digest = blake3::hash(format!("{seed}:{call_number}:{prefix}").as_bytes());
    format!("{prefix}_{}", &digest.to_hex().to_string()[..12])
}

fn flatten_json_leaves(value: &JsonValue) -> BTreeMap<String, JsonValue> {
    let mut out = BTreeMap::new();
    flatten_json_leaves_into("", value, &mut out);
    out
}

fn flatten_json_leaves_into(path: &str, value: &JsonValue, out: &mut BTreeMap<String, JsonValue>) {
    match value {
        JsonValue::Object(object) if !object.is_empty() => {
            for (key, child) in object {
                let child_path = format!("{path}/{}", escape_json_pointer_segment(key));
                flatten_json_leaves_into(&child_path, child, out);
            }
        }
        JsonValue::Array(items) if !items.is_empty() => {
            for (index, child) in items.iter().enumerate() {
                let child_path = format!("{path}/{index}");
                flatten_json_leaves_into(&child_path, child, out);
            }
        }
        _ => {
            out.insert(path.to_string(), value.clone());
        }
    }
}

fn request_match_surface(request: &JsonValue) -> JsonValue {
    json!({
        "method": request.get("method").cloned().unwrap_or(JsonValue::Null),
        "params": request.get("params").cloned().unwrap_or(JsonValue::Null),
    })
}

fn without_jsonrpc_id(value: &JsonValue) -> JsonValue {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("id");
    }
    value
}

fn rewrite_jsonrpc_id(response: &mut JsonValue, id: Option<JsonValue>) {
    let Some(object) = response.as_object_mut() else {
        return;
    };
    if let Some(id) = id {
        object.insert("id".to_string(), id);
    }
}

pub fn json_digest(value: &JsonValue) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

fn extract_tool_metadata(method: &str, response: &JsonValue) -> Vec<McpRecordedToolMetadata> {
    if method != "tools/list" {
        return Vec::new();
    }
    let Some(tools) = response
        .pointer("/result/tools")
        .and_then(JsonValue::as_array)
    else {
        return Vec::new();
    };
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(JsonValue::as_str)?.to_string();
            Some(McpRecordedToolMetadata {
                name,
                output_schema_digest: tool.get("outputSchema").map(json_digest),
                annotations_digest: tool.get("annotations").map(json_digest),
            })
        })
        .collect()
}

fn extract_structured_content(method: &str, response: &JsonValue) -> Option<JsonValue> {
    (method == "tools/call")
        .then(|| response.pointer("/result/structuredContent").cloned())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cassette_recording_redacts_and_replays_with_new_ids() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "lookup",
                "arguments": { "api_key": "secret-token" }
            }
        });
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "structuredContent": { "ok": true, "token": "secret-token" }
            }
        });
        let interaction = record_interaction(0, &request, &response, 7);
        assert_eq!(
            interaction.request["params"]["arguments"]["api_key"],
            crate::redact::REDACTED_PLACEHOLDER
        );
        assert_eq!(
            interaction.response["result"]["structuredContent"]["token"],
            crate::redact::REDACTED_PLACEHOLDER
        );

        let mut replayer = McpCassetteReplayer::new(McpCassette {
            interactions: vec![interaction],
            ..McpCassette::default()
        });
        let replay_request = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "lookup",
                "arguments": { "api_key": "[redacted]" }
            }
        });
        let replayed = replayer
            .replay_request(&replay_request)
            .expect("replay succeeds")
            .expect("request has response");
        assert_eq!(replayed["id"], 99);
        assert!(replayer.is_finished());
    }

    #[test]
    fn cassette_verify_flags_schema_and_structured_content_drift() {
        let recorded = McpCassette {
            interactions: vec![record_interaction(
                0,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
                &json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": [{
                    "name": "write",
                    "inputSchema": {"type": "object"},
                    "outputSchema": {"type": "object"},
                    "annotations": {"readOnlyHint": false}
                }]}}),
                0,
            )],
            ..McpCassette::default()
        };
        let candidate = McpCassette {
            interactions: vec![record_interaction(
                0,
                &json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {}}),
                &json!({"jsonrpc": "2.0", "id": 9, "result": {"tools": [{
                    "name": "write",
                    "inputSchema": {"type": "object"},
                    "outputSchema": {"type": "string"},
                    "annotations": {"readOnlyHint": true}
                }]}}),
                0,
            )],
            ..McpCassette::default()
        };
        let report = verify_cassettes(&recorded, &candidate);
        assert!(!report.passed);
        assert!(report
            .divergences
            .iter()
            .any(|divergence| divergence.category == "tool_metadata"));
    }

    #[test]
    fn world_runtime_mutates_state_and_scores_collateral_damage() {
        let spec = McpWorldSpec {
            initial_state: json!({
                "tickets": {
                    "t1": { "status": "open", "title": "A" },
                    "t2": { "status": "open", "title": "B" }
                }
            }),
            goal_state: Some(json!({
                "tickets": {
                    "t1": { "status": "closed" }
                }
            })),
            tools: vec![McpWorldToolSpec {
                name: "close_ticket".to_string(),
                description: "Close a ticket".to_string(),
                input_schema: default_input_schema(),
                output_schema: None,
                annotations: Some(json!({"readOnlyHint": false, "destructiveHint": false})),
                operation: McpWorldOperation::Set {
                    path: "/tickets/{id}/status".to_string(),
                    value_arg: "status".to_string(),
                },
            }],
            ..McpWorldSpec::default()
        };
        let mut runtime = McpWorldRuntime::new(spec.clone());
        let response = runtime
            .handle_json_rpc(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "close_ticket",
                    "arguments": { "id": "t1", "status": "closed" }
                }
            }))
            .expect("response");
        assert_eq!(response["result"]["isError"], false);
        let score = score_world_state(&spec, runtime.state());
        assert!(score.passed);

        let damaged = json!({
            "tickets": {
                "t1": { "status": "closed", "title": "A" },
                "t2": { "status": "deleted", "title": "B" }
            }
        });
        let score = score_world_state(&spec, &damaged);
        assert!(!score.passed);
        assert_eq!(score.collateral_damage[0].path, "/tickets/t2/status");
    }

    #[test]
    fn world_fault_partial_write_mutates_then_errors() {
        let spec = McpWorldSpec {
            initial_state: json!({ "rows": { "a": { "count": 0 } } }),
            tools: vec![McpWorldToolSpec {
                name: "increment".to_string(),
                description: String::new(),
                input_schema: default_input_schema(),
                output_schema: None,
                annotations: None,
                operation: McpWorldOperation::Noop { result: json!({}) },
            }],
            faults: vec![McpWorldFaultSpec {
                tool: "increment".to_string(),
                at_call: Some(1),
                every: None,
                fault: McpWorldFault::PartialWrite {
                    path: "/rows/a/count".to_string(),
                    value: json!(1),
                    message: "write failed after commit".to_string(),
                },
            }],
            ..McpWorldSpec::default()
        };
        let mut runtime = McpWorldRuntime::new(spec);
        let response = runtime
            .handle_json_rpc(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "increment", "arguments": {} }
            }))
            .expect("response");
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(runtime.state().pointer("/rows/a/count"), Some(&json!(1)));
    }
}
