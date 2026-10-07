use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Count(Arc<AtomicUsize>);
impl AgentEventSink for Count {
    fn handle_event(&self, _: &AgentEvent) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn empty_transport_and_nested_loop_observers_do_not_inherit_or_replace_transport() {
    let session = format!("transport-nested-{}", uuid::Uuid::now_v7());
    let count = Arc::new(AtomicUsize::new(0));
    let bound = AgentEventTransport::new(Arc::new(Count(count.clone())));
    let observer = Arc::new(AtomicUsize::new(0));
    let wildcard = Arc::new(AtomicUsize::new(0));
    let handle = crate::agent_events::register_wildcard_sink(Arc::new(Count(wildcard.clone())));
    let event = AgentEvent::IterationStart {
        session_id: session,
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    };
    bound
        .scope(async {
            crate::llm::scope_agent_event_sink(Some(Arc::new(Count(observer.clone()))), async {
                crate::llm::emit_live_agent_event_sync(&event);
                AgentEventTransport::default()
                    .scope(async {
                        crate::llm::emit_live_agent_event_sync(&event);
                        tokio::task::yield_now().await;
                        crate::llm::emit_live_agent_event_sync(&event);
                    })
                    .await;
                crate::llm::emit_live_agent_event_sync(&event);
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
