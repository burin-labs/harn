use crate::agent_events::AgentEvent;

/// Observe an event and snapshot the callbacks for its derived facts together.
pub(crate) fn observe_event(
    event: &AgentEvent,
) -> (Option<AgentEvent>, Vec<super::SessionSubscriber>) {
    // Health timing is runtime bookkeeping, not a script-visible clock read.
    let now_ms = crate::stdlib::clock::now_monotonic_ms_unrecorded();
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(state) = sessions.get_mut(event.session_id()) else {
            return (None, Vec::new());
        };
        let health = state
            .health
            .observe(event, now_ms)
            .map(|fact| AgentEvent::SessionHealth {
                session_id: event.session_id().to_owned(),
                fact,
            });
        (health, state.subscribers.clone())
    })
}
