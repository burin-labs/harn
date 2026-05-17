use super::*;
use crate::orchestration::{WorkflowEdge, WorkflowNode};

fn fixture_bundle() -> WorkflowBundle {
    super::super::workflow_test_fixtures::pr_monitor_bundle()
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
fn v2_manifest_deserializes_and_v1_is_typed_error() {
    let bundle = parse_workflow_bundle_manifest(
        super::super::workflow_test_fixtures::PR_MONITOR_BUNDLE_JSON.as_bytes(),
    )
    .unwrap();
    assert_eq!(bundle.schema_version, WORKFLOW_BUNDLE_SCHEMA_VERSION);
    assert_eq!(
        bundle.entrypoint,
        std::path::PathBuf::from("workflows/github-pr-monitor.harn")
    );
    assert_eq!(bundle.transitive_modules.len(), 1);
    assert!(current_provider_catalog_hash_blake3()
        .unwrap()
        .starts_with("blake3:"));

    let err = parse_workflow_bundle_manifest(br#"{"schema_version":1}"#).unwrap_err();
    assert_eq!(
        err.kind,
        WorkflowBundleErrorKind::UnsupportedSchemaVersion {
            actual: 1,
            expected: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        }
    );
}

#[test]
fn harnpack_build_read_and_bundle_hash_are_deterministic() {
    let bundle = fixture_bundle();
    let contents = vec![
        HarnpackEntry::new("bytecode/pr_monitor.harnbc", b"bytecode-v1".to_vec()),
        HarnpackEntry::new("sources/pr_monitor.harn", b"source-v1".to_vec()),
    ];
    let reversed_contents = vec![contents[1].clone(), contents[0].clone()];

    let left_pack = build_harnpack(&bundle, &contents).unwrap();
    let right_pack = build_harnpack(&bundle, &reversed_contents).unwrap();
    assert_eq!(left_pack, right_pack);

    let left_hash = workflow_bundle_hash(&bundle, &contents).unwrap();
    let right_hash = workflow_bundle_hash(&bundle, &reversed_contents).unwrap();
    assert_eq!(left_hash, right_hash);

    let changed_hash = workflow_bundle_hash(
        &bundle,
        &[HarnpackEntry::new(
            "sources/pr_monitor.harn",
            b"source-v2".to_vec(),
        )],
    )
    .unwrap();
    assert_ne!(left_hash, changed_hash);

    let archive = read_harnpack(&left_pack).unwrap();
    assert_eq!(archive.manifest.id, bundle.id);
    assert_eq!(
        archive
            .contents
            .iter()
            .map(|entry| entry.path.display().to_string())
            .collect::<Vec<_>>(),
        vec!["bytecode/pr_monitor.harnbc", "sources/pr_monitor.harn"]
    );

    let temp = tempfile::tempdir().unwrap();
    let pack_path = temp.path().join("fixture.harnpack");
    std::fs::write(&pack_path, left_pack).unwrap();
    let loaded = load_workflow_bundle(&pack_path).unwrap();
    assert_eq!(loaded.id, bundle.id);
}

#[test]
fn validator_rejects_missing_v2_manifest_contract_fields() {
    let mut bundle = fixture_bundle();
    bundle.entrypoint = std::path::PathBuf::new();
    bundle.transitive_modules.clear();
    bundle.provider_catalog_hash = "sha256:not-blake3".to_string();
    bundle.sbom.format.clear();

    let report = validate_workflow_bundle(&bundle);
    assert!(!report.valid);
    for path in [
        "entrypoint",
        "transitive_modules",
        "provider_catalog_hash",
        "sbom.format",
    ] {
        assert!(
            report
                .errors
                .iter()
                .any(|diagnostic| diagnostic.path == path),
            "{report:#?}"
        );
    }
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
