use crate::agent_events::AgentEvent;
use harn_clock::Clock;
use std::sync::OnceLock;

/// Observe an event and snapshot the callbacks for its derived facts together.
pub(crate) fn observe_event(
    event: &AgentEvent,
    include_subscribers: bool,
) -> (Option<Box<AgentEvent>>, Vec<super::SessionSubscriber>) {
    let measures_health = crate::agent_events::session_health::SessionHealth::observes(event);
    // Synchronous emission never invokes VM-bound closure subscribers. Avoid
    // the store entirely for notifications that cannot change health, including
    // compaction emitted while a transcript mutation owns the store lock.
    if !measures_health && !include_subscribers {
        return (None, Vec::new());
    }
    // Read the clock without recording a script-visible tape read, and without
    // reaching up into stdlib from the session layer.
    static CLOCK: OnceLock<harn_clock::RealClock> = OnceLock::new();
    let now_ms = crate::clock_mock::active_clock()
        .map(|clock| clock.monotonic_ms())
        .unwrap_or_else(|| {
            CLOCK
                .get_or_init(harn_clock::RealClock::default)
                .monotonic_ms()
        });
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(event.session_id()) else {
            return (None, Vec::new());
        };
        let health = measures_health
            .then(|| state.health.observe(event, now_ms))
            .flatten()
            .map(|fact| {
                Box::new(AgentEvent::SessionHealth {
                    session_id: event.session_id().to_owned(),
                    fact: Box::new(fact),
                })
            });
        let subscribers = if include_subscribers {
            state.subscribers.clone()
        } else {
            Vec::new()
        };
        (health, subscribers)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_events::{AgentEventSink, ToolCallStatus};
    use crate::llm::{
        emit_live_agent_event_sync as emit_agent_event_sync,
        emit_live_agent_event_with_ctx as emit_agent_event_with_ctx, fire_session_end_hooks,
        scope_agent_event_sink,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<AgentEvent>>);

    impl AgentEventSink for RecordingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[tokio::test]
    async fn synchronous_abandoned_calls_reach_the_session_health_owner() {
        let session_id = "health-sync-abandoned-owner";
        super::super::open_session(Some(session_id.to_owned()), None, None).unwrap();
        let sink = Arc::new(RecordingSink::default());
        scope_agent_event_sink(Some(sink.clone()), async {
            for id in ["first", "second"] {
                emit_agent_event_sync(&AgentEvent::ToolCall {
                    session_id: session_id.to_owned(),
                    tool_call_id: id.to_owned(),
                    tool_name: "verify".to_owned(),
                    kind: None,
                    status: ToolCallStatus::Pending,
                    raw_input: serde_json::json!({}),
                    parsing: None,
                    audit: None,
                });
            }
            emit_agent_event_with_ctx(
                None,
                &AgentEvent::IterationEnd {
                    session_id: session_id.to_owned(),
                    iteration: 1,
                    iteration_info: serde_json::json!({}),
                },
            )
            .await;
            fire_session_end_hooks(session_id, true);
        })
        .await;
        let events = sink.0.lock().unwrap();
        let fact = events
            .iter()
            .rev()
            .find_map(|event| match event {
                AgentEvent::SessionHealth { fact, .. } => Some(fact),
                _ => None,
            })
            .expect("closeout must publish measured health");
        let rate = fact
            .rolling
            .tool_call_success_rate
            .as_ref()
            .expect("both abandoned calls must be measured");
        assert_eq!((rate.numerator, rate.denominator), (0, 2));
        assert!(fact.rolling.nonzero_command_exit_rate.is_none());
        super::super::close(session_id);
    }
}
