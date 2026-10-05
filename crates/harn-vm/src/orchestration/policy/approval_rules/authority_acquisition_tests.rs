//! Authority negotiation must not inherit permission to perform concrete effects.

use super::super::{
    evaluate_authority_request, PolicyAuthorityRequest, ToolApprovalPolicy, ToolApprovalRequest,
    ToolApprovalWorkspaceBoundary,
};

#[test]
fn authority_acquisition_does_not_admit_a_concrete_external_effect() {
    let workspace = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let args = serde_json::json!({"path": external.path(), "access": "read"});
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_external_paths": false,
        "allow_sensitive_paths": true,
        "rules": [{"id": "acquire", "ask": {"tool": "prepared_run.filesystem"}}]
    }))
    .unwrap();
    let authority = PolicyAuthorityRequest {
        authority_name: "prepared_run.filesystem".into(),
        arguments: args.clone(),
    };
    let acquisition = evaluate_authority_request(&policy, &authority).unwrap();
    assert!(acquisition.is_ask(), "{acquisition:?}");
    assert_eq!(acquisition.receipt["matched_rule"]["id"], "acquire");

    let effect = ToolApprovalRequest {
        tool_name: authority.authority_name,
        arguments: args,
        workspace_boundary: Some(ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_string_lossy().into_owned(),
        }),
        ..Default::default()
    };
    let refusal = policy.evaluate_request(&effect);
    assert!(refusal.is_deny(), "{refusal:?}");
    assert!(
        refusal
            .risk_labels
            .iter()
            .any(|label| label == "external_path"),
        "{refusal:?}"
    );
}

#[test]
fn literal_invocation_approval_cannot_grant_authority_acquisition() {
    let workspace = tempfile::tempdir().unwrap();
    let args = serde_json::json!({"path": "notes.txt", "access": "read"});
    let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
        "allow_sensitive_paths": true,
        "rules": [
            {"id": "remembered-effect", "source": "user", "identity_match": "literal",
                "allow": {"tool": "prepared_run.filesystem", "path": "notes.txt"}},
            {"id": "acquire", "source": "mode", "ask": {"tool": "prepared_run.filesystem"}}
        ]
    }))
    .unwrap();
    let effect = ToolApprovalRequest {
        tool_name: "prepared_run.filesystem".into(),
        arguments: args.clone(),
        workspace_boundary: Some(ToolApprovalWorkspaceBoundary {
            root: workspace.path().to_string_lossy().into_owned(),
        }),
        ..Default::default()
    };
    let remembered = policy.evaluate_request(&effect);
    assert!(remembered.is_allow(), "{remembered:?}");
    assert_eq!(
        remembered.receipt["matched_rule"]["id"],
        "remembered-effect"
    );

    let acquisition = evaluate_authority_request(
        &policy,
        &PolicyAuthorityRequest {
            authority_name: effect.tool_name,
            arguments: args,
        },
    )
    .unwrap();
    assert!(acquisition.is_ask(), "{acquisition:?}");
    assert_eq!(acquisition.receipt["matched_rule"]["id"], "acquire");
}
