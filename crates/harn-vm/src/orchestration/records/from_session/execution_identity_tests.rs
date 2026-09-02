use harn_session_store::{
    AppendEvent, CreateSession, MemorySessionStore, SessionEventKind, SessionStore,
};
use serde_json::json;

use super::*;

fn run_started(execution_id: &str) -> AppendEvent {
    AppendEvent::new(
        SessionEventKind::Custom {
            custom_type: "agent_run_started".to_string(),
        },
        json!({
            "transcript_event": {
                "id": "event-agent_run_started",
                "kind": "agent_run_started",
                "role": "assistant",
                "text": "",
                "metadata": {
                    "execution_id": execution_id,
                    "lifecycle_state": "running",
                },
            }
        }),
    )
}

async fn project(execution_id: &str) -> RunRecord {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(&meta.id, run_started(execution_id))
        .await
        .expect("append event");
    project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project session")
}

#[tokio::test]
async fn session_execution_identity_survives_projection_and_json_round_trip() {
    const EXECUTION_ID: &str = "hxe-019c13e0-8080-7000-8000-000000000041";
    let projected = project(EXECUTION_ID).await;
    assert_eq!(
        projected.evidence.execution_id.as_deref(),
        Some(EXECUTION_ID)
    );
    assert!(projected.evidence.gaps.is_empty());
    assert_eq!(
        crate::orchestration::validate_execution_evidence(&projected.evidence),
        Ok(())
    );

    let encoded = serde_json::to_vec(&projected).expect("encode projected run");
    let decoded: RunRecord = serde_json::from_slice(&encoded).expect("decode projected run");
    assert_eq!(decoded.evidence, projected.evidence);
    assert_eq!(
        crate::orchestration::validate_execution_evidence(&decoded.evidence),
        Ok(())
    );
}

#[tokio::test]
async fn malformed_session_execution_identity_becomes_an_explicit_gap() {
    let projected = project("external-run-id").await;
    assert_eq!(projected.evidence.execution_id, None);
    assert!(projected.evidence.gaps.iter().any(|gap| {
        gap.component == "execution_identity" && gap.code == "session_projection_invalid"
    }));
}
