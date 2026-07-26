//! Workflow normalization and validation.
//!
//! The legacy act/verify/repair shape upgrades in place, tool-registry nodes are
//! accepted, a condition node without both true and false edges is rejected, and
//! `exit_when_verified` round-trips through serde while defaulting to false for
//! records written before the field existed.

use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn workflow_normalization_upgrades_legacy_act_verify_repair_shape() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "name": "legacy",
        "act": {"mode": "llm"},
        "verify": {"kind": "verify"},
        "repair": {"mode": "agent"},
    }));
    let graph = normalize_workflow_value(&value).unwrap();
    assert_eq!(graph.type_name, "workflow_graph");
    assert!(graph.nodes.contains_key("act"));
    assert!(graph.nodes.contains_key("verify"));
    assert!(graph.nodes.contains_key("repair"));
    assert_eq!(graph.entry, "act");
}

#[test]
fn workflow_normalization_accepts_tool_registry_nodes() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "name": "registry_tools",
        "entry": "implement",
        "nodes": {
            "implement": {
                "kind": "stage",
                "mode": "agent",
                "tools": {
                    "_type": "tool_registry",
                    "tools": [
                        {"name": "read", "description": "Read files"},
                        {"name": "run", "description": "Run commands"}
                    ]
                }
            }
        },
        "edges": []
    }));
    let graph = normalize_workflow_value(&value).unwrap();
    let node = graph.nodes.get("implement").unwrap();
    assert_eq!(
        crate::tool_surface::tool_names_from_spec(&node.tools),
        vec!["read", "run"]
    );
}

#[test]
fn workflow_validation_rejects_condition_without_true_false_edges() {
    let graph = WorkflowGraph {
        entry: "gate".to_string(),
        nodes: BTreeMap::from([(
            "gate".to_string(),
            WorkflowNode {
                id: Some("gate".to_string()),
                kind: "condition".to_string(),
                ..Default::default()
            },
        )]),
        edges: vec![WorkflowEdge {
            from: "gate".to_string(),
            to: "next".to_string(),
            branch: Some("true".to_string()),
            label: None,
        }],
        ..Default::default()
    };
    let report = validate_workflow(&graph, None);
    assert!(!report.valid);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("true") && error.contains("false")));
}

#[test]
fn workflow_node_defaults_exit_when_verified_to_false() {
    let node = WorkflowNode::default();
    assert!(!node.exit_when_verified);
}

#[test]
fn workflow_node_exit_when_verified_round_trips_through_serde() {
    let node = WorkflowNode {
        id: Some("execute".to_string()),
        kind: "stage".to_string(),
        exit_when_verified: true,
        ..Default::default()
    };
    let encoded = serde_json::to_value(&node).expect("serialize");
    assert_eq!(
        encoded.get("exit_when_verified"),
        Some(&serde_json::json!(true))
    );
    let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
    assert!(decoded.exit_when_verified);
}

#[test]
fn workflow_node_exit_when_verified_accepts_missing_field_for_backcompat() {
    let encoded = serde_json::json!({
        "id": "legacy_stage",
        "kind": "stage",
    });
    let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
    assert!(
        !decoded.exit_when_verified,
        "nodes serialized before this field was added must deserialize with the default"
    );
}

// --- Trusted-bridge scope for the runtime's own registered closures ---------
//
// The agent loop runs under an active execution policy. When the runtime
// invokes a closure IT registered (a reminder provider or a session hook) and
// that closure's body calls an app-provided bridged builtin, the call must be
// treated as a trusted bridge call rather than a model-issued tool call.
// Otherwise `enforce_current_policy_for_bridge_builtin` rejects it with
// "exceeds execution policy" and kills the turn — the regression these tests
// lock. Trust is narrowed to the runtime's own registered-closure seams, not
// the whole policy (see the negative control below).
