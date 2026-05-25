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
fn read_manifest_any_version_accepts_v1_payload() {
    // A minimal v1 manifest: only the pre-v2 fields. The relaxed parser
    // must accept it and default the v2-only fields, so `harn pack
    // --upgrade <old>` can keep the original workflow/triggers and
    // bolt on the new metadata.
    let v1 = r#"{
      "schema_version": 1,
      "id": "legacy",
      "version": "1.0.0",
      "workflow": {
        "_type": "workflow_graph",
        "id": "wf",
        "version": 1,
        "entry": "step",
        "nodes": { "step": { "id": "step", "kind": "action" } }
      },
      "triggers": [{ "id": "manual", "kind": "manual", "node_id": "step" }]
    }"#;
    let bundle = read_workflow_bundle_manifest_any_version(v1.as_bytes()).unwrap();
    assert_eq!(bundle.schema_version, 1);
    assert_eq!(bundle.id, "legacy");
    assert_eq!(bundle.workflow.id, "wf");
    assert_eq!(bundle.triggers.len(), 1);
    assert!(bundle.transitive_modules.is_empty());
    assert!(bundle.entrypoint.as_os_str().is_empty());

    // The strict parser still rejects v1 so callers that aren't
    // explicitly migrating cannot accidentally interpret one.
    let strict = parse_workflow_bundle_manifest(v1.as_bytes()).unwrap_err();
    assert_eq!(
        strict.kind,
        WorkflowBundleErrorKind::UnsupportedSchemaVersion {
            actual: 1,
            expected: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        }
    );
}

#[test]
fn load_any_version_reads_v1_harnpack_archive() {
    // Round-trip: assemble a v2 harnpack, hand-roll a manifest
    // override to schema_version=1, repack, and confirm
    // load_workflow_bundle_any_version returns it.
    let bundle = fixture_bundle();
    let contents = vec![HarnpackEntry::new(
        "sources/legacy.harn",
        b"// placeholder".to_vec(),
    )];
    let temp = tempfile::tempdir().unwrap();
    let pack_path = temp.path().join("legacy.harnpack");
    // Build with the current schema first to reuse the canonicalizer.
    let archive = build_harnpack(&bundle, &contents).unwrap();
    // Patch the manifest's schema_version to 1 by rewriting the
    // archive's manifest entry, then write it to disk.
    let opened = read_harnpack(&archive).unwrap();
    let mut downgraded = opened.manifest;
    downgraded.schema_version = 1;
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let manifest_bytes = serde_json::to_vec(&downgraded).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_path(HARNPACK_MANIFEST_PATH).unwrap();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(manifest_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder.append(&header, manifest_bytes.as_slice()).unwrap();
        for entry in &opened.contents {
            let mut header = tar::Header::new_gnu();
            header
                .set_path(entry.path.to_string_lossy().to_string())
                .unwrap();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(entry.bytes.len() as u64);
            header.set_mode(entry.mode);
            header.set_mtime(0);
            header.set_cksum();
            builder.append(&header, entry.bytes.as_slice()).unwrap();
        }
        builder.finish().unwrap();
    }
    let zstd_bytes = zstd::stream::encode_all(std::io::Cursor::new(tar_bytes), 0).unwrap();
    std::fs::write(&pack_path, &zstd_bytes).unwrap();

    // The strict loader rejects the downgraded archive…
    let strict = load_workflow_bundle(&pack_path).unwrap_err();
    assert_eq!(
        strict.kind,
        WorkflowBundleErrorKind::UnsupportedSchemaVersion {
            actual: 1,
            expected: WORKFLOW_BUNDLE_SCHEMA_VERSION,
        }
    );
    // …the relaxed loader accepts it.
    let loaded = load_workflow_bundle_any_version(&pack_path).unwrap();
    assert_eq!(loaded.schema_version, 1);
    assert_eq!(loaded.id, bundle.id);
    assert_eq!(loaded.workflow.id, bundle.workflow.id);
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
