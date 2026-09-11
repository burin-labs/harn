//! Session-owned callbacks retain the execution policy of their registration.

use super::{open_or_create, SessionOpenError, SESSIONS};
use crate::orchestration::RegisteredExecutionPolicy;
use crate::value::VmValue;

/// A session callback and the execution policy that authorized its registration.
#[derive(Clone)]
pub struct SessionSubscriber {
    pub(crate) callback: VmValue,
    pub(crate) execution_policy: RegisteredExecutionPolicy,
}

pub fn append_subscriber(id: &str, callback: VmValue) -> Result<(), SessionOpenError> {
    open_or_create(Some(id.to_string()))?;
    let subscriber = SessionSubscriber {
        callback,
        execution_policy: RegisteredExecutionPolicy::capture(),
    };
    SESSIONS.with(|sessions| {
        if let Some(state) = sessions.borrow_mut().get_mut(id) {
            state.subscribers.push(subscriber);
            state.touch();
        }
    });
    Ok(())
}

pub fn subscribers_for(id: &str) -> Vec<VmValue> {
    registered_subscribers_for(id)
        .into_iter()
        .map(|subscriber| subscriber.callback)
        .collect()
}

pub(crate) fn registered_subscribers_for(id: &str) -> Vec<SessionSubscriber> {
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .map(|state| state.subscribers.clone())
            .unwrap_or_default()
    })
}

pub fn subscriber_count(id: &str) -> usize {
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .map(|state| state.subscribers.len())
            .unwrap_or(0)
    })
}
