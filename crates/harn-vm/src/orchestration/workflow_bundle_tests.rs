use super::*;

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
