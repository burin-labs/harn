use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::*;
use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus};
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
