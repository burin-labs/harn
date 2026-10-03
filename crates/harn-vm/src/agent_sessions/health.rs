use crate::agent_events::AgentEvent;
use harn_clock::Clock;
use std::sync::OnceLock;

/// Observe an event and snapshot the callbacks for its derived facts together.
pub(crate) fn observe_event(
    event: &AgentEvent,
) -> (Option<AgentEvent>, Vec<super::SessionSubscriber>) {
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
        let health = state
            .health
            .observe(event, now_ms)
            .map(|fact| AgentEvent::SessionHealth {
                session_id: event.session_id().to_owned(),
                fact: Box::new(fact),
            });
        (health, state.subscribers.clone())
    })
}
