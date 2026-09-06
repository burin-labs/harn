//! Child-run lineage: parents, reused sessions, and root-run resolution.

use harn_session_store::{CreateSession, MemorySessionStore};

use super::super::*;
use super::support::*;
#[tokio::test]
async fn child_sessions_project_as_child_runs_from_the_stores_own_lineage() {
    let store = MemorySessionStore::default();
    let parent = store
        .create(CreateSession::default())
        .await
        .expect("create parent");
    for name in ["worker-a", "worker-b"] {
        store
            .create(CreateSession {
                parent_session_id: Some(parent.id.clone()),
                persona: Some(name.to_string()),
                title: Some(format!("{name} task")),
                ..CreateSession::default()
            })
            .await
            .expect("create child");
    }

    let run = project_run_record_from_session(&store, &parent.id)
        .await
        .expect("project");
    assert_eq!(run.child_runs.len(), 2);
    let names: Vec<&str> = run
        .child_runs
        .iter()
        .map(|child| child.worker_name.as_str())
        .collect();
    assert_eq!(names, vec!["worker-a", "worker-b"]);
    assert!(run
        .child_runs
        .iter()
        .all(|child| child.parent_session_id.as_deref() == Some(parent.id.as_str())));
}

#[tokio::test]
async fn reused_session_projects_only_the_latest_invocations_children() {
    let store = MemorySessionStore::default();
    let parent = store
        .create(CreateSession::default())
        .await
        .expect("create parent");
    let old_child = store
        .create(CreateSession {
            parent_session_id: Some(parent.id.clone()),
            persona: Some("old-worker".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create old child");
    let current_child = store
        .create(CreateSession {
            parent_session_id: Some(parent.id.clone()),
            persona: Some("current-worker".to_string()),
            ..CreateSession::default()
        })
        .await
        .expect("create current child");

    store
        .append(&parent.id, run_started())
        .await
        .expect("append old start");
    store
        .append(
            &parent.id,
            sub_agent_start(&old_child.id, "agent_run_old_child"),
        )
        .await
        .expect("append old child start");
    store
        .append(&parent.id, run_started())
        .await
        .expect("append current start");
    store
        .append(
            &parent.id,
            sub_agent_start(&current_child.id, "agent_run_current_child"),
        )
        .await
        .expect("append current child start");

    let run = project_run_record_from_session(&store, &parent.id)
        .await
        .expect("project");
    assert_eq!(run.child_runs.len(), 1);
    assert_eq!(
        run.child_runs[0].session_id.as_deref(),
        Some(current_child.id.as_str())
    );
    assert_eq!(
        run.child_runs[0].run_id.as_deref(),
        Some("agent_run_current_child")
    );
}

/// `root_run_id` has to be the top of the delegation chain, not the immediate
/// parent. A grandchild reporting its parent as root would make a three-level
/// fan-out look like two unrelated two-level ones in any report that groups by
/// root.
#[tokio::test]
async fn a_grandchild_reports_the_top_of_its_chain_as_the_root_run() {
    let store = MemorySessionStore::default();
    let root = store
        .create(CreateSession::default())
        .await
        .expect("create root");
    let middle = store
        .create(CreateSession {
            parent_session_id: Some(root.id.clone()),
            ..CreateSession::default()
        })
        .await
        .expect("create middle");
    let leaf = store
        .create(CreateSession {
            parent_session_id: Some(middle.id.clone()),
            ..CreateSession::default()
        })
        .await
        .expect("create leaf");

    let projected = project_run_record_from_session(&store, &leaf.id)
        .await
        .expect("project");
    assert_eq!(projected.parent_run_id.as_deref(), Some(middle.id.as_str()));
    assert_eq!(
        projected.root_run_id.as_deref(),
        Some(root.id.as_str()),
        "root must be the chain's top, not one hop up"
    );

    // The root itself is its own root, which is what a report keying on
    // `root_run_id` needs in order to find the bundle at all.
    let root_projected = project_run_record_from_session(&store, &root.id)
        .await
        .expect("project root");
    assert_eq!(root_projected.parent_run_id, None);
    assert_eq!(
        root_projected.root_run_id.as_deref(),
        Some(root.id.as_str())
    );
}
