use super::*;

/// A bare/`Any` deny rule denies the whole tool: a hard ceiling, so the
/// denial is TERMINAL.
#[tokio::test(flavor = "current_thread")]
async fn whole_tool_deny_rule_is_terminal() {
    crate::reset_thread_local_state();
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("dyn-perm-tool-deny".to_string()));
    let policy = DynamicPermissionPolicy {
        allow: Vec::new(),
        deny: vec![PermissionRule {
            tool_pattern: "exec".to_string(),
            matcher: PermissionMatcher::Any,
        }],
        on_escalation: None,
    };
    let mut grants = BTreeSet::new();
    let check = check_one_dynamic_permission(
        None,
        &policy,
        0,
        &mut grants,
        "exec",
        &serde_json::json!({"command": "ls"}),
        &session_id,
    )
    .await
    .expect("permission check");
    assert!(
        !denial_recoverable(check),
        "a whole-tool deny rule is a hard ceiling and must be terminal"
    );
}
