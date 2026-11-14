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
/// process store. A current session the runtime has no store for, such as one
/// already removed, also falls back to the process store, so a granted value
/// is never exposed unfiltered.
fn filter_store(create: bool) -> Store {
    crate::current_agent_session_id()
        .and_then(|id| store(&id, create).ok())
        .unwrap_or_else(process_store)
}

pub(crate) fn has_private_filter() -> bool {
    filter_store(false).lock().has_secret_filters()
}

/// Below this a value is too likely to occur in unrelated output.
const MIN_FILTERED_VALUE_LEN: usize = 8;

/// Filter a credential a grant is about to hand a child from this session's
/// output, or from the process's output when no session store is available.
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
    filter_store(true).lock().add_filter(value)
}

/// Retain the filter for one operation, including output received after close.
/// Dropping the operation releases this reference to the private material.
pub struct OutputFilter(Store);

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
        self.0.lock().redact(text)
    }

    pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        let stored = {
            let store = self.0.lock();
            store
                .has_secret_filters()
                .then(|| store.redact_bytes(bytes))
                .filter(|redacted| redacted != bytes)
        };
        match stored {
            Some(redacted) => std::borrow::Cow::Owned(redacted),
            None => std::borrow::Cow::Borrowed(bytes),
        }
    }

    /// A redactor for one output stream that arrives in chunks.
    pub fn stream(&self) -> StreamRedactor {
        StreamRedactor {
            filter: self.0.clone(),
            pending: Vec::new(),
        }
    }
}

/// Redacts one output stream before anything reads or stores it. A tail that
/// begins a filtered value is held until the next chunk or the end of the
/// stream, so a value split across reads is still caught whole.
pub struct StreamRedactor {
    filter: Store,
    pending: Vec<u8>,
}

impl StreamRedactor {
    /// The redacted bytes of `chunk` (and of any held tail) that later output
    /// can no longer change.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let store = self.filter.lock();
        if !store.has_secret_filters() {
            return std::mem::take(&mut self.pending);
        }
        let settled = store.settled_len(&self.pending);
        let redacted = store.redact_bytes(&self.pending[..settled]);
        drop(store);
        self.pending.drain(..settled);
        redacted
    }

    /// The redacted held tail, once the stream has ended.
    pub fn finish(&mut self) -> Vec<u8> {
        let pending = std::mem::take(&mut self.pending);
        self.filter.lock().redact_bytes(&pending)
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

/// Exact-value filtering complements the shared pattern policy. Without a
/// session store, the process store applies; it holds only credentials a grant
/// handed a child, never an ambient credential fallback.
pub fn redact(text: &str) -> String {
    OutputFilter::capture().redact(text)
}
