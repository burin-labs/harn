use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use harn_session_store::SessionStore;
use serde_json::json;

use crate::value::VmDictExt;

#[tokio::test(flavor = "current_thread")]
async fn ordinary_init_failure_persists_terminal_before_releasing_owned_session() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "ordinary-init-failure";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-init-failure".to_string(),
        "turn-init-failure".to_string(),
    )
    .await
    .expect("prepare journal");
    crate::agent_sessions::open_or_create(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    let mut rollback = super::AgentSessionInitRollback::new(session_id.to_string(), true);
    rollback.fail().await;

    assert!(!crate::agent_sessions::exists(session_id));
    assert!(!crate::agent_sessions::has_journal(session_id));
    let store = crate::stdlib::session_store::open_canonical_agent_session(
        &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
        session_id,
        None,
        harn_session_store::SessionType::User,
    )
    .await
    .expect("open canonical session");
    let events = crate::stdlib::session_store::read_all_events(&store, session_id)
        .await
        .expect("read canonical events");
    let terminals = events
        .iter()
        .filter(|event| event.payload.to_string().contains("agent_run_terminal"))
        .count();
    assert_eq!(terminals, 1, "ordinary failure needs one durable terminal");
    crate::agent_sessions::reset_session_store();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_finalize_flushes_once_and_fires_native_cleanup_once() {
    crate::agent_sessions::reset_session_store();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "abandoned-finalize";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-abandoned".to_string(),
        "turn-abandoned".to_string(),
    )
    .await
    .expect("prepare journal");
    crate::agent_sessions::open_or_create(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::llm::agent_session_host::seed_host_session_provider_model(session_id, "mock", "fixture");
    crate::llm::agent_session_host::record_provider_dispatch(session_id);
    crate::agent_sessions::inject_message(
        session_id,
        crate::stdlib::json_to_vm_value(&json!({
            "role": "user",
            "content": "persist before cancellation",
        })),
    )
    .expect("enqueue transcript mutation");

    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let observed_count = cleanup_count.clone();
    let _registration =
        crate::llm::agent_runtime::register_session_end_hook(Arc::new(move |ended_session_id| {
            if ended_session_id == session_id {
                observed_count.fetch_add(1, Ordering::SeqCst);
            }
        }));

    super::abandon_agent_session(session_id)
        .await
        .expect("first abandonment");
    super::abandon_agent_session(session_id)
        .await
        .expect("idempotent second abandonment");

    assert!(!crate::agent_sessions::has_journal(session_id));
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
    let store = crate::stdlib::session_store::open_canonical_agent_session(
        &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
        session_id,
        None,
        harn_session_store::SessionType::User,
    )
    .await
    .expect("open canonical session");
    let events = crate::stdlib::session_store::read_all_events(&store, session_id)
        .await
        .expect("read canonical events");
    assert_eq!(events.len(), 2);
    let payloads = events
        .iter()
        .map(|event| event.payload.to_string())
        .collect::<Vec<_>>();
    assert!(payloads[0].contains("persist before cancellation"));
    assert!(payloads[1].contains("agent_run_terminal"));
    assert_eq!(
        events[1].payload["transcript_event"]["metadata"]["provider_call_count"],
        1
    );
    crate::agent_sessions::reset_session_store();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_cancel_terminal_append_keeps_every_owner_visible() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "cancel-terminal-pending";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-cancel-pending".to_string(),
        "turn-cancel-pending".to_string(),
    )
    .await
    .expect("prepare journal");
    let store = prepared.state.store();
    crate::agent_sessions::open_or_create(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::agent_sessions::claim_journal_task(session_id, "task-pending".to_string(), true)
        .expect("claim journal task");
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
    super::super::record_provider_dispatch(session_id);
    store
        .close(session_id)
        .await
        .expect("close canonical session to inject append failure");

    let error = super::abandon_agent_session(session_id)
        .await
        .expect_err("cancel terminal append must report failure");
    assert!(error.to_string().contains("closed"));
    let retry_error = super::abandon_agent_session(session_id)
        .await
        .expect_err("retry against the closed store must still report failure");
    assert!(retry_error.to_string().contains("closed"));
    assert!(crate::agent_sessions::has_journal(session_id));
    assert!(super::super::AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow().contains_key(session_id)));
    let (_, pending) = crate::agent_sessions::next_journal_event(session_id)
        .expect("inspect pending terminal")
        .expect("terminal remains queued");
    assert_eq!(
        pending.payload["transcript_event"]["metadata"]["provider_call_count"],
        1
    );
    let terminal_count = crate::agent_sessions::transcript(session_id)
        .and_then(|transcript| transcript.as_dict().cloned())
        .and_then(|transcript| transcript.get("events").cloned())
        .and_then(|events| match events {
            crate::value::VmValue::List(events) => Some(
                events
                    .iter()
                    .filter(|event| {
                        event
                            .as_dict()
                            .and_then(|event| event.get("kind"))
                            .is_some_and(|kind| kind.display() == "agent_run_terminal")
                    })
                    .count(),
            ),
            _ => None,
        });
    assert_eq!(
        terminal_count,
        Some(1),
        "a failed terminal retry must flush the queued boundary, not append another"
    );
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[tokio::test(flavor = "current_thread")]
async fn detached_task_abort_terminalizes_the_claimed_session() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "detached-task-abort";
    let task_id = "detached-runtime-task";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-detached-abort".to_string(),
        "turn-detached-abort".to_string(),
    )
    .await
    .expect("prepare journal");
    crate::agent_sessions::open_or_create(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::agent_sessions::claim_journal_task(session_id, task_id.to_string(), true)
        .expect("claim journal task");
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");

    let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = tokio::spawn(async {
        std::future::pending::<Result<(crate::value::VmValue, String), crate::value::VmError>>()
            .await
    });
    crate::vm::ops::abort_task_detached(
        crate::stdlib::pool::new_pool_registry(),
        crate::value::VmTaskHandle {
            handle,
            cancel_token: cancel_token.clone(),
            wait_task_id: task_id.to_string(),
        },
    );

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while crate::agent_sessions::has_journal(session_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached cleanup must finish");
    assert!(cancel_token.load(Ordering::SeqCst));
    assert!(!crate::agent_sessions::exists(session_id));
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[test]
fn failed_finalization_restores_the_exact_provider_ledger() {
    let session_id = "finalization-restore";
    super::super::reset_agent_session_host_state();
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
    super::super::record_provider_dispatch(session_id);

    let finalization =
        super::AgentSessionFinalization::take(session_id).expect("take finalization owner");
    assert!(super::super::AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow().get(session_id).is_none()));
    drop(finalization);

    let count = super::super::AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(session_id)
            .map(|session| session.provider_call_count)
    });
    assert_eq!(count, Some(1));
    super::super::reset_agent_session_host_state();
}

#[test]
fn cancelled_nested_guard_does_not_pop_callers_policy() {
    use crate::orchestration::{
        clear_execution_policy_stacks, current_execution_policy, enter_nested_execution_policy,
        pop_execution_policy, push_execution_policy, swap_execution_policy_stack, CapabilityPolicy,
        NestedExecutionKind,
    };

    clear_execution_policy_stacks();
    push_execution_policy(CapabilityPolicy {
        recursion_limit: Some(4),
        ..Default::default()
    });
    let nested =
        enter_nested_execution_policy(None, NestedExecutionKind::AgentLoop, "cancelled-session")
            .expect("enter nested policy");
    let abandoned_stack = swap_execution_policy_stack(Vec::new());
    push_execution_policy(CapabilityPolicy {
        recursion_limit: Some(99),
        ..Default::default()
    });

    drop(super::CancelSafeNestedExecutionGuard::new(nested));
    assert_eq!(
        current_execution_policy().and_then(|policy| policy.recursion_limit),
        Some(99),
        "dropping a cancelled session must not pop the caller's unrelated policy"
    );

    pop_execution_policy();
    drop(abandoned_stack);
    clear_execution_policy_stacks();
}
