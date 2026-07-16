use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::orchestration::CapabilityPolicy;

fn fixture_bundle() -> WorkflowBundle {
    super::super::workflow_test_fixtures::pr_monitor_bundle()
}

fn parent_ceiling_act_with_approval() -> CapabilityPolicy {
    let mut capabilities = BTreeMap::new();
    capabilities.insert("workspace".to_string(), vec!["read_text".to_string()]);
    capabilities.insert("connector".to_string(), vec!["call".to_string()]);
    capabilities.insert("process".to_string(), vec!["exec".to_string()]);
    CapabilityPolicy {
        tools: vec!["read_file".to_string(), "github_search".to_string()],
        tools_restricted: false,
        capabilities,
        capabilities_restricted: false,
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        side_effect_level: Some("process_exec".to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
        process_sandbox: Default::default(),
    }
}

fn parent_ceiling_read_only() -> CapabilityPolicy {
    let mut capabilities = BTreeMap::new();
    capabilities.insert("workspace".to_string(), vec!["read_text".to_string()]);
    CapabilityPolicy {
        tools: vec!["read_file".to_string()],
        tools_restricted: false,
        capabilities,
        capabilities_restricted: false,
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: BTreeMap::new(),
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
        process_sandbox: Default::default(),
    }
}

fn pr_verifier_patch() -> WorkflowPatch {
    serde_json::from_value(json!({
        "schema_version": 1,
        "id": "patch-pr-verifier-001",
        "summary": "Insert deterministic verifier between query_logs and notify",
        "operations": [
            {
                "op": "insert_node",
                "node_id": "verify_logs",
                "node": {
                    "kind": "action",
                    "task_label": "Verify summary against deploy logs"
                }
            },
            {
                "op": "insert_node",
                "node_id": "repair_logs",
                "node": {
                    "kind": "agent",
                    "task_label": "Repair summary"
                }
            },
            { "op": "add_edge", "from": "query_logs", "to": "verify_logs" },
            { "op": "add_edge", "from": "verify_logs", "to": "notify", "branch": "verify_pass" },
            { "op": "add_edge", "from": "verify_logs", "to": "repair_logs", "branch": "verify_fail" },
            { "op": "add_edge", "from": "repair_logs", "to": "verify_logs" },
            {
                "op": "upsert_prompt_capsule",
                "capsule_id": "verify-logs",
                "capsule": {
                    "node_id": "verify_logs",
                    "prompt": "Confirm the previous summary cites at least one file:line."
                }
            }
        ]
    }))
    .expect("patch parses")
}

#[test]
fn pr_monitor_verifier_patch_applies_validates_and_diffs() {
    let bundle = fixture_bundle();
    let patch = pr_verifier_patch();
    let report =
        validate_workflow_patch(&bundle, &patch, Some(&parent_ceiling_act_with_approval()));
    assert!(report.valid, "report should be valid: {report:#?}");
    assert!(report.apply_errors.is_empty());
    assert!(report.bundle_validation.valid);
    assert_eq!(
        report.graph_diff.added_nodes,
        vec!["repair_logs".to_string(), "verify_logs".to_string()]
    );
    assert!(report
        .graph_diff
        .added_edges
        .iter()
        .any(|edge| edge.from == "verify_logs"
            && edge.to == "notify"
            && edge.branch.as_deref() == Some("verify_pass")));
    assert!(report
        .graph_diff
        .added_edges
        .iter()
        .any(|edge| edge.from == "verify_logs"
            && edge.to == "repair_logs"
            && edge.branch.as_deref() == Some("verify_fail")));
    assert!(report
        .graph_diff
        .updated_capsules
        .contains(&"verify-logs".to_string()));
    assert!(report.capability_delta.widening.is_empty());
}

#[test]
fn empty_operations_are_rejected() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-noop".to_string(),
        summary: None,
        operations: Vec::new(),
    };
    let result = apply_workflow_patch(&bundle, &patch);
    let errors = result.expect_err("empty patch is rejected");
    assert!(errors
        .iter()
        .any(|err| err.message.contains("no operations")));
}

#[test]
fn unknown_node_id_in_add_edge_fails() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-bad-edge".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::AddEdge {
            from: "ingest".to_string(),
            to: "does_not_exist".to_string(),
            branch: None,
            label: None,
        }],
    };
    let report = validate_workflow_patch(&bundle, &patch, None);
    assert!(!report.valid);
    assert!(report
        .apply_errors
        .iter()
        .any(|err| err.message.contains("does_not_exist")));
}

#[test]
fn duplicate_insert_node_is_rejected() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-dup".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::InsertNode {
            node_id: "ingest".to_string(),
            node: WorkflowPatchNodeBody::default(),
        }],
    };
    let report = validate_workflow_patch(&bundle, &patch, None);
    assert!(!report.valid);
    assert!(report
        .apply_errors
        .iter()
        .any(|err| err.message.contains("already contains node ingest")));
}

#[test]
fn duplicate_edge_is_rejected() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-dup-edge".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::AddEdge {
            from: "ingest".to_string(),
            to: "wait_for_deploy".to_string(),
            branch: None,
            label: None,
        }],
    };
    let report = validate_workflow_patch(&bundle, &patch, None);
    assert!(!report.valid);
    assert!(report
        .apply_errors
        .iter()
        .any(|err| err.message.contains("already exists")));
}

#[test]
fn capability_widening_via_node_policy_is_rejected() {
    let bundle = fixture_bundle();
    let mut node_capabilities = BTreeMap::new();
    node_capabilities.insert("process".to_string(), vec!["exec".to_string()]);
    let node_policy = CapabilityPolicy {
        tools: vec!["dangerous_exec".to_string()],
        capabilities: node_capabilities,
        side_effect_level: Some("process_exec".to_string()),
        ..CapabilityPolicy::default()
    };

    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-widen".to_string(),
        summary: None,
        operations: vec![
            WorkflowPatchOperation::InsertNode {
                node_id: "exec_node".to_string(),
                node: WorkflowPatchNodeBody {
                    kind: Some("action".to_string()),
                    task_label: Some("Run exec".to_string()),
                    capability_policy: Some(node_policy),
                    ..WorkflowPatchNodeBody::default()
                },
            },
            WorkflowPatchOperation::AddEdge {
                from: "ingest".to_string(),
                to: "exec_node".to_string(),
                branch: None,
                label: None,
            },
        ],
    };
    let report = validate_workflow_patch(&bundle, &patch, Some(&parent_ceiling_read_only()));
    assert!(!report.valid, "widened patch should fail: {report:#?}");
    assert!(!report.capability_delta.widening.is_empty());
    let kinds: Vec<&str> = report
        .capability_delta
        .widening
        .iter()
        .map(|v| v.kind.as_str())
        .collect();
    assert!(kinds.contains(&"tool"));
    assert!(kinds.contains(&"side_effect_level"));
}

#[test]
fn raising_autonomy_tier_is_a_widening_violation() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-raise-autonomy".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::UpdateBundlePolicy {
            policy: WorkflowPatchBundlePolicyBody {
                autonomy_tier: Some("act_auto".to_string()),
                ..WorkflowPatchBundlePolicyBody::default()
            },
        }],
    };
    let report =
        validate_workflow_patch(&bundle, &patch, Some(&parent_ceiling_act_with_approval()));
    assert!(!report.valid);
    assert!(report
        .capability_delta
        .widening
        .iter()
        .any(|violation| violation.kind == "autonomy_tier"));
}

#[test]
fn upsert_prompt_capsule_collision_is_rejected() {
    let bundle = fixture_bundle();
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-capsule-collision".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::UpsertPromptCapsule {
            capsule_id: "another-capsule".to_string(),
            capsule: WorkflowPatchPromptCapsuleBody {
                node_id: "query_logs".to_string(),
                prompt: "Override".to_string(),
                ..WorkflowPatchPromptCapsuleBody::default()
            },
        }],
    };
    let report = validate_workflow_patch(&bundle, &patch, None);
    assert!(!report.valid);
    assert!(report
        .apply_errors
        .iter()
        .any(|err| err.message.contains("already targets")));
}

#[test]
fn update_node_policy_succeeds_when_within_ceiling() {
    let bundle = fixture_bundle();
    let mut tool_policy = BTreeMap::new();
    tool_policy.insert("github_search".to_string(), json!({"allowed": true}));
    let patch = WorkflowPatch {
        schema_version: WORKFLOW_PATCH_SCHEMA_VERSION,
        id: "patch-update-policy".to_string(),
        summary: None,
        operations: vec![WorkflowPatchOperation::UpdateBundlePolicy {
            policy: WorkflowPatchBundlePolicyBody {
                tool_policy: Some(tool_policy),
                ..WorkflowPatchBundlePolicyBody::default()
            },
        }],
    };
    let report =
        validate_workflow_patch(&bundle, &patch, Some(&parent_ceiling_act_with_approval()));
    assert!(report.valid, "{report:#?}");
    assert!(report
        .graph_diff
        .policy_fields_changed
        .contains(&"tool_policy".to_string()));
}

#[test]
fn bundle_capability_ceiling_includes_connector_call_for_bundles_with_connectors() {
    let bundle = fixture_bundle();
    let ceiling = bundle_capability_ceiling(&bundle);
    let connector_ops = ceiling
        .capabilities
        .get("connector")
        .expect("connector capability is derived from connector requirements");
    assert!(connector_ops.iter().any(|op| op == "call"));
}

#[test]
fn bundle_capability_ceiling_floors_side_effect_to_autonomy_tier() {
    let mut bundle = fixture_bundle();
    bundle.policy.autonomy_tier = "act_auto".to_string();
    let ceiling = bundle_capability_ceiling(&bundle);
    assert_eq!(ceiling.side_effect_level.as_deref(), Some("network"));
}
