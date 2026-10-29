use crate::agent_sessions::{open_retained_with_actor_chain, PreparedWorkspaceRoots};
use harn_session_store::{SessionStore, SessionType, SessionWriteLease};

#[tokio::test(flavor = "current_thread")]
async fn workspace_root_replacement_persists_before_changing_authority() {
    crate::reset_thread_local_state();
    let workspace = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state_root = workspace.path().join("captured-state");
    let launcher = crate::security::LauncherEnvironment::from_snapshot(
        [("HARN_STATE_DIR".into(), state_root.to_str().unwrap().into())].into(),
    );
    let scope = crate::SessionStoreScope::resolve(workspace.path(), &launcher).unwrap();
    assert_eq!(scope.state_root(), state_root);
    let directories = [second.path(), first.path()]
        .map(|path| path.canonicalize().unwrap().to_str().unwrap().to_string())
        .to_vec();
    let (session, _retention) =
        open_retained_with_actor_chain(Some("atomic-root-replacement".into()), None).unwrap();
    super::install_workspace_roots(
        &session,
        PreparedWorkspaceRoots::prepare(workspace.path(), &directories).unwrap(),
        &scope,
        SessionType::User,
    )
    .await
    .unwrap();
    let store = scope
        .run(async { crate::open_canonical_store(scope.workspace()) })
        .await
        .unwrap();
    assert_eq!(
        store
            .describe(&session)
            .await
            .unwrap()
            .additional_directories()
            .unwrap(),
        directories
    );
    let accepted = crate::agent_sessions::workspace_anchor(&session).unwrap();
    assert_eq!(accepted.additional_directories().unwrap(), directories);

    let lease = SessionWriteLease::try_acquire(scope.state_root(), &session).unwrap();
    assert!(super::install_workspace_roots(
        &session,
        PreparedWorkspaceRoots::prepare(workspace.path(), &[]).unwrap(),
        &scope,
        SessionType::User
    )
    .await
    .is_err());
    assert_eq!(
        crate::agent_sessions::workspace_anchor(&session),
        Some(accepted.clone())
    );
    assert_eq!(
        store
            .describe(&session)
            .await
            .unwrap()
            .additional_directories()
            .unwrap(),
        directories
    );
    drop(lease);

    let missing = workspace
        .path()
        .join("missing")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        PreparedWorkspaceRoots::prepare(workspace.path(), &[directories[0].clone(), missing])
            .is_err()
    );
    assert_eq!(
        crate::agent_sessions::workspace_anchor(&session),
        Some(accepted)
    );
    super::install_workspace_roots(
        &session,
        PreparedWorkspaceRoots::prepare(workspace.path(), &[]).unwrap(),
        &scope,
        SessionType::User,
    )
    .await
    .unwrap();
    assert!(store
        .describe(&session)
        .await
        .unwrap()
        .additional_directories()
        .unwrap()
        .is_empty());
    assert!(crate::agent_sessions::workspace_anchor(&session)
        .unwrap()
        .additional_roots
        .is_empty());
}
