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
    crate::agent_sessions::active_session_runtime()
        .inputs
        .write()
        .insert(session_id.to_owned(), store);
}

fn store(session_id: &str, create: bool) -> Result<Store, InputError> {
    let runtime = crate::agent_sessions::active_session_runtime();
    if let Some(store) = runtime.inputs.read().get(session_id).cloned() {
        return Ok(store);
    }
    if !create || !crate::agent_sessions::exists(session_id) {
        return Err(InputError::Unavailable);
    }
    let store = runtime
        .inputs
        .write()
        .entry(session_id.to_owned())
        .or_default()
        .clone();
    Ok(store)
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

pub(crate) fn close(session_id: &str) {
    let runtime = crate::agent_sessions::active_session_runtime();
    if let Some(store) = runtime.inputs.write().remove(session_id) {
        store.lock().close();
    };
}

pub fn now_ms() -> u64 {
    u64::try_from(crate::stdlib::clock::now_wall_ms_unrecorded()).unwrap_or_default()
}

fn current_store() -> Result<Store, InputError> {
    let id = crate::current_agent_session_id().ok_or(InputError::Unavailable)?;
    store(&id, false)
}

pub(crate) fn has_private_filter() -> bool {
    current_store().is_ok_and(|store| store.lock().has_secret_filters())
}

/// Retain the filter for one operation, including output received after close.
/// Dropping the operation releases this reference to the private material.
pub struct OutputFilter(Option<Store>);

impl OutputFilter {
    pub fn capture() -> Self {
        Self(current_store().ok())
    }

    pub fn redact(&self, text: &str) -> String {
        match &self.0 {
            Some(store) => store.lock().redact(text),
            None => text.to_owned(),
        }
    }

    pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        let Some(store) = &self.0 else {
            return std::borrow::Cow::Borrowed(bytes);
        };
        let store = store.lock();
        if !store.has_secret_filters() {
            return std::borrow::Cow::Borrowed(bytes);
        }
        let redacted = store.redact_bytes(bytes);
        if redacted == bytes {
            std::borrow::Cow::Borrowed(bytes)
        } else {
            std::borrow::Cow::Owned(redacted)
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

/// Exact-value filtering complements the shared pattern policy. No store or
/// active session is a real empty set, not an ambient credential fallback.
pub fn redact(text: &str) -> String {
    OutputFilter::capture().redact(text)
}
