use super::*;

fn skill_induce_trace(id: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "source_hash": format!("sha256:{id}"),
        "actions": [
            {
                "id": format!("{id}-branch"),
                "kind": "tool_call",
                "name": "git.checkout_branch",
                "parameters": {
                    "branch_name": format!("release-{version}")
                },
                "side_effects": [
                    {
                        "kind": "git_ref",
                        "target": "release-branch",
                        "capability": "git.write"
                    }
                ],
                "capabilities": ["git.write"],
                "deterministic": true
            },
            {
                "id": format!("{id}-manifest"),
                "kind": "file_mutation",
                "name": "update_manifest_version",
                "parameters": {
                    "version": version
                },
                "side_effects": [
                    {
                        "kind": "file_write",
                        "target": "harn.toml",
                        "capability": "fs.write"
                    }
                ],
                "capabilities": ["fs.write"],
                "deterministic": true
            }
        ]
    })
}

#[test]
fn skill_induce_accepts_single_trace_with_heldout_sibling() {
    let payload = serde_json::json!({
        "trace": skill_induce_trace("source_trace", "0.9.1"),
        "heldout_traces": [skill_induce_trace("heldout_trace", "0.9.2")],
        "options": {
            "workflow_name": "release_package_maintenance",
            "package_name": "release-workflows"
        }
    });

    let result = skill_induce_impl(
        &[crate::stdlib::json_to_vm_value(&payload)],
        &mut String::new(),
    )
    .expect("skill_induce");
    let result = result.as_dict().expect("skill_induce result dict");
    assert!(matches!(result.get("accepted"), Some(VmValue::Bool(true))));

    let Some(VmValue::List(skills)) = result.get("skill_candidates") else {
        panic!("skill_candidates should be a list");
    };
    assert_eq!(skills.len(), 1);

    let skill = skills[0].as_dict().expect("skill candidate dict");
    let replay_gate = skill
        .get("replay_gate")
        .and_then(VmValue::as_dict)
        .expect("replay_gate dict");
    assert_eq!(
        replay_gate
            .get("original_trace_count")
            .and_then(VmValue::as_int),
        Some(1)
    );
    assert_eq!(
        replay_gate
            .get("heldout_trace_count")
            .and_then(VmValue::as_int),
        Some(1)
    );

    let receipt = replay_gate
        .get("receipt")
        .and_then(VmValue::as_dict)
        .expect("receipt dict");
    assert!(matches!(receipt.get("accepted"), Some(VmValue::Bool(true))));
}
