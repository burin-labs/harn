use super::*;
use crate::orchestration::{WorkflowEdge, WorkflowNode};

fn fixture_bundle() -> WorkflowBundle {
    serde_json::from_str(
        r#"{
  "schema_version": 1,
  "id": "github-pr-monitor",
  "name": "GitHub PR monitor",
  "version": "1.0.0",
  "triggers": [
    {
      "id": "github-pr-updated",
      "kind": "github",
      "provider": "github",
      "events": ["pull_request.opened", "pull_request.synchronize"],
      "node_id": "ingest"
    },
    {
      "id": "delay-log-check",
      "kind": "delay",
      "delay": "PT10M",
      "node_id": "query_logs"
    }
  ],
  "workflow": {
    "_type": "workflow_graph",
    "id": "pr_monitor_workflow",
    "name": "PR monitor",
    "version": 1,
    "entry": "ingest",
    "nodes": {
      "ingest": {
        "id": "ingest",
        "kind": "action",
        "task_label": "Normalize PR event"
      },
      "wait_for_deploy": {
        "id": "wait_for_deploy",
        "kind": "waitpoint",
        "task_label": "Wait for deploy"
      },
      "query_logs": {
        "id": "query_logs",
        "kind": "action",
        "task_label": "Query logs"
      },
      "notify": {
        "id": "notify",
        "kind": "notification",
        "task_label": "Notify user"
      }
    },
    "edges": [
      {
        "from": "ingest",
        "to": "wait_for_deploy"
      },
      {
        "from": "wait_for_deploy",
        "to": "query_logs"
      },
      {
        "from": "query_logs",
        "to": "notify"
      }
    ]
  },
  "prompt_capsules": {
    "query-logs": {
      "id": "query-logs",
      "node_id": "query_logs",
      "trigger_id": "delay-log-check",
      "prompt": "Query deploy logs for the pull request and summarize failures."
    }
  },
  "policy": {
    "autonomy_tier": "act_with_approval",
    "retry": {
      "max_attempts": 2,
      "backoff": "exponential"
    },
    "catchup": {
      "mode": "latest",
      "max_events": 1
    }
  },
  "connectors": [
    {
      "id": "github",
      "provider_id": "github",
      "scopes": ["pull_requests:read", "checks:read"],
      "setup_required": true,
      "status_required": true
    }
  ],
  "environment": {
    "repo_setup_profile": "default",
    "worktree_policy": "host_managed",
    "command_gates": ["make test"]
  },
  "receipts": {
    "run_id": "bundle_run_pr_monitor_fixture",
    "event_ids": ["github:event:42"],
    "workflow_version": 1
  }
}"#,
    )
    .unwrap()
}

fn graph_node_types(preview: &WorkflowBundlePreview) -> Vec<String> {
    let mut types = preview
        .graph
        .nodes
        .iter()
        .map(|node| node.node_type.clone())
        .collect::<Vec<_>>();
    types.sort();
    types.dedup();
    types
}

#[test]
fn validates_fixture_bundle_and_previews_graph() {
    let bundle = fixture_bundle();
    let report = validate_workflow_bundle(&bundle);
    assert!(report.valid, "{report:#?}");
    assert!(report.graph_digest.starts_with("sha256:"));

    let preview = preview_workflow_bundle(&bundle);
    assert_eq!(preview.nodes.len(), 4);
    assert_eq!(preview.nodes[0].id, "ingest");
    assert_eq!(
        preview.nodes[2].prompt_capsule.as_deref(),
        Some("query-logs")
    );
    assert!(preview.mermaid.starts_with("flowchart TD"));
    assert_eq!(preview.mermaid, preview.graph.mermaid);
    let node_types = graph_node_types(&preview);
    for expected in [
        "action",
        "catchup",
        "connector_call",
        "dlq",
        "notification",
        "terminal",
        "trigger",
        "wait",
    ] {
        assert!(
            node_types.contains(&expected.to_string()),
            "{node_types:#?}"
        );
    }
    assert!(preview
        .graph
        .edges
        .iter()
        .any(|edge| { edge.from == "trigger/github-pr-updated" && edge.to == "policy/catchup" }));
    assert!(preview.graph.editable_fields.iter().any(|field| {
        field.id == "prompt_capsule.query-logs.prompt"
            && field.json_pointer == "/prompt_capsules/query-logs/prompt"
    }));
}

#[test]
fn rejects_unstable_or_unknown_bundle_references() {
    let mut bundle = fixture_bundle();
    bundle
        .prompt_capsules
        .get_mut("query-logs")
        .unwrap()
        .node_id = "missing".to_string();
    bundle.policy.catchup.mode = "surprise".to_string();
    bundle.workflow.id.clear();

    let report = validate_workflow_bundle(&bundle);
    assert!(!report.valid);
    assert!(report
        .errors
        .iter()
        .any(|diagnostic| diagnostic.path == "workflow.id"));
    assert!(report
        .errors
        .iter()
        .any(|diagnostic| diagnostic.path == "prompt_capsules.query-logs.node_id"));
    assert!(report
        .errors
        .iter()
        .any(|diagnostic| diagnostic.path == "policy.catchup.mode"));
}

#[test]
fn graph_export_covers_agent_approval_connector_and_terminal_nodes() {
    let mut bundle = fixture_bundle();
    bundle.workflow.nodes.insert(
        "repair".to_string(),
        WorkflowNode {
            id: Some("repair".to_string()),
            kind: "agent".to_string(),
            task_label: Some("Repair PR".to_string()),
            ..WorkflowNode::default()
        },
    );
    bundle.workflow.nodes.insert(
        "delegate".to_string(),
        WorkflowNode {
            id: Some("delegate".to_string()),
            kind: "subagent".to_string(),
            task_label: Some("Delegate log review".to_string()),
            ..WorkflowNode::default()
        },
    );
    bundle.workflow.nodes.insert(
        "approve".to_string(),
        WorkflowNode {
            id: Some("approve".to_string()),
            kind: "approval".to_string(),
            task_label: Some("Approve deploy action".to_string()),
            ..WorkflowNode::default()
        },
    );
    bundle.workflow.nodes.insert(
        "call_render".to_string(),
        WorkflowNode {
            id: Some("call_render".to_string()),
            kind: "connector_call".to_string(),
            task_label: Some("Query deployment provider".to_string()),
            ..WorkflowNode::default()
        },
    );
    bundle.workflow.edges.extend([
        WorkflowEdge {
            from: "notify".to_string(),
            to: "repair".to_string(),
            branch: Some("failed".to_string()),
            label: None,
        },
        WorkflowEdge {
            from: "repair".to_string(),
            to: "delegate".to_string(),
            branch: None,
            label: None,
        },
        WorkflowEdge {
            from: "delegate".to_string(),
            to: "approve".to_string(),
            branch: None,
            label: None,
        },
        WorkflowEdge {
            from: "approve".to_string(),
            to: "call_render".to_string(),
            branch: None,
            label: None,
        },
    ]);

    let preview = preview_workflow_bundle(&bundle);
    assert!(preview.validation.valid, "{:#?}", preview.validation);
    let node_types = graph_node_types(&preview);
    for expected in [
        "agent",
        "subagent",
        "approval",
        "connector_call",
        "terminal",
    ] {
        assert!(
            node_types.contains(&expected.to_string()),
            "{node_types:#?}"
        );
    }
}

#[test]
fn graph_diagnostics_include_node_ids_for_gui_annotation() {
    let mut bundle = fixture_bundle();
    bundle.workflow.nodes.insert(
        "orphan".to_string(),
        WorkflowNode {
            id: Some("orphan".to_string()),
            kind: "action".to_string(),
            task_label: Some("Unreachable node".to_string()),
            ..WorkflowNode::default()
        },
    );
    bundle.workflow.edges.push(WorkflowEdge {
        from: "missing".to_string(),
        to: "notify".to_string(),
        branch: None,
        label: None,
    });

    let preview = preview_workflow_bundle(&bundle);
    assert!(!preview.validation.valid);
    assert!(
        preview.graph.diagnostics.iter().any(|diagnostic| {
            diagnostic.node_id.as_deref() == Some("orphan")
                && diagnostic.graph_node_id.as_deref() == Some("node/orphan")
        }),
        "{:#?}",
        preview.graph.diagnostics
    );
    assert!(
        preview.graph.diagnostics.iter().any(|diagnostic| {
            diagnostic.node_id.as_deref() == Some("missing")
                && diagnostic.graph_node_id.as_deref() == Some("node/missing")
        }),
        "{:#?}",
        preview.graph.diagnostics
    );
}

#[test]
fn editable_field_pointers_round_trip_supported_surfaces() {
    let bundle = fixture_bundle();
    let preview = preview_workflow_bundle(&bundle);
    let mut value = serde_json::to_value(&bundle).unwrap();
    let edits = [
        (
            "trigger.github-pr-updated.events",
            serde_json::json!(["pull_request.closed"]),
        ),
        (
            "prompt_capsule.query-logs.prompt",
            serde_json::json!("Check deployment logs and report only actionable failures."),
        ),
        (
            "workflow.query_logs.model_policy",
            serde_json::json!({"provider": "mock", "model": "cheap-local"}),
        ),
        (
            "workflow.query_logs.tools",
            serde_json::json!([{"name": "render.logs.query"}]),
        ),
        ("policy.retry.max_attempts", serde_json::json!(3)),
        ("policy.catchup.mode", serde_json::json!("all")),
        (
            "connector.github.provider_id",
            serde_json::json!("github-enterprise"),
        ),
    ];

    for (field_id, replacement) in edits {
        let pointer = preview
            .graph
            .editable_fields
            .iter()
            .find(|field| field.id == field_id)
            .unwrap_or_else(|| panic!("missing editable field {field_id}"))
            .json_pointer
            .clone();
        *value
            .pointer_mut(&pointer)
            .unwrap_or_else(|| panic!("missing JSON pointer {pointer}")) = replacement;
    }

    let edited: WorkflowBundle = serde_json::from_value(value).unwrap();
    assert_eq!(
        edited.triggers[0].events,
        vec!["pull_request.closed".to_string()]
    );
    assert_eq!(edited.policy.retry.max_attempts, 3);
    assert_eq!(edited.policy.catchup.mode, "all");
    assert_eq!(edited.connectors[0].provider_id, "github-enterprise");
    assert_eq!(
        edited.workflow.nodes["query_logs"]
            .model_policy
            .provider
            .as_deref(),
        Some("mock")
    );
    assert_eq!(
        edited.prompt_capsules["query-logs"].prompt,
        "Check deployment logs and report only actionable failures."
    );
}

#[test]
fn run_rejects_unknown_trigger_id() {
    let bundle = fixture_bundle();
    let report = run_workflow_bundle(
        &bundle,
        WorkflowBundleRunRequest {
            trigger_id: Some("missing-trigger".to_string()),
            event_id: None,
        },
    )
    .unwrap_err();
    assert!(report
        .errors
        .iter()
        .any(|diagnostic| diagnostic.path == "trigger_id"));
}

#[test]
fn graph_digest_and_run_receipt_are_deterministic() {
    let bundle = fixture_bundle();
    let left = run_workflow_bundle(
        &bundle,
        WorkflowBundleRunRequest {
            trigger_id: Some("github-pr-updated".to_string()),
            event_id: Some("github:event:43".to_string()),
        },
    )
    .unwrap();
    let right = run_workflow_bundle(
        &bundle,
        WorkflowBundleRunRequest {
            trigger_id: Some("github-pr-updated".to_string()),
            event_id: Some("github:event:43".to_string()),
        },
    )
    .unwrap();
    assert_eq!(left, right);
    assert_eq!(left.executed_nodes.len(), 4);
    assert_eq!(left.event_ids, vec!["github:event:42", "github:event:43"]);
}
