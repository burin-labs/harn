use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use harn_session_store::SessionStore;
use serde_json::json;

use crate::value::VmDictExt;

/// Block the calling thread until detached recovery settles.
///
/// The source runtime that scheduled recovery is already gone, so the wait
/// needs a runtime of its own. It is driven by the cleanup progress signal
/// subscribed before the triggering drop, and its bound fails the test rather
/// than passing it when recovery never runs.
fn settle_detached_cleanup(
    mut progress: tokio::sync::watch::Receiver<u64>,
    settled: impl FnMut() -> bool,
    what: &str,
) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("cleanup verification runtime")
        .block_on(crate::agent_lifecycle_cleanup::settle_cleanup(
            &mut progress,
            settled,
            what,
        ));
}

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
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
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
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
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
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::agent_sessions::claim_journal_task(
        session_id,
        "test-execution",
        "task-pending".to_string(),
        true,
    )
    .expect("claim journal task");
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
    super::super::record_provider_dispatch(session_id);
    assert!(
        !crate::agent_sessions::close(session_id),
        "generic close must not discard an active lifecycle owner"
    );
    let close_error = crate::agent_sessions::close_with_status(
        session_id,
        "user_requested",
        "cancelled",
        serde_json::Value::Null,
    )
    .expect_err("explicit close must refuse an active durable run");
    assert!(close_error.contains("finalize it before closing"));
    assert!(crate::agent_sessions::has_journal(session_id));
    assert!(super::super::AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow().contains_key(session_id)));
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

#[test]
fn detached_task_abort_survives_the_source_runtime_shutdown() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "detached-task-abort";
    let task_id = "detached-runtime-task";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("source runtime");
    source_runtime.block_on(async {
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-detached-abort".to_string(),
            "turn-detached-abort".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        crate::agent_sessions::claim_journal_task(
            session_id,
            "detached-execution",
            task_id.to_string(),
            true,
        )
        .expect("claim journal task");
        super::super::seed_host_session_provider_model(session_id, "mock", "fixture");

        let handle = tokio::spawn(async {
            std::future::pending::<Result<(crate::value::VmValue, String), crate::value::VmError>>()
                .await
        });
        crate::vm::ops::abort_task_detached(
            crate::value::VmTaskHandle {
                handle,
                cancel_token: cancel_token.clone(),
                wait_task_id: task_id.to_string(),
            },
            crate::agent_lifecycle_cleanup::CleanupRuntimes::new(
                "detached-execution".to_string(),
                crate::agent_sessions::active_session_runtime(),
                super::super::active_agent_host_session_runtime(),
            ),
        );
    });
    drop(source_runtime);

    settle_detached_cleanup(
        cleanup_progress,
        || {
            !crate::agent_sessions::has_journal(session_id)
                && !crate::agent_sessions::exists(session_id)
        },
        "process-owned cleanup must outlive the source runtime",
    );
    assert!(cancel_token.load(Ordering::SeqCst));
    assert!(!crate::agent_sessions::has_journal(session_id));
    assert!(!crate::agent_sessions::exists(session_id));
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[test]
fn dropping_a_top_level_vm_terminalizes_its_own_agent_lifecycle() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "dropped-top-level-vm";
    let task_id = "task_root";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("source runtime");
    source_runtime.block_on(async {
        let vm = crate::vm::Vm::new();
        let execution_id = vm.execution_id().to_string();
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-dropped-top-level".to_string(),
            "turn-dropped-top-level".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        crate::agent_sessions::claim_journal_task(
            session_id,
            &execution_id,
            task_id.to_string(),
            true,
        )
        .expect("claim top-level lifecycle");
        super::super::seed_host_session_provider_model(session_id, "mock", "fixture");

        // Positive control for the counter the inline case reads. A per-thread
        // count that never moves would let that case pass without proving
        // anything, so the drop that is supposed to transfer recovery has to
        // move it here.
        let spawns_before = crate::vm::subtask::lifecycle_cleanup_spawn_count();
        drop(vm);
        assert!(
            crate::vm::subtask::lifecycle_cleanup_spawn_count() > spawns_before,
            "dropping a top-level VM must schedule recovery on this thread",
        );
    });
    drop(source_runtime);

    settle_detached_cleanup(
        cleanup_progress,
        || {
            !crate::agent_sessions::has_journal(session_id)
                && !crate::agent_sessions::exists(session_id)
        },
        "top-level cleanup must transfer to the process-owned runtime",
    );
    assert!(!crate::agent_sessions::has_journal(session_id));
    assert!(!crate::agent_sessions::exists(session_id));
    let verification_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("verification runtime");
    let terminal_count = verification_runtime.block_on(async {
        let store = crate::stdlib::session_store::open_canonical_agent_session(
            &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
            session_id,
            None,
            harn_session_store::SessionType::User,
        )
        .await
        .expect("open canonical session");
        crate::stdlib::session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events")
            .into_iter()
            .filter(|event| event.payload.to_string().contains("agent_run_terminal"))
            .count()
    });
    assert_eq!(terminal_count, 1, "drop must persist one terminal boundary");
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[test]
fn dropping_an_inline_vm_does_not_cancel_the_parent_lifecycle() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "inline-vm-parent-lifecycle";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("source runtime");
    source_runtime.block_on(async {
        let parent = crate::vm::Vm::new();
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-inline-parent".to_string(),
            "turn-inline-parent".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        crate::agent_sessions::claim_journal_task(
            session_id,
            parent.execution_id(),
            "task_root".to_string(),
            true,
        )
        .expect("claim parent lifecycle");

        // Whether a drop transfers recovery is decided synchronously inside
        // `cancel_spawned_tasks`, so the claim is "this drop scheduled
        // nothing", not "nothing had happened yet". Counting the transfers
        // states that exactly; waiting for an interval that elapsed cannot,
        // because an interval always elapses.
        let spawns_before = crate::vm::subtask::lifecycle_cleanup_spawn_count();
        let inline = parent.child_vm_inline();
        drop(inline);
        assert_eq!(
            crate::vm::subtask::lifecycle_cleanup_spawn_count(),
            spawns_before,
            "a transient inline execution context must not schedule recovery for its parent"
        );
        assert!(
            crate::agent_sessions::has_journal(session_id),
            "a transient inline execution context must not activate its parent's reservation"
        );

        crate::agent_sessions::clear_journal(session_id);
        crate::agent_sessions::close(session_id);
        drop(parent);
    });
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[test]
fn dropping_one_execution_does_not_cancel_same_task_in_shared_runtimes() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("source runtime");
    source_runtime.block_on(async {
        let first = crate::vm::Vm::new();
        let second = crate::vm::Vm::new();
        for (session_id, run_id, execution_id) in [
            ("shared-runtime-first", "run-first", first.execution_id()),
            ("shared-runtime-second", "run-second", second.execution_id()),
        ] {
            let mut options = crate::value::DictMap::new();
            options.put_str("root", root.path().to_string_lossy().as_ref());
            let prepared = crate::agent_session_journal::prepare(
                session_id,
                &options,
                run_id.to_string(),
                format!("turn-{run_id}"),
            )
            .await
            .expect("prepare journal");
            crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
            crate::agent_sessions::install_journal(session_id, prepared.state)
                .expect("install journal");
            crate::agent_sessions::claim_journal_task(
                session_id,
                execution_id,
                "task_root".to_string(),
                true,
            )
            .expect("claim lifecycle");
            super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
        }

        let mut cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
        drop(first);
        crate::agent_lifecycle_cleanup::settle_cleanup(
            &mut cleanup_progress,
            || !crate::agent_sessions::exists("shared-runtime-first"),
            "dropping the first execution must release its own session",
        )
        .await;
        assert!(
            crate::agent_sessions::has_journal("shared-runtime-second"),
            "cleanup must retain an unrelated execution with the same task id and runtimes"
        );

        drop(second);
    });
    drop(source_runtime);
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[test]
fn pending_cleanup_keeps_the_execution_that_owned_the_cancelled_task() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "pending-cleanup-original-execution";
    let task_id = "task_cancelled_before_execution_reuse";
    let cleanup_progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("source runtime");
    source_runtime.block_on(async {
        let mut vm = crate::vm::Vm::new();
        let original_execution_id = vm.execution_id().to_string();
        let mut options = crate::value::DictMap::new();
        options.put_str("root", root.path().to_string_lossy().as_ref());
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-pending-cleanup".to_string(),
            "turn-pending-cleanup".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        crate::agent_sessions::claim_journal_task(
            session_id,
            &original_execution_id,
            task_id.to_string(),
            true,
        )
        .expect("claim lifecycle");
        super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
        vm.pending_task_cleanups.insert(
            "public-task".to_string(),
            crate::vm::PendingTaskCleanup {
                execution_id: original_execution_id.clone(),
                task_id: task_id.to_string(),
            },
        );

        vm.prepare_execution_for_top_level();
        assert_ne!(vm.execution_id().to_string(), original_execution_id);
        drop(vm);
    });
    drop(source_runtime);

    settle_detached_cleanup(
        cleanup_progress,
        || !crate::agent_sessions::exists(session_id),
        "cleanup must retain the cancelled task's original execution identity",
    );
    assert!(!crate::agent_sessions::exists(session_id));
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[tokio::test(flavor = "current_thread")]
async fn persisted_terminal_seals_the_live_session_against_late_mutation() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "sealed-terminal";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        "run-sealed-terminal".to_string(),
        "turn-sealed-terminal".to_string(),
    )
    .await
    .expect("prepare journal");
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    crate::agent_sessions::append_event(
        session_id,
        crate::stdlib::json_to_vm_value(&json!({
            "kind": crate::llm::helpers::SYSTEM_REMINDER_EVENT_KIND,
            "role": "system",
            "content": "",
        })),
    )
    .expect("seed malformed reminder before terminal");

    let terminal = crate::agent_events::AgentTerminalOutcome::new(
        crate::agent_events::classify_agent_terminal("completed", "end_turn", false, None),
        "end_turn",
    );
    super::super::live_transcript_journal::flush_terminal(
        session_id,
        "completed",
        "end_turn",
        None,
        None,
        &terminal,
        1,
    )
    .await
    .expect("persist terminal");

    let error = crate::agent_sessions::append_event(
        session_id,
        crate::stdlib::json_to_vm_value(&json!({
            "kind": "late_event",
            "role": "system",
        })),
    )
    .expect_err("terminal session must reject recap-racing mutations");
    assert!(error.contains("is terminal"));
    let prompt_error = crate::agent_sessions::record_system_prompt(session_id, "late prompt")
        .expect_err("terminal session must reject prompt metadata mutation");
    assert!(prompt_error.contains("is terminal"));
    assert_eq!(crate::agent_sessions::system_prompt(session_id), None);
    let sealed_transcript = crate::agent_sessions::transcript(session_id).expect("sealed session");
    assert!(!crate::agent_sessions::reset_transcript(session_id));
    assert_eq!(
        crate::agent_sessions::prune_invalid_reminder_events(session_id),
        0
    );
    assert!(crate::values_equal(
        &sealed_transcript,
        &crate::agent_sessions::transcript(session_id).expect("unchanged sealed session")
    ));
    assert!(
        crate::agent_sessions::next_journal_event(session_id)
            .expect("inspect journal")
            .is_none(),
        "terminal flush must leave no mutation that clear_journal could discard"
    );

    crate::agent_sessions::clear_journal(session_id);
    crate::agent_sessions::close(session_id);
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
}

#[tokio::test(flavor = "current_thread")]
async fn live_journal_makes_session_cap_a_hard_admission_ceiling() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    crate::agent_sessions::set_session_cap(1);
    let root = tempfile::tempdir().expect("temp root");
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());

    let pinned = "pinned-journal";
    let prepared = crate::agent_session_journal::prepare(
        pinned,
        &options,
        "run-pinned".to_string(),
        "turn-pinned".to_string(),
    )
    .await
    .expect("prepare pinned journal");
    crate::agent_sessions::open_or_create_for_test(Some(pinned.to_string()));
    crate::agent_sessions::install_journal(pinned, prepared.state).expect("install pinned journal");
    crate::agent_sessions::claim_journal_task(
        pinned,
        "capacity-execution",
        "task-pinned".to_string(),
        true,
    )
    .expect("first lifecycle fits the cap");

    let error = crate::agent_sessions::open_or_create(Some("second-session".to_string()))
        .expect_err("a live journal must not be evicted to admit another session");
    assert!(crate::agent_sessions::exists(pinned));
    assert!(!crate::agent_sessions::exists("second-session"));
    assert_eq!(
        error,
        crate::agent_sessions::SessionOpenError::CapacityExhausted {
            limit: 1,
            active: 1,
            protected: 1,
        }
    );
    assert_eq!(
        error.to_string(),
        "agent session capacity exhausted: limit=1 active=1 protected=1"
    );
    assert!(crate::agent_sessions::has_journal(pinned));

    crate::agent_sessions::clear_journal(pinned);
    crate::agent_sessions::close(pinned);
    crate::agent_sessions::set_session_cap(crate::agent_sessions::DEFAULT_SESSION_CAP);
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
fn finalization_retry_reuses_the_first_attempt_status_and_stage() {
    let session_id = "finalization-retry-receipt";
    super::super::reset_agent_session_host_state();
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");

    super::super::AGENT_HOST_SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let session = sessions.get_mut(session_id).expect("seeded session");
        let mut original = crate::value::DictMap::new();
        original.put_str("final_status", "failed");
        original.put_str("stop_reason", "error");
        original.insert(
            crate::value::intern_key("iterations"),
            crate::value::VmValue::Int(7),
        );
        let (first, prepared) = session.begin_finalization(original);
        assert_eq!(prepared, super::super::AgentFinalizationStage::Preparing);
        assert_eq!(
            first
                .get("final_status")
                .map(|value| value.display())
                .as_deref(),
            Some("failed")
        );
        session.advance_finalization_to(super::super::AgentFinalizationStage::EndHookCompleted);

        let mut reconstructed = crate::value::DictMap::new();
        reconstructed.put_str("final_status", "completed");
        let (retried, prepared) = session.begin_finalization(reconstructed);
        assert_eq!(
            prepared,
            super::super::AgentFinalizationStage::EndHookCompleted
        );
        assert_eq!(
            retried
                .get("final_status")
                .map(|value| value.display())
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            retried
                .get("stop_reason")
                .map(|value| value.display())
                .as_deref(),
            Some("error")
        );
        assert_eq!(
            retried
                .get("iterations")
                .map(|value| value.display())
                .as_deref(),
            Some("7")
        );
    });

    super::super::reset_agent_session_host_state();
}

#[test]
fn stale_finalization_retry_receipt_cannot_claim_a_successor_run() {
    let session_id = "stale-finalization-retry";
    super::super::reset_agent_session_host_state();
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");

    let error = match super::AgentSessionFinalization::take_retry(session_id, "prior-run") {
        Ok(_) => panic!("stale retry must not claim the live successor"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("retry receipt names run"));
    assert!(super::super::AGENT_HOST_SESSIONS
        .with(|sessions| sessions.borrow().contains_key(session_id)));

    super::super::reset_agent_session_host_state();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_terminal_flush_retains_completed_side_effect_stages_for_retry() {
    crate::agent_sessions::reset_session_store();
    super::super::reset_agent_session_host_state();
    let root = tempfile::tempdir().expect("temp root");
    let session_id = "finalization-stage-retry";
    let run_id = "run-finalization-stage-retry";
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        session_id,
        &options,
        run_id.to_string(),
        "turn-finalization-stage-retry".to_string(),
    )
    .await
    .expect("prepare journal");
    let store = prepared.state.store();
    crate::agent_sessions::open_or_create_for_test(Some(session_id.to_string()));
    crate::agent_sessions::install_journal(session_id, prepared.state).expect("install journal");
    super::super::seed_host_session_provider_model(session_id, "mock", "fixture");
    super::super::AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .borrow_mut()
            .get_mut(session_id)
            .expect("seeded host session")
            .run_id = run_id.to_string();
    });
    store
        .close(session_id)
        .await
        .expect("close canonical session to inject terminal flush failure");

    let status = crate::stdlib::json_to_vm_value(&json!({
        "final_status": "failed",
        "stop_reason": "error",
        "iterations": 3,
        "error": {"kind": "fixture_failure"},
    }));
    let error = super::super::lifecycle::host_agent_session_finalize(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        vec![
            crate::value::VmValue::String(arcstr::ArcStr::from(session_id)),
            status,
        ],
    )
    .await
    .expect_err("closed store must reject terminal persistence");
    assert!(error.to_string().contains("closed"));

    super::super::AGENT_HOST_SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let pending = sessions
            .get(session_id)
            .and_then(|session| session.pending_finalization.as_ref())
            .expect("failed finalization remains retryable");
        assert_eq!(
            pending.status.get("final_status").unwrap().display(),
            "failed"
        );
        assert_eq!(
            pending.stage,
            super::super::AgentFinalizationStage::PromptOutcomeProjected
        );
    });
    let terminal_errors = crate::agent_sessions::transcript(session_id)
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
                            .is_some_and(|kind| kind.display() == "agent_loop_terminal_error")
                    })
                    .count(),
            ),
            _ => None,
        })
        .unwrap_or(0);
    assert_eq!(terminal_errors, 1);

    let mut retry = crate::value::DictMap::new();
    retry.put_str("retry_run_id", run_id);
    let retry_error = super::super::lifecycle::host_agent_session_finalize(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        vec![
            crate::value::VmValue::String(arcstr::ArcStr::from(session_id)),
            crate::value::VmValue::dict(retry),
        ],
    )
    .await
    .expect_err("retry still sees the deliberately closed store");
    assert!(retry_error.to_string().contains("closed"));
    let terminal_errors_after_retry = crate::agent_sessions::transcript(session_id)
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
                            .is_some_and(|kind| kind.display() == "agent_loop_terminal_error")
                    })
                    .count(),
            ),
            _ => None,
        })
        .unwrap_or(0);
    assert_eq!(terminal_errors_after_retry, 1);

    crate::agent_sessions::reset_session_store();
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
