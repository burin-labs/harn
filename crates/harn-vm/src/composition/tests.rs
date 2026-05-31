use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

use super::*;
use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus};
use crate::testbench::mcp_mock::{
    McpWorldFault, McpWorldFaultSpec, McpWorldOperation, McpWorldRuntime, McpWorldSpec,
    McpWorldToolSpec,
};
use crate::tool_annotations::{SideEffectLevel, ToolAnnotations};

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
                "_mcp_tool_name": "search",
                "defer_loading": true,
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
    let mcp = manifest.find_by_name("mcp.search").expect("mcp binding");
    assert!(mcp.deferred);
    assert_eq!(mcp.metadata["_mcp_server"], "docs");
    assert_eq!(mcp.metadata["_mcp_tool_name"], "search");
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
fn manifest_compact_form_and_declarations_are_stable() {
    let tools = serde_json::json!([
        {
            "name": "docs__read_file",
            "_mcp_server": "docs",
            "_mcp_tool_name": "read_file",
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
    let manifest = binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
    let compact = manifest.to_compact_value();
    assert_eq!(compact["bindings"][0]["binding"], "docs_read_file");
    assert!(compact["bindings"][0].get("input_schema").is_none());
    let declarations = composition_typescript_declarations(&manifest);
    assert!(declarations.contains("export declare function docs_read_file"));
    assert!(declarations.contains("path: string"));
    assert!(declarations.contains("limit?: number"));
    let harn_api = composition_harn_api(&manifest);
    assert!(harn_api.contains("// MCP docs/read_file -> docs_read_file"));
    assert!(harn_api.contains("type DocsReadFileArgs = {limit?: int, path: string}"));
    assert!(harn_api.contains("fn docs_read_file(args: DocsReadFileArgs)"));
    assert!(harn_api.contains("return __composition_call(\"docs__read_file\", args)"));
    harn_parser::parse_source(&harn_api).expect("generated Harn API should parse");
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
    let manifest = binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-test".to_string(),
            snippet: "let file = read_file({path: \"README.md\"})\nreturn {text: file.text}"
                .to_string(),
            manifest,
            ..CompositionExecutionRequest::default()
        },
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
    let manifest = binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-deny".to_string(),
            snippet: "return write_file({path: \"x\", content: \"bad\"})".to_string(),
            manifest,
            ..CompositionExecutionRequest::default()
        },
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
    let manifest = binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());
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
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
    use parking_lot::Mutex;
    let tools = serde_json::json!([
        {
            "name": "read_file",
            "parameters": {"type": "object", "required": ["path"]},
            "annotations": {"kind": "read", "side_effect_level": "read_only"},
        }
    ]);
    let manifest = binding_manifest_from_tool_surface(&tools, BindingManifestOptions::default());

    struct CapturingHost {
        calls: Mutex<Vec<(String, Value)>>,
    }
    #[async_trait::async_trait]
    impl CompositionToolHost for CapturingHost {
        async fn call(
            &self,
            binding: &BindingManifestEntry,
            input: Value,
        ) -> CompositionToolOutput {
            self.calls
                .lock()
                .push((binding.name.clone(), input.clone()));
            CompositionToolOutput::ok(serde_json::json!({
                "path": input.get("path").cloned().unwrap_or(Value::Null),
                "text": "real-file-bytes",
            }))
        }
    }
    let host = Arc::new(CapturingHost {
        calls: Mutex::new(Vec::new()),
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
    assert_eq!(host.calls.lock().len(), 1);
    assert_eq!(host.calls.lock()[0].0, "read_file");
    assert_eq!(
        host.calls.lock()[0].1.get("path").and_then(Value::as_str),
        Some("README.md")
    );
    assert_eq!(
        report.run.result.as_ref().and_then(Value::as_str),
        Some("real-file-bytes")
    );
}

struct WorldCompositionHost {
    runtime: Arc<parking_lot::Mutex<McpWorldRuntime>>,
    attempts: Arc<AtomicU64>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl WorldCompositionHost {
    fn new(spec: McpWorldSpec) -> Self {
        Self {
            runtime: Arc::new(parking_lot::Mutex::new(McpWorldRuntime::new(spec))),
            attempts: Arc::new(AtomicU64::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn update_peak(&self, active: usize) {
        let mut observed = self.peak.load(Ordering::SeqCst);
        while active > observed {
            match self
                .peak
                .compare_exchange(observed, active, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(next) => observed = next,
            }
        }
    }
}

#[async_trait::async_trait]
impl CompositionToolHost for WorldCompositionHost {
    async fn call(&self, binding: &BindingManifestEntry, input: Value) -> CompositionToolOutput {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.update_peak(active);
        tokio::task::yield_now().await;
        let tool_name = binding
            .metadata
            .get("_mcp_tool_name")
            .or_else(|| binding.metadata.get("mcp_tool_name"))
            .and_then(Value::as_str)
            .unwrap_or(&binding.name);
        let response = self
            .runtime
            .lock()
            .handle_json_rpc(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": input,
                },
            }))
            .unwrap_or_else(|| {
                serde_json::json!({
                    "error": {"message": "simulated MCP world returned no response"}
                })
            });
        self.active.fetch_sub(1, Ordering::SeqCst);
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP server error");
            return CompositionToolOutput::error(
                format!("MCP server error: {message}; data={}", error["data"]),
                ToolCallErrorCategory::McpServerError,
            );
        }
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let message = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("MCP tool error");
            return CompositionToolOutput::error(message, ToolCallErrorCategory::ToolError);
        }
        CompositionToolOutput::ok(
            result
                .get("structuredContent")
                .cloned()
                .unwrap_or(Value::Null),
        )
    }
}

fn mcp_world_manifest(tool: &McpWorldToolSpec) -> BindingManifest {
    let mut entry = serde_json::json!({
        "name": format!("docs__{}", tool.name),
        "description": tool.description.clone(),
        "inputSchema": tool.input_schema.clone(),
        "executor": "mcp_server",
        "_mcp_server": "docs",
        "_mcp_tool_name": tool.name.clone(),
        "defer_loading": true,
    });
    if let Some(output_schema) = &tool.output_schema {
        entry["outputSchema"] = output_schema.clone();
    }
    if let Some(annotations) = &tool.annotations {
        entry["annotations"] = annotations.clone();
    }
    binding_manifest_from_tool_surface(
        &Value::Array(vec![entry]),
        BindingManifestOptions::default(),
    )
}

fn retry_test_policy() -> CompositionMcpPolicy {
    CompositionMcpPolicy {
        trusted_servers: BTreeSet::from(["docs".to_string()]),
        retry: CompositionRetryPolicy {
            max_attempts: 3,
            base_delay_ms: 0,
            max_delay_ms: 0,
            honor_retry_after: true,
        },
        ..CompositionMcpPolicy::default()
    }
}

fn retry_gate_binding() -> BindingManifestEntry {
    BindingManifestEntry {
        name: "docs__search".to_string(),
        binding: "docs_search".to_string(),
        annotations: ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            ..ToolAnnotations::default()
        },
        side_effect_level: SideEffectLevel::ReadOnly,
        source: "mcp_server".to_string(),
        metadata: serde_json::json!({"_mcp_server": "docs"}),
        ..BindingManifestEntry::default()
    }
}

#[test]
fn retry_gate_trusts_mcp_annotations_only_for_trusted_servers() {
    let binding = retry_gate_binding();
    assert!(!retry_allowed(
        &binding,
        &serde_json::json!({}),
        &CompositionMcpPolicy::default()
    ));
    assert!(retry_allowed(
        &binding,
        &serde_json::json!({}),
        &retry_test_policy()
    ));

    let destructive = BindingManifestEntry {
        annotations: ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            ..ToolAnnotations::default()
        },
        ..binding
    };
    assert!(!retry_allowed(
        &destructive,
        &serde_json::json!({}),
        &retry_test_policy()
    ));
    assert!(retry_allowed(
        &destructive,
        &serde_json::json!({"idempotency_key": "row-1"}),
        &retry_test_policy()
    ));
}

#[test]
fn retry_delay_honors_retry_after_caps_and_deterministic_jitter() {
    let binding = retry_gate_binding();
    let policy = CompositionRetryPolicy {
        max_attempts: 4,
        base_delay_ms: 100,
        max_delay_ms: 250,
        honor_retry_after: true,
    };
    assert_eq!(
        compute_retry_delay_ms(
            &binding,
            &serde_json::json!({"query": "runtime"}),
            1,
            &policy,
            "HTTP 503\nRetry-After: 7",
        ),
        250
    );

    let jitter_policy = CompositionRetryPolicy {
        max_delay_ms: 1_000,
        ..policy
    };
    let input = serde_json::json!({"query": "runtime"});
    let first = compute_retry_delay_ms(&binding, &input, 2, &jitter_policy, "HTTP 503");
    let second = compute_retry_delay_ms(&binding, &input, 2, &jitter_policy, "HTTP 503");
    assert_eq!(first, second);
    assert!((200..=300).contains(&first), "delay was {first}");
}

#[tokio::test(flavor = "current_thread")]
async fn harn_composition_retries_trusted_read_only_mcp_faults() {
    let tool = McpWorldToolSpec {
        name: "search".to_string(),
        description: "Search docs".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
        }),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "required": ["ok"],
            "properties": {"ok": {"type": "boolean"}},
        })),
        annotations: Some(serde_json::json!({
            "readOnlyHint": true,
            "destructiveHint": false,
        })),
        operation: McpWorldOperation::Noop {
            result: serde_json::json!({"ok": true}),
        },
    };
    let host = Arc::new(WorldCompositionHost::new(McpWorldSpec {
        tools: vec![tool.clone()],
        faults: vec![McpWorldFaultSpec {
            tool: "search".to_string(),
            at_call: Some(1),
            every: None,
            fault: McpWorldFault::JsonRpcError {
                code: -32000,
                message: "HTTP 503 Service Unavailable\nRetry-After: 0".to_string(),
                data: Some(serde_json::json!({"status": 503})),
            },
        }],
        ..McpWorldSpec::default()
    }));
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-retry".to_string(),
            snippet: "return docs_search({query: \"runtime\"})".to_string(),
            manifest: mcp_world_manifest(&tool),
            mcp_policy: retry_test_policy(),
            ..CompositionExecutionRequest::default()
        },
        host.clone(),
    )
    .await;
    assert!(report.ok, "{}", report.summary);
    assert_eq!(host.attempts(), 2);
    assert_eq!(report.child_results[0].retry_attempts, 1);
    assert_eq!(report.child_results[0].retry_delays_ms, vec![0]);
    assert_eq!(report.run.result.unwrap()["ok"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn harn_composition_does_not_retry_untrusted_mcp_annotations() {
    let tool = McpWorldToolSpec {
        name: "search".to_string(),
        description: "Search docs".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        annotations: Some(serde_json::json!({"readOnlyHint": true})),
        operation: McpWorldOperation::Noop {
            result: serde_json::json!({"ok": true}),
        },
    };
    let host = Arc::new(WorldCompositionHost::new(McpWorldSpec {
        tools: vec![tool.clone()],
        faults: vec![McpWorldFaultSpec {
            tool: "search".to_string(),
            at_call: Some(1),
            every: None,
            fault: McpWorldFault::JsonRpcError {
                code: -32000,
                message: "HTTP 503 Service Unavailable".to_string(),
                data: None,
            },
        }],
        ..McpWorldSpec::default()
    }));
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-untrusted".to_string(),
            snippet: "return docs_search({query: \"runtime\"})".to_string(),
            manifest: mcp_world_manifest(&tool),
            ..CompositionExecutionRequest::default()
        },
        host.clone(),
    )
    .await;
    assert!(!report.ok);
    assert_eq!(host.attempts(), 1);
    assert_eq!(report.child_results[0].retry_attempts, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn map_bounded_respects_per_server_bulkhead() {
    let tool = McpWorldToolSpec {
        name: "search".to_string(),
        description: "Search docs".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        annotations: Some(serde_json::json!({"readOnlyHint": true})),
        operation: McpWorldOperation::Noop {
            result: serde_json::json!({"ok": true}),
        },
    };
    let host = Arc::new(WorldCompositionHost::new(McpWorldSpec {
        tools: vec![tool.clone()],
        ..McpWorldSpec::default()
    }));
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-map-bounded".to_string(),
            snippet: "let settled = map_bounded([1, 2, 3, 4, 5], { item -> docs_search({query: to_string(item)}) }, {concurrency: 5})\nreturn {succeeded: settled.succeeded, failed: settled.failed}".to_string(),
            manifest: mcp_world_manifest(&tool),
            limits: CompositionExecutionLimits {
                max_concurrent_operations: 8,
                max_concurrent_per_server: 2,
                ..CompositionExecutionLimits::default()
            },
            mcp_policy: retry_test_policy(),
            ..CompositionExecutionRequest::default()
        },
        host.clone(),
    )
    .await;
    assert!(report.ok, "{}", report.summary);
    assert_eq!(host.attempts(), 5);
    assert!(host.peak() <= 2, "peak was {}", host.peak());
    let result = report.run.result.unwrap();
    assert_eq!(result["succeeded"], 5);
    assert_eq!(result["failed"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn harn_composition_rejects_invalid_mcp_structured_content() {
    let tool = McpWorldToolSpec {
        name: "count".to_string(),
        description: "Count rows".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "required": ["count"],
            "properties": {"count": {"type": "integer"}},
        })),
        annotations: Some(serde_json::json!({"readOnlyHint": true})),
        operation: McpWorldOperation::Noop {
            result: serde_json::json!({"count": "bad"}),
        },
    };
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-schema".to_string(),
            snippet: "return docs_count({})".to_string(),
            manifest: mcp_world_manifest(&tool),
            mcp_policy: retry_test_policy(),
            ..CompositionExecutionRequest::default()
        },
        Arc::new(WorldCompositionHost::new(McpWorldSpec {
            tools: vec![tool.clone()],
            ..McpWorldSpec::default()
        })),
    )
    .await;
    assert!(!report.ok);
    assert_eq!(
        report.child_results[0].error_category,
        Some(ToolCallErrorCategory::SchemaValidation)
    );
    assert!(report.child_results[0]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("outputSchema"));
}

#[tokio::test(flavor = "current_thread")]
async fn harn_composition_retries_destructive_mcp_only_with_idempotency_key() {
    let tool = McpWorldToolSpec {
        name: "append_row".to_string(),
        description: "Append a row".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: Some(serde_json::json!({"type": "object"})),
        annotations: Some(serde_json::json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
        })),
        operation: McpWorldOperation::Append {
            path: "/rows".to_string(),
            value_arg: "row".to_string(),
            id_field: None,
            id_prefix: None,
        },
    };
    let host = Arc::new(WorldCompositionHost::new(McpWorldSpec {
        initial_state: serde_json::json!({"rows": []}),
        tools: vec![tool.clone()],
        faults: vec![McpWorldFaultSpec {
            tool: "append_row".to_string(),
            at_call: Some(1),
            every: None,
            fault: McpWorldFault::PartialWrite {
                path: "/rows".to_string(),
                value: serde_json::json!([{"id": "r1"}]),
                message: "HTTP 503 Service Unavailable\nRetry-After: 0".to_string(),
            },
        }],
        ..McpWorldSpec::default()
    }));
    let report = execute_harn_composition(
        CompositionExecutionRequest {
            run_id: "run-idempotency".to_string(),
            snippet: "return docs_append_row({row: {id: \"r1\"}, idempotency_key: \"row-r1\"})"
                .to_string(),
            manifest: mcp_world_manifest(&tool),
            mcp_policy: retry_test_policy(),
            ..CompositionExecutionRequest::default()
        },
        host.clone(),
    )
    .await;
    assert!(report.ok, "{}", report.summary);
    assert_eq!(host.attempts(), 2);
    assert_eq!(report.child_results[0].retry_attempts, 1);
    let state = host.runtime.lock().state().clone();
    assert_eq!(state["rows"], serde_json::json!([{"id": "r1"}]));
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
        Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
