//! Session input custody follows the existing isolated session runtime.
//! Private values never enter the transcript or the durable session schema.

use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
mod tests;

pub use harn_execution_policy::session_inputs::{
    InputDescription, InputError, InputKind, InputLifetime, InputSpec, InputUsage, InputUse,
    PrivateInputSnapshot, SessionInputs,
};

pub type Store = Arc<parking_lot::Mutex<SessionInputs>>;

pub fn bind(session_id: &str, store: Store) {
    crate::agent_sessions::active_session_runtime().bind_inputs(session_id, store);
}

fn store(session_id: &str, create: bool) -> Result<Store, InputError> {
    crate::agent_sessions::active_session_runtime().input_store(session_id, create)
}

pub fn put(
    session_id: &str,
    spec: InputSpec,
    value: String,
    replace: bool,
    now_ms: u64,
) -> Result<InputDescription, InputError> {
    store(session_id, true)?
        .lock()
        .put(spec, value, replace, now_ms)
}

pub fn list(session_id: &str, now_ms: u64) -> Result<Vec<InputDescription>, InputError> {
    match store(session_id, false) {
        Ok(store) => Ok(store.lock().list(now_ms)),
        Err(_) if crate::agent_sessions::exists(session_id) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub fn remove(session_id: &str, name: &str) -> bool {
    store(session_id, false).is_ok_and(|store| store.lock().remove(name))
}

pub fn now_ms() -> u64 {
    u64::try_from(crate::stdlib::clock::now_wall_ms_unrecorded()).unwrap_or_default()
}

fn current_store() -> Result<Store, InputError> {
    let id = crate::current_agent_session_id().ok_or(InputError::Unavailable)?;
    store(&id, false)
}

/// The filter for output produced while no agent session is current, such as a
/// plain `harn run --grant`. It holds only values added by
/// [`filter_granted_value`] and lasts for the process.
fn process_store() -> Store {
    static PROCESS: std::sync::OnceLock<Store> = std::sync::OnceLock::new();
    PROCESS.get_or_init(Store::default).clone()
}

/// The filter that applies to output produced here: the current session's
/// store when one is current (created on demand with `create`), otherwise the
/// process store.
fn filter_store(create: bool) -> Option<Store> {
    match crate::current_agent_session_id() {
        Some(id) => store(&id, create).ok(),
        None => Some(process_store()),
    }
}

pub(crate) fn has_private_filter() -> bool {
    filter_store(false).is_some_and(|store| store.lock().has_secret_filters())
}

/// Below this a value is too likely to occur in unrelated output.
const MIN_FILTERED_VALUE_LEN: usize = 8;

/// Filter a credential a grant is about to hand a child from this session's
/// output, or from the process's output when no session is current.
///
/// Value matching is best-effort cleanup of accidents, not a security boundary:
/// a child holding a value can encode or split it past any filter, and what holds
/// a secret back is not handing it over. Returns `false` only when the filter is
/// full, so the caller must not expose the value.
#[must_use]
pub fn filter_granted_value(value: &str) -> bool {
    if value.len() < MIN_FILTERED_VALUE_LEN {
        return true;
    }
    filter_store(true).map_or(true, |store| store.lock().add_filter(value))
}

/// Retain the filter for one operation, including output received after close.
/// Dropping the operation releases this reference to the private material.
pub struct OutputFilter(Option<Store>);

impl OutputFilter {
    pub fn capture() -> Self {
        Self(filter_store(false))
    }

    /// Capture for a child about to spawn, whose grants may be the first thing
    /// this session has to filter. Never call this under the session lock.
    pub fn capture_for_spawn() -> Self {
        Self(filter_store(true))
    }

    pub fn redact(&self, text: &str) -> String {
        match &self.0 {
            Some(store) => store.lock().redact(text),
            None => text.to_owned(),
        }
    }

    pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        let stored = self.0.as_ref().and_then(|store| {
            let store = store.lock();
            store
                .has_secret_filters()
                .then(|| store.redact_bytes(bytes))
                .filter(|redacted| redacted != bytes)
        });
        match stored {
            Some(redacted) => std::borrow::Cow::Owned(redacted),
            None => std::borrow::Cow::Borrowed(bytes),
        }
    }
}

/// Resolve only explicitly named bindings for the canonical process owner.
pub fn command_environment(
    bindings: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, InputError> {
    if bindings.is_empty() {
        return Ok(BTreeMap::new());
    }
    let store = current_store()?;
    let mut store = store.lock();
    let mut environment = BTreeMap::new();
    for (target, source) in bindings {
        InputSpec {
            name: target.clone(),
            kind: InputKind::Variable,
            lifetime: InputLifetime::Session,
            usage: InputUsage::SessionTools,
        }
        .validate(now_ms())?;
        let value = store.with_value(&source, InputUse::Command, now_ms(), str::to_owned)?;
        environment.insert(target, value);
    }
    Ok(environment)
}

pub(crate) fn http_header(
    name: &str,
    usage: InputUse<'_>,
    prefix: &str,
) -> Result<String, InputError> {
    current_store()?
        .lock()
        .with_value(name, usage, now_ms(), |value| format!("{prefix}{value}"))
}

/// Exact-value filtering complements the shared pattern policy. A session with
/// no store is a real empty set, not an ambient credential fallback; with no
/// session, the process store holds only credentials a grant handed a child.
pub fn redact(text: &str) -> String {
    OutputFilter::capture().redact(text)
}
