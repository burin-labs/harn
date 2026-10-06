use std::path::Path;

use crate::workspace_anchor::MountedRoot;

use super::*;

fn anchor(base: &Path) -> WorkspaceAnchor {
    WorkspaceAnchor {
        primary: base.join("harn-anchor-a"),
        additional_roots: vec![
            MountedRoot {
                path: base.join("harn-anchor-b"),
                mount_mode: MountMode::Extend,
                mounted_at: "2026-05-24T00:00:00Z".to_string(),
            },
            MountedRoot {
                path: base.join("harn-anchor-c"),
                mount_mode: MountMode::ReadOnly,
                mounted_at: "2026-05-24T00:00:00Z".to_string(),
            },
        ],
        anchored_at: "2026-05-24T00:00:00Z".to_string(),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn path_scope_matcher_round_trips_through_serde() {
    let matcher = PathScopeMatcher {
        scope: PathScopeMode::AnchorPlusMounted(MountModeFilter {
            modes: vec![MountMode::Extend],
        }),
        arg_keys: vec!["path".to_string(), "destination".to_string()],
        on_violation: PathScopeAction::Deny,
    };
    let encoded = serde_json::to_string(&matcher).expect("serialize");
    let decoded: PathScopeMatcher = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, matcher);
}

#[test]
fn path_scope_allows_extend_mounts_and_rejects_filtered_modes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let anchor = anchor(temp.path());
    let scope = PathScopeMode::AnchorPlusMounted(MountModeFilter {
        modes: vec![MountMode::Extend],
    });
    let extend_path = path_string(&anchor.additional_roots[0].path.join("file.txt"));
    assert!(anchor_path_scope_violation(&extend_path, &scope, &anchor).is_none());
    let read_only_root = anchor.additional_roots[1].path.display().to_string();
    let read_only_path = path_string(&anchor.additional_roots[1].path.join("file.txt"));
    let reason = anchor_path_scope_violation(&read_only_path, &scope, &anchor)
        .expect("read-only root is outside writable scope");
    assert!(
        reason.contains(&format!("{read_only_root} [read_only]")),
        "{reason}"
    );
}

#[test]
fn path_scope_rejects_absolute_paths_outside_anchor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let anchor = anchor(temp.path());
    let outside_path = path_string(&temp.path().join("harn-anchor-z").join("file.txt"));
    let extend_root = anchor.additional_roots[0].path.display().to_string();
    let reason = anchor_path_scope_violation(&outside_path, &PathScopeMode::AnchorOnly, &anchor)
        .expect("outside path is rejected");
    assert!(reason.contains(&format!("path '{outside_path}' is outside anchor")));
    assert!(reason.contains(&format!("{extend_root} [extend]")));
}

#[tokio::test(flavor = "current_thread")]
async fn path_scope_allow_violation_is_not_overridden_by_later_allow() {
    crate::reset_thread_local_state();
    let temp = tempfile::tempdir().expect("tempdir");
    let anchor = anchor(temp.path());
    let mounted_path = path_string(&anchor.additional_roots[0].path.join("file.txt"));
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(
        "path-scope-terminal-allow".to_string(),
    ));
    crate::agent_sessions::set_workspace_anchor(&session_id, Some(anchor)).expect("set anchor");
    let rules = vec![
        PermissionRule {
            tool_pattern: "Read".to_string(),
            matcher: PermissionMatcher::PathScope(PathScopeMatcher {
                scope: PathScopeMode::AnchorOnly,
                arg_keys: vec!["path".to_string()],
                on_violation: PathScopeAction::Deny,
            }),
        },
        PermissionRule {
            tool_pattern: "*".to_string(),
            matcher: PermissionMatcher::Any,
        },
    ];
    let result = first_matching_allow_rule(
        None,
        &rules,
        "Read",
        &serde_json::json!({"path": mounted_path}),
        &session_id,
    )
    .await
    .expect("check allow rules");
    assert!(
        matches!(result, AllowRuleMatch::Rejected(reason) if reason.contains("outside anchor"))
    );
}

fn denial_recoverable(check: PermissionCheck) -> bool {
    match check {
        PermissionCheck::Denied { recoverable, .. } => recoverable,
        PermissionCheck::Granted { reason, .. } => {
            panic!("expected a denial, got grant: {reason}")
        }
    }
}

/// A path-scope allow rule that rejects this specific path is arg/path-scoped:
/// the tool is permitted, only the path is out of scope, so the denial is
/// RECOVERABLE (mirrors the ArgConstraint allow-list gate, harn#3670).
#[tokio::test(flavor = "current_thread")]
async fn path_scope_allow_rejection_is_recoverable() {
    crate::reset_thread_local_state();
    let temp = tempfile::tempdir().expect("tempdir");
    let anchor = anchor(temp.path());
    let outside_path = path_string(&temp.path().join("harn-anchor-z").join("file.txt"));
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("dyn-perm-path-scope".to_string()));
    crate::agent_sessions::set_workspace_anchor(&session_id, Some(anchor)).expect("set anchor");
    let policy = DynamicPermissionPolicy {
        allow: vec![PermissionRule {
            tool_pattern: "Read".to_string(),
            matcher: PermissionMatcher::PathScope(PathScopeMatcher {
                scope: PathScopeMode::AnchorOnly,
                arg_keys: vec!["path".to_string()],
                on_violation: PathScopeAction::Deny,
            }),
        }],
        deny: Vec::new(),
        on_escalation: None,
    };
    let mut grants = BTreeSet::new();
    let check = check_one_dynamic_permission(
        None,
        &policy,
        0,
        &mut grants,
        "Read",
        &serde_json::json!({"path": outside_path}),
        &session_id,
    )
    .await
    .expect("permission check");
    assert!(
        denial_recoverable(check),
        "path-scope allow rejection must be recoverable"
    );
}

/// A tool that matches no allow rule is a hard ceiling: the tool itself is
/// not permitted, so the denial is TERMINAL (not recoverable).
#[tokio::test(flavor = "current_thread")]
async fn tool_not_in_allow_list_is_terminal() {
    crate::reset_thread_local_state();
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("dyn-perm-tool-ceiling".to_string()));
    let policy = DynamicPermissionPolicy {
        allow: vec![PermissionRule {
            tool_pattern: "Read".to_string(),
            matcher: PermissionMatcher::Any,
        }],
        deny: Vec::new(),
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
        "a tool outside the allow-list is a hard ceiling and must be terminal"
    );
}

/// A deny rule keyed on a specific argument value is arg-scoped: re-issuing
/// with an allowed value can succeed, so the denial is RECOVERABLE.
#[tokio::test(flavor = "current_thread")]
async fn arg_keyed_deny_rule_is_recoverable() {
    crate::reset_thread_local_state();
    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("dyn-perm-arg-deny".to_string()));
    let policy = DynamicPermissionPolicy {
        allow: Vec::new(),
        deny: vec![PermissionRule {
            tool_pattern: "exec".to_string(),
            matcher: PermissionMatcher::Patterns(vec!["rm *".to_string()]),
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
        &serde_json::json!({"command": "rm -rf /"}),
        &session_id,
    )
    .await
    .expect("permission check");
    assert!(
        denial_recoverable(check),
        "an arg-keyed deny rule is arg-scoped and must be recoverable"
    );
}

mod whole_tool_deny_tests;
