use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::agent_events::{emit_event, register_sink, AgentEventSink};
use crate::value::VmDictExt;

struct Count(Arc<AtomicUsize>);
impl AgentEventSink for Count {
    fn handle_event(&self, _: &AgentEvent) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn transport() -> (AgentEventTransport, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    (
        AgentEventTransport::new(Arc::new(Count(count.clone()))),
        count,
    )
}

fn event(session: &str) -> AgentEvent {
    AgentEvent::IterationStart {
        session_id: session.into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn captured_child_scope_retains_transport_after_same_session_rebind() {
    let session = format!("transport-child-{}", uuid::Uuid::now_v7());
    let (old, old_count) = transport();
    let (new, new_count) = transport();
    let durable = Arc::new(AtomicUsize::new(0));
    register_sink(session.clone(), Arc::new(Count(durable.clone())));
    let old_scope = old
        .scope(async { crate::orchestration::AmbientExecutionScope::capture_inherited() })
        .await;
    new.scope(async {
        emit_event(&event(&session));
        crate::orchestration::scope_ambient(old_scope, async {
            emit_event(&event(&session));
            tokio::task::yield_now().await;
            emit_event(&event(&session));
        })
        .await;
        emit_event(&event(&session));
    })
    .await;
    crate::agent_events::clear_session_sinks(&session);
    assert_eq!(
        (
            old_count.load(Ordering::SeqCst),
            new_count.load(Ordering::SeqCst)
        ),
        (2, 2)
    );
    assert_eq!(durable.load(Ordering::SeqCst), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn empty_transport_and_nested_loop_observers_do_not_inherit_or_replace_transport() {
    let session = format!("transport-nested-{}", uuid::Uuid::now_v7());
    let (bound, count) = transport();
    let observer = Arc::new(AtomicUsize::new(0));
    let wildcard = Arc::new(AtomicUsize::new(0));
    let handle = crate::agent_events::register_wildcard_sink(Arc::new(Count(wildcard.clone())));
    bound
        .scope(async {
            crate::llm::scope_agent_event_sink(Some(Arc::new(Count(observer.clone()))), async {
                crate::llm::emit_live_agent_event_sync(&event(&session));
                AgentEventTransport::default()
                    .scope(async {
                        crate::llm::emit_live_agent_event_sync(&event(&session));
                        tokio::task::yield_now().await;
                        crate::llm::emit_live_agent_event_sync(&event(&session));
                    })
                    .await;
                crate::llm::emit_live_agent_event_sync(&event(&session));
            })
            .await;
        })
        .await;
    crate::agent_events::unregister_wildcard_sink(handle);
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "empty binding cannot borrow outer/current transport"
    );
    assert_eq!(
        observer.load(Ordering::SeqCst),
        4,
        "top loop observer still receives its complete stream"
    );
    assert_eq!(
        wildcard.load(Ordering::SeqCst),
        4,
        "global observation is unchanged"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn top_level_vm_drop_keeps_origin_after_execution_scope_has_unwound() {
    let root = tempfile::tempdir().unwrap();
    let session = format!("transport-drop-{}", uuid::Uuid::now_v7());
    let (old, old_count) = transport();
    let (new, new_count) = transport();
    let vm = old
        .scope(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let chunk = crate::compile_source("pipeline main() {}").unwrap();
            vm.execute(&chunk).await.unwrap();
            vm
        })
        .await;
    let old_before_cleanup = old_count.load(Ordering::SeqCst);
    let mut options = crate::value::DictMap::new();
    options.put_str("root", root.path().to_string_lossy().as_ref());
    let prepared = crate::agent_session_journal::prepare(
        &session,
        &options,
        format!("run-{session}"),
        format!("turn-{session}"),
    )
    .await
    .unwrap();
    crate::agent_sessions::open_or_create_for_test(Some(session.clone()));
    crate::agent_sessions::install_journal(&session, prepared.state).unwrap();
    crate::agent_sessions::claim_journal_task(
        &session,
        vm.execution_id(),
        "task_root".into(),
        true,
    )
    .unwrap();
    crate::llm::agent_session_host::seed_host_session_provider_model(&session, "mock", "fixture");
    let durable = Arc::new(AtomicUsize::new(0));
    register_sink(session.clone(), Arc::new(Count(durable.clone())));
    assert_eq!(
        crate::agent_events::session_external_sink_count(&session),
        1,
        "the observer census reaches a known nonzero registration"
    );
    let mut progress = crate::agent_lifecycle_cleanup::subscribe_cleanup_progress();
    let before = crate::vm::subtask::lifecycle_cleanup_spawn_count();
    new.scope(async {
        emit_event(&event(&session));
        // Drop happens under NEW ambient ownership, after OLD execution unwound.
        drop(vm);
        assert!(crate::vm::subtask::lifecycle_cleanup_spawn_count() > before);
        crate::agent_lifecycle_cleanup::settle_cleanup(
            &mut progress,
            || {
                !crate::agent_sessions::has_journal(&session)
                    && !crate::agent_sessions::exists(&session)
                    && old_count.load(Ordering::SeqCst) > old_before_cleanup
            },
            "originating top-level cleanup must emit and release both session owners",
        )
        .await;
        let cleanup_events = old_count.load(Ordering::SeqCst) - old_before_cleanup;
        assert!(
            cleanup_events > 0,
            "actual terminal cleanup reaches OLD transport"
        );
        let observed_at_close = durable.load(Ordering::SeqCst);
        assert_eq!(
            observed_at_close,
            cleanup_events + 1,
            "session observer receives every cleanup event and the pre-close current anchor"
        );
        assert_eq!(
            crate::agent_events::session_external_sink_count(&session),
            0,
            "settled session close releases its dynamic observers"
        );
        // This post-close positive proves the caller's NEW transport was
        // restored; a closed session no longer owns a registry subscription.
        emit_event(&event(&session));
        assert_eq!(
            durable.load(Ordering::SeqCst),
            observed_at_close,
            "post-close transport observation cannot revive session subscribers"
        );
    })
    .await;
    crate::agent_events::clear_session_sinks(&session);
    assert_eq!(
        new_count.load(Ordering::SeqCst),
        2,
        "cleanup never uses NEW transport"
    );
    // The Count fixture measures observation, not durability. Read the owning
    // journal independently after close to prove its terminal really committed.
    let store = crate::stdlib::session_store::open_canonical_agent_session(
        &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
        &session,
        None,
        harn_session_store::SessionType::User,
    )
    .await
    .expect("open canonical session after cleanup");
    let events = crate::stdlib::session_store::read_all_events(&store, &session)
        .await
        .expect("read committed journal after cleanup");
    let terminals: Vec<_> = events
        .iter()
        .filter(|event| {
            event
                .payload
                .pointer("/transcript_event/kind")
                .and_then(serde_json::Value::as_str)
                == Some("agent_run_terminal")
        })
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "cleanup commits exactly one durable terminal"
    );
    let metadata = terminals[0]
        .payload
        .pointer("/transcript_event/metadata")
        .unwrap();
    assert_eq!(metadata["final_status"], "cancelled");
    assert_eq!(metadata["stop_reason"], "cancelled");
    let phase = crate::agent_events::AgentTurnPhase::from_terminal_record(metadata)
        .expect("persisted terminal decodes through owning typed projection");
    assert!(
        matches!(phase, crate::agent_events::AgentTurnPhase::Terminal { outcome, .. }
        if outcome.kind == crate::agent_events::AgentTerminalKind::UserCancelled)
    );
}
