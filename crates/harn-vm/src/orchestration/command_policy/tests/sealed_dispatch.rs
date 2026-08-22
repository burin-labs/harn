use super::*;

fn authority(command: &str, root: &str) -> SealedDispatchAuthority {
    let mut authority = SealedDispatchAuthority {
        schema: SEALED_DISPATCH_AUTHORITY_SCHEMA.to_string(),
        source: "host".to_string(),
        intent_id: "prepared-test-intent".to_string(),
        lease_fingerprint: "blake3:test-lease".to_string(),
        plan_fingerprint: "blake3:test-plan".to_string(),
        workspace_root_sha256: sha256_fingerprint(root),
        command_sha256: sha256_fingerprint(command.trim()),
        binding_sha256: String::new(),
        allow_risk_labels: SEALED_DISPATCH_ALLOWED_RISK_LABELS
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    };
    authority.binding_sha256 = authority.expected_binding_sha256();
    authority
}

fn policy(command: &str, root: &str) -> CommandPolicy {
    CommandPolicy {
        workspace_roots: vec![root.to_string()],
        default_shell_mode: "shell".to_string(),
        require_approval: [
            EXECUTION_SEMANTICS_UNRESOLVED_LABEL.to_string(),
            "outside_workspace".to_string(),
            "write_intent".to_string(),
            "credential_file_read".to_string(),
        ]
        .into_iter()
        .collect(),
        sealed_dispatch: Some(authority(command, root)),
        ..Default::default()
    }
}

fn authority_json(command: &str, root: &str) -> JsonValue {
    let authority = authority(command, root);
    serde_json::json!({
        "schema": authority.schema,
        "source": authority.source,
        "intent_id": authority.intent_id,
        "lease_fingerprint": authority.lease_fingerprint,
        "plan_fingerprint": authority.plan_fingerprint,
        "workspace_root_sha256": authority.workspace_root_sha256,
        "command_sha256": authority.command_sha256,
        "binding_sha256": authority.binding_sha256,
        "allow_risk_labels": authority.allow_risk_labels,
    })
}

fn assert_rejected(preflight: &CommandPolicyPreflight) {
    match preflight {
        CommandPolicyPreflight::Blocked { decisions, .. } => {
            assert!(decisions.iter().any(|decision| {
                decision.source == "sealed_dispatch_authority" && decision.action == "reject"
            }));
            assert!(!decisions.iter().any(|decision| {
                decision.source == "sealed_dispatch_authority" && decision.action == "authorize"
            }));
        }
        other => panic!("invalid sealed dispatch unexpectedly proceeded: {other:?}"),
    }
}

#[tokio::test]
async fn exact_host_seal_suppresses_only_bounded_approval_labels() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "target=$OUTPUT && printf verifier-ok > \"$target\"";
    clear_command_policies();
    push_command_policy(policy(COMMAND, ROOT));

    match preflight_shell(COMMAND).await {
        CommandPolicyPreflight::Proceed { decisions, .. } => {
            let accepted = decisions
                .iter()
                .find(|decision| decision.source == "sealed_dispatch_authority")
                .expect("sealed authority decision");
            assert_eq!(accepted.action, "authorize");
            assert!(accepted
                .risk_labels
                .contains(&EXECUTION_SEMANTICS_UNRESOLVED_LABEL.to_string()));
            assert!(accepted.risk_labels.contains(&"write_intent".to_string()));
        }
        other => panic!("exact sealed dispatch was not authorized: {other:?}"),
    }
    clear_command_policies();
}

#[tokio::test]
async fn exact_host_seal_keeps_unrelated_approval_labels_authoritative() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "cat .env && printf verifier-ok > proof.txt";
    clear_command_policies();
    push_command_policy(policy(COMMAND, ROOT));

    match preflight_shell(COMMAND).await {
        CommandPolicyPreflight::Blocked { decisions, .. } => {
            assert!(decisions.iter().any(|decision| {
                decision.source == "sealed_dispatch_authority" && decision.action == "authorize"
            }));
            assert!(decisions.iter().any(|decision| {
                decision.source == "deterministic"
                    && decision.action == "require_approval"
                    && decision
                        .risk_labels
                        .contains(&"credential_file_read".to_string())
            }));
        }
        other => panic!("sealed dispatch bypassed credential approval: {other:?}"),
    }
    clear_command_policies();
}

#[tokio::test]
async fn seal_rejects_mutated_command_root_hash_lease_and_binding() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "true";

    clear_command_policies();
    push_command_policy(policy(COMMAND, ROOT));
    assert_rejected(&preflight_shell("printf changed").await);

    clear_command_policies();
    let mut wrong_root = policy(COMMAND, ROOT);
    let authority = wrong_root.sealed_dispatch.as_mut().unwrap();
    authority.workspace_root_sha256 = sha256_fingerprint("/tmp/other");
    authority.binding_sha256 = authority.expected_binding_sha256();
    push_command_policy(wrong_root);
    assert_rejected(&preflight_shell(COMMAND).await);

    clear_command_policies();
    let mut wrong_hash = policy(COMMAND, ROOT);
    let authority = wrong_hash.sealed_dispatch.as_mut().unwrap();
    authority.command_sha256 = sha256_fingerprint("other");
    authority.binding_sha256 = authority.expected_binding_sha256();
    push_command_policy(wrong_hash);
    assert_rejected(&preflight_shell(COMMAND).await);

    clear_command_policies();
    let mut wrong_lease = policy(COMMAND, ROOT);
    wrong_lease
        .sealed_dispatch
        .as_mut()
        .unwrap()
        .lease_fingerprint = "blake3:wrong".into();
    push_command_policy(wrong_lease);
    assert_rejected(&preflight_shell(COMMAND).await);

    clear_command_policies();
    let mut wrong_binding = policy(COMMAND, ROOT);
    wrong_binding
        .sealed_dispatch
        .as_mut()
        .unwrap()
        .binding_sha256 = sha256_fingerprint("forged");
    push_command_policy(wrong_binding);
    assert_rejected(&preflight_shell(COMMAND).await);
    clear_command_policies();
}

#[tokio::test]
async fn process_request_cannot_self_declare_a_seal() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "target=$OUTPUT && printf verifier-ok > \"$target\"";
    clear_command_policies();
    push_command_policy(CommandPolicy::default());

    let mut params = shell_params(COMMAND);
    params.insert(
        crate::value::intern_key("sealed_dispatch"),
        crate::stdlib::json_to_vm_value(&authority_json(COMMAND, ROOT)),
    );
    match run_command_policy_preflight(&params, JsonValue::Null)
        .await
        .expect("preflight ok")
    {
        CommandPolicyPreflight::Blocked { decisions, .. } => {
            assert!(decisions.iter().any(|decision| {
                decision.action == "require_approval" && decision.source == "deterministic"
            }));
            assert!(!decisions
                .iter()
                .any(|decision| decision.source == "sealed_dispatch_authority"));
        }
        other => panic!("request-owned seal unexpectedly authorized dispatch: {other:?}"),
    }
    clear_command_policies();
}

#[test]
fn shape_rejects_unbounded_risk_labels_and_malformed_leases() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "target=$OUTPUT && printf verifier-ok > \"$target\"";

    let mut widened = authority_json(COMMAND, ROOT);
    widened["allow_risk_labels"] = serde_json::json!([
        "execution_semantics_unresolved",
        "outside_workspace",
        "write_intent",
        "credential_file_read",
    ]);
    let widened_policy = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "sealed_dispatch": widened,
    }));
    let widened_error = parse_command_policy_value(Some(&widened_policy), "test policy")
        .expect_err("a seal must not widen beyond the fixed verifier-write labels");
    assert!(widened_error
        .to_string()
        .contains("must be a non-empty subset"));

    let mut malformed = authority_json(COMMAND, ROOT);
    malformed["lease_fingerprint"] = serde_json::json!("caller-controlled");
    let malformed_policy = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "sealed_dispatch": malformed,
    }));
    let malformed_error = parse_command_policy_value(Some(&malformed_policy), "test policy")
        .expect_err("a seal requires a typed host lease fingerprint");
    assert!(malformed_error
        .to_string()
        .contains("lease/plan fingerprints are invalid"));
}

#[tokio::test]
async fn seal_never_bypasses_the_catastrophic_floor() {
    const ROOT: &str = "/tmp/work";
    const COMMAND: &str = "rm -rf /";
    clear_command_policies();
    push_command_policy(policy(COMMAND, ROOT));
    assert_floor_blocked(&preflight_shell(COMMAND).await);
    clear_command_policies();
}
