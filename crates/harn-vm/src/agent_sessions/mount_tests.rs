//! A mount the sandbox backend refuses must leave no trace: the anchor, the
//! redo stack and the transcript read as before the attempt.

use crate::workspace_anchor::{MountMode, WorkspaceAnchor};

struct SessionCapRestore(usize);

impl SessionCapRestore {
    fn set(cap: usize) -> Self {
        let previous = crate::agent_sessions::session_cap();
        crate::agent_sessions::set_session_cap(cap);
        Self(previous)
    }
}

impl Drop for SessionCapRestore {
    fn drop(&mut self) {
        crate::agent_sessions::set_session_cap(self.0);
    }
}

#[test]
fn retained_session_roots_survive_capacity_pressure_until_release() {
    use crate::agent_sessions::{open_or_create, open_retained_with_actor_chain, SessionOpenError};
    crate::reset_thread_local_state();
    let _cap = SessionCapRestore::set(1);
    let (session, retention) =
        open_retained_with_actor_chain(Some("retained-root".into()), None).unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut anchor = WorkspaceAnchor {
        primary: root.path().to_path_buf(),
        additional_roots: Vec::new(),
        anchored_at: "0".into(),
    };
    anchor.canonicalize_roots().unwrap();
    crate::agent_sessions::set_workspace_anchor(&session, Some(anchor.clone())).unwrap();
    let error = open_or_create(Some("pressure".into())).unwrap_err();
    assert!(matches!(
        error,
        SessionOpenError::CapacityExhausted {
            limit: 1,
            active: 1,
            protected: 1
        }
    ));
    assert_eq!(
        crate::agent_sessions::workspace_anchor(&session),
        Some(anchor)
    );
    drop(retention);
    assert_eq!(open_or_create(Some("pressure".into())).unwrap(), "pressure");
    assert!(!crate::agent_sessions::exists(&session));
}

#[test]
fn session_retention_is_bound_to_the_admitted_generation() {
    use crate::agent_sessions::{close, open_or_create, open_retained_with_actor_chain};
    crate::reset_thread_local_state();
    let _cap = SessionCapRestore::set(1);
    let (id, old) = open_retained_with_actor_chain(Some("reused-root".into()), None).unwrap();
    assert!(close(&id));
    let (_, current) = open_retained_with_actor_chain(Some(id.clone()), None).unwrap();
    drop(old);
    open_or_create(Some("pressure".into())).unwrap_err();
    assert!(close(&id));
    open_or_create(Some(id.clone())).unwrap();
    assert_eq!(open_or_create(Some("pressure".into())).unwrap(), "pressure");
    assert!(!crate::agent_sessions::exists(&id));
    drop(current);
}

fn root_mounted_events(session: &str) -> usize {
    crate::agent_sessions::transcript(session)
        .as_ref()
        .map(crate::llm::helpers::vm_value_to_json)
        .and_then(|transcript| transcript.get("events").cloned())
        .and_then(|events| events.as_array().cloned())
        .map_or(0, |events| {
            events
                .iter()
                .filter(|event| event["kind"] == "RootMounted")
                .count()
        })
}

fn anchored_session(name: &str) -> String {
    crate::reset_thread_local_state();
    let session = crate::agent_sessions::open_or_create_for_test(Some(name.to_string()));
    let anchor = WorkspaceAnchor {
        primary: std::env::temp_dir(),
        additional_roots: Vec::new(),
        anchored_at: "0".to_string(),
    };
    crate::agent_sessions::set_workspace_anchor(&session, Some(anchor)).unwrap();
    session
}

#[test]
fn a_refused_new_mount_records_no_event_and_leaves_no_root() {
    let session = anchored_session("refused-new-mount");
    let root = tempfile::tempdir().unwrap();
    let text = root.path().to_string_lossy().to_string();
    let refused = crate::agent_sessions::add_workspace_root_verified(
        &session,
        &text,
        Some(MountMode::ReadOnly),
        None,
        |_| Err("refused".to_string()),
    );
    assert_eq!(refused.unwrap_err(), "refused");
    assert_eq!(root_mounted_events(&session), 0);
    let (_, roots) = crate::agent_sessions::list_workspace_roots(&session).unwrap();
    assert!(roots.is_empty());
}

#[test]
fn a_refused_remount_restores_the_prior_mount_and_adds_no_event() {
    let session = anchored_session("refused-remount");
    let root = tempfile::tempdir().unwrap();
    let text = root.path().to_string_lossy().to_string();
    crate::agent_sessions::add_workspace_root(&session, &text, Some(MountMode::Extend), None)
        .unwrap();
    let (_, before) = crate::agent_sessions::list_workspace_roots(&session).unwrap();
    assert_eq!(root_mounted_events(&session), 1);
    let refused = crate::agent_sessions::add_workspace_root_verified(
        &session,
        &text,
        Some(MountMode::ReadOnly),
        None,
        |_| Err("refused".to_string()),
    );
    assert!(refused.is_err());
    let (_, after) = crate::agent_sessions::list_workspace_roots(&session).unwrap();
    assert_eq!(after, before, "the earlier mount, timestamp included");
    assert_eq!(root_mounted_events(&session), 1);
}
