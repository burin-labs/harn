use super::*;
use crate::agent_events::{emit_event, AgentEvent, AgentEventSink, AgentEventTransport};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Count(Arc<AtomicUsize>);
impl AgentEventSink for Count {
    fn handle_event(&self, _: &AgentEvent) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn event(session: &str) -> AgentEvent {
    AgentEvent::IterationStart {
        session_id: session.into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    }
}

fn runtimes() -> CleanupRuntimes {
    CleanupRuntimes::new(
        "transport-restoration".into(),
        crate::agent_sessions::active_session_runtime(),
        crate::llm::agent_session_host::active_agent_host_session_runtime(),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_restores_transport_on_pending_ready_and_explicit_empty_binding() {
    let session = format!("cleanup-poll-{}", uuid::Uuid::now_v7());
    let old_count = Arc::new(AtomicUsize::new(0));
    let new_count = Arc::new(AtomicUsize::new(0));
    let durable = Arc::new(AtomicUsize::new(0));
    crate::agent_events::register_sink(session.clone(), Arc::new(Count(durable.clone())));
    let old = AgentEventTransport::new(Arc::new(Count(old_count.clone())));
    let new = AgentEventTransport::new(Arc::new(Count(new_count.clone())));
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut cleanup = Box::pin(ScopedCleanup {
        runtimes: runtimes().with_event_transport(old),
        inner: async {
            emit_event(&event(&session));
            wait.await.unwrap();
            emit_event(&event(&session));
        },
    });
    new.scope(async {
        std::future::poll_fn(|context| {
            assert!(cleanup.as_mut().poll(context).is_pending());
            emit_event(&event(&session));
            Poll::Ready(())
        })
        .await;
        release.send(()).unwrap();
        cleanup.await;
        emit_event(&event(&session));
        ScopedCleanup {
            runtimes: runtimes(),
            inner: async {
                emit_event(&event(&session));
            },
        }
        .await;
        emit_event(&event(&session));
    })
    .await;
    crate::agent_events::clear_session_sinks(&session);
    assert_eq!(old_count.load(Ordering::SeqCst), 2);
    assert_eq!(new_count.load(Ordering::SeqCst), 3);
    assert_eq!(
        durable.load(Ordering::SeqCst),
        6,
        "empty cleanup preserves durable delivery"
    );
}
