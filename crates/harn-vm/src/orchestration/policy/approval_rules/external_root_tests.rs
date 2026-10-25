//! The workspace boundary's external-root and session-mount decisions.

use super::*;
use crate::orchestration::{pop_execution_policy, ToolApprovalPolicy};
use crate::tool_annotations::ToolKind;

/// The one place these tests reach the host layer, so the module-layering
/// ratchet counts a single upward reference.
fn set_execution_context(record: Option<crate::orchestration::RunExecutionRecord>) {
    crate::stdlib::process::set_thread_execution_context(record);
}

fn with_workspace_and_external_root<T>(run: impl FnOnce(&str, &str) -> T) -> T {
    let workspace = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    set_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(workspace.path().to_string_lossy().into_owned()),
        source_dir: Some(workspace.path().to_string_lossy().into_owned()),
        ..Default::default()
    }));
    let external_root = external
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let inside = external
        .path()
        .canonicalize()
        .unwrap()
        .join("notes.txt")
        .to_string_lossy()
        .into_owned();
    let result = run(&external_root, &inside);
    set_execution_context(None);
    result
}

fn evaluate_annotated(
    policy: &ToolApprovalPolicy,
    tool: &str,
    kind: ToolKind,
    path: &str,
) -> PolicyEvaluation {
    policy_with_path_annotation(tool, kind);
    let decision =
        evaluate_tool_approval_policy(policy, tool, &serde_json::json!({ "path": path }), None);
    pop_execution_policy();
    decision
}

/// The negative-control shape: the policy carries nothing but the root. No
/// deny rule, no `auto_deny`, no write-path allowlist. A refused write here
/// can only come from Harn's own boundary reading the root's mode.
#[test]
fn a_string_external_root_admits_reads_and_refuses_writes_at_the_boundary() {
    with_workspace_and_external_root(|root, inside| {
        let policy: ToolApprovalPolicy =
            serde_json::from_value(serde_json::json!({ "external_roots": [root] })).unwrap();
        assert!(policy.rules.is_empty() && policy.auto_deny.is_empty());

        let read = evaluate_annotated(&policy, "read_file", ToolKind::Read, inside);
        assert!(read.is_allow(), "read under a read root: {}", read.reason);
        assert_eq!(
            read.receipt["context"]["external_roots"],
            serde_json::json!([{ "path": root, "access": "read" }])
        );

        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(write.is_deny(), "write under a read root: {}", write.reason);
        let rule = write.matched_rule.as_ref().expect("the boundary decided");
        assert_eq!(rule.source, SOURCE_DEFAULT_EXTERNAL_PATH);
        assert_eq!(rule.id.as_deref(), Some(EXTERNAL_ROOT_READ_ONLY));
        assert_eq!(write.risk_labels, vec![EXTERNAL_ROOT_READ_ONLY.to_string()]);
        // The boundary reports a host path with forward slashes on every OS.
        let reported = inside.replace('\\', "/");
        assert_eq!(write.denied_paths, vec![reported.clone()]);
        assert!(
            write.reason.contains(&reported)
                && write.reason.contains(root)
                && write.reason.contains("'read'"),
            "the refusal names the path, the root, and its mode: {}",
            write.reason
        );
        assert_eq!(
            write.denial_gate(),
            crate::agent_events::DenialGate::WorkspaceBoundary
        );
        assert_eq!(
            write.receipt["context"]["external_roots"],
            serde_json::json!([{ "path": root, "access": "read" }])
        );
    });
}

#[test]
fn a_read_write_external_root_admits_writes() {
    with_workspace_and_external_root(|root, inside| {
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "external_roots": [{ "path": root, "access": "read_write" }]
        }))
        .unwrap();
        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(
            write.is_allow(),
            "write under a read_write root: {}",
            write.reason
        );
        assert_eq!(
            write.receipt["context"]["external_roots"],
            serde_json::json!([{ "path": root, "access": "read_write" }])
        );
    });
}

#[test]
fn a_read_root_still_refuses_writes_when_external_paths_are_opted_out() {
    with_workspace_and_external_root(|root, inside| {
        let policy = ToolApprovalPolicy {
            allow_external_paths: true,
            external_roots: vec![ExternalRoot::read(root)],
            ..Default::default()
        };
        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(write.is_deny(), "{}", write.reason);
        assert_eq!(
            write
                .matched_rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some(EXTERNAL_ROOT_READ_ONLY)
        );
    });
}

#[test]
fn intersecting_policies_keeps_the_narrower_mode_for_a_shared_root() {
    let host = ToolApprovalPolicy {
        external_roots: vec![ExternalRoot::read("/opt/shared")],
        ..Default::default()
    };
    let session = ToolApprovalPolicy {
        external_roots: vec![ExternalRoot::read_write("/opt/shared")],
        ..Default::default()
    };
    assert_eq!(
        host.intersect(&session).external_roots,
        vec![ExternalRoot::read("/opt/shared")]
    );
    assert_eq!(
        session.intersect(&host).external_roots,
        vec![ExternalRoot::read("/opt/shared")]
    );
}

/// A mount the host adds to the session mid-run is the same grant as a
/// declared external root. The policy here carries no root at all, so a
/// decision that admits the path can only have read the session's mounts.
#[test]
fn a_session_mount_widens_the_boundary_by_its_mode_and_only_while_mounted() {
    use crate::workspace_anchor::MountMode;
    crate::reset_thread_local_state();
    with_workspace_and_external_root(|root, inside| {
        let policy = ToolApprovalPolicy::default();
        let session =
            crate::agent_sessions::open_or_create_for_test(Some("mount-boundary".to_string()));
        let _current = crate::agent_sessions::enter_current_session(session.clone());
        let anchor = crate::workspace_anchor::WorkspaceAnchor {
            primary: std::env::temp_dir(),
            additional_roots: Vec::new(),
            anchored_at: "0".to_string(),
        };
        crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();

        let denied = evaluate_annotated(&policy, "read_file", ToolKind::Read, inside);
        assert!(denied.is_deny(), "unmounted root: {}", denied.reason);

        crate::agent_sessions::add_workspace_root(&session, root, Some(MountMode::ReadOnly), None)
            .unwrap();
        let read = evaluate_annotated(&policy, "read_file", ToolKind::Read, inside);
        assert!(
            read.is_allow(),
            "read under a read_only mount: {}",
            read.reason
        );
        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(
            write.is_deny(),
            "write under a read_only mount: {}",
            write.reason
        );

        crate::agent_sessions::add_workspace_root(&session, root, Some(MountMode::Extend), None)
            .unwrap();
        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(
            write.is_allow(),
            "write under an extend mount: {}",
            write.reason
        );

        crate::agent_sessions::add_workspace_root(&session, root, Some(MountMode::Sandboxed), None)
            .unwrap();
        let read = evaluate_annotated(&policy, "read_file", ToolKind::Read, inside);
        assert!(
            read.is_deny(),
            "a sandboxed mount grants nothing: {}",
            read.reason
        );
    });
}

/// A mount that spells a declared root differently (a symlink alias) is the
/// same root, so it cannot raise a declared `read` root to `read_write`.
#[cfg(unix)]
#[test]
fn a_mount_through_a_symlink_alias_cannot_widen_a_declared_read_root() {
    use crate::workspace_anchor::{MountMode, WorkspaceAnchor};
    crate::reset_thread_local_state();
    with_workspace_and_external_root(|root, inside| {
        let links = tempfile::tempdir().unwrap();
        let alias = links.path().join("alias");
        std::os::unix::fs::symlink(root, &alias).unwrap();
        let policy: ToolApprovalPolicy = serde_json::from_value(serde_json::json!({
            "external_roots": [alias.to_string_lossy()]
        }))
        .unwrap();
        let session =
            crate::agent_sessions::open_or_create_for_test(Some("mount-alias".to_string()));
        let _current = crate::agent_sessions::enter_current_session(session.clone());
        let anchor = WorkspaceAnchor {
            primary: std::env::temp_dir(),
            additional_roots: Vec::new(),
            anchored_at: "0".to_string(),
        };
        crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();
        crate::agent_sessions::add_workspace_root(&session, root, Some(MountMode::Extend), None)
            .unwrap();
        let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, inside);
        assert!(
            write.is_deny(),
            "the declared read root keeps its mode: {}",
            write.reason
        );
    });
}

/// The boundary agrees with the builtins: a read-only mount nested in the
/// workspace refuses a write there and nowhere else.
#[test]
fn a_read_only_mount_inside_the_workspace_refuses_writes_at_the_boundary() {
    use crate::workspace_anchor::{MountMode, WorkspaceAnchor};
    crate::reset_thread_local_state();
    let workspace = tempfile::tempdir().unwrap();
    let ws = workspace.path().canonicalize().unwrap();
    let vendor = ws.join("vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    set_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(ws.to_string_lossy().into_owned()),
        source_dir: Some(ws.to_string_lossy().into_owned()),
        ..Default::default()
    }));
    let session = crate::agent_sessions::open_or_create_for_test(Some("nested-ro-b".to_string()));
    let _current = crate::agent_sessions::enter_current_session(session.clone());
    let anchor = WorkspaceAnchor {
        primary: ws.clone(),
        additional_roots: Vec::new(),
        anchored_at: "0".to_string(),
    };
    crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();
    let policy = ToolApprovalPolicy::default();
    let nested = vendor.join("f.txt").to_string_lossy().into_owned();
    let beside = ws.join("a.txt").to_string_lossy().into_owned();
    let before = evaluate_annotated(&policy, "write_file", ToolKind::Edit, &nested);
    assert!(
        before.is_allow(),
        "control: writable before: {}",
        before.reason
    );
    crate::agent_sessions::add_workspace_root(
        &session,
        &vendor.to_string_lossy(),
        Some(MountMode::ReadOnly),
        None,
    )
    .unwrap();
    let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, &nested);
    assert!(write.is_deny(), "nested read-only mount: {}", write.reason);
    let read = evaluate_annotated(&policy, "read_file", ToolKind::Read, &nested);
    assert!(read.is_allow(), "still readable: {}", read.reason);
    let other = evaluate_annotated(&policy, "write_file", ToolKind::Edit, &beside);
    assert!(
        other.is_allow(),
        "the rest stays writable: {}",
        other.reason
    );
    set_execution_context(None);
}

/// The execution workspace is a writable root of its own. A read-only mount
/// nested in it must refuse writes even when the session anchor is elsewhere.
#[test]
fn a_read_only_mount_inside_the_execution_workspace_refuses_writes_off_anchor() {
    use crate::workspace_anchor::{MountMode, WorkspaceAnchor};
    crate::reset_thread_local_state();
    let workspace = tempfile::tempdir().unwrap();
    let anchor_dir = tempfile::tempdir().unwrap();
    let ws = workspace.path().canonicalize().unwrap();
    let vendor = ws.join("vendor");
    std::fs::create_dir_all(&vendor).unwrap();
    set_execution_context(Some(crate::orchestration::RunExecutionRecord {
        cwd: Some(ws.to_string_lossy().into_owned()),
        source_dir: Some(ws.to_string_lossy().into_owned()),
        ..Default::default()
    }));
    let session = crate::agent_sessions::open_or_create_for_test(Some("nested-ro-c".to_string()));
    let _current = crate::agent_sessions::enter_current_session(session.clone());
    let anchor = WorkspaceAnchor {
        primary: anchor_dir.path().canonicalize().unwrap(),
        additional_roots: Vec::new(),
        anchored_at: "0".to_string(),
    };
    crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();
    crate::agent_sessions::add_workspace_root(
        &session,
        &vendor.to_string_lossy(),
        Some(MountMode::ReadOnly),
        None,
    )
    .unwrap();
    let policy = ToolApprovalPolicy::default();
    let nested = vendor.join("f.txt").to_string_lossy().into_owned();
    let write = evaluate_annotated(&policy, "write_file", ToolKind::Edit, &nested);
    assert!(write.is_deny(), "off-anchor nested mount: {}", write.reason);
    let beside = ws.join("a.txt").to_string_lossy().into_owned();
    let other = evaluate_annotated(&policy, "write_file", ToolKind::Edit, &beside);
    assert!(
        other.is_allow(),
        "the rest stays writable: {}",
        other.reason
    );
    set_execution_context(None);
}
