use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

use crate::value::VmValue;

static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);
static HANDLE_STORE: LazyLock<Mutex<BTreeMap<String, HandleEntry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

struct HandleEntry {
    session_id: String,
    cancel: Arc<AtomicBool>,
}

pub(crate) struct OperationHandleInfo {
    pub handle_id: String,
    pub started_at: String,
    pub operation: String,
    pub descriptor: String,
}

impl OperationHandleInfo {
    pub(crate) fn into_vm_value(self) -> VmValue {
        let mut dict = BTreeMap::new();
        dict.insert(
            "handle_id".to_string(),
            VmValue::String(Rc::from(self.handle_id)),
        );
        dict.insert(
            "started_at".to_string(),
            VmValue::String(Rc::from(self.started_at)),
        );
        dict.insert("ended_at".to_string(), VmValue::Nil);
        dict.insert("duration_ms".to_string(), VmValue::Int(0));
        dict.insert("status".to_string(), VmValue::String(Rc::from("running")));
        dict.insert(
            "operation".to_string(),
            VmValue::String(Rc::from(self.operation)),
        );
        dict.insert(
            "command_or_op_descriptor".to_string(),
            VmValue::String(Rc::from(self.descriptor)),
        );
        VmValue::Dict(Rc::new(dict))
    }
}

pub(crate) fn spawn_json_operation<F>(
    operation: impl Into<String>,
    descriptor: impl Into<String>,
    session_id: String,
    run: F,
) -> Result<OperationHandleInfo, String>
where
    F: FnOnce(Arc<AtomicBool>) -> Result<serde_json::Value, String> + Send + 'static,
{
    register_cleanup_hook();
    let handle_id = format!(
        "hso-{:x}-{}",
        std::process::id(),
        HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let started_at = chrono::Utc::now().to_rfc3339();
    let operation = operation.into();
    let descriptor = descriptor.into();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut store = HANDLE_STORE
            .lock()
            .expect("stdlib long-running handle store poisoned");
        store.insert(
            handle_id.clone(),
            HandleEntry {
                session_id: session_id.clone(),
                cancel: cancel.clone(),
            },
        );
    }

    let worker_handle_id = handle_id.clone();
    let worker_started_at = started_at.clone();
    let worker_operation = operation.clone();
    let worker_descriptor = descriptor.clone();
    std::thread::Builder::new()
        .name(format!("hso-worker-{worker_handle_id}"))
        .spawn(move || {
            let started = Instant::now();
            let result = run(cancel.clone());
            let entry = {
                let mut store = HANDLE_STORE
                    .lock()
                    .expect("stdlib long-running handle store poisoned");
                store.remove(&worker_handle_id)
            };
            if cancel.load(Ordering::Acquire) {
                return;
            }
            let Some(entry) = entry else {
                return;
            };
            let mut payload = serde_json::Map::new();
            payload.insert(
                "handle_id".to_string(),
                serde_json::Value::String(worker_handle_id),
            );
            payload.insert(
                "operation".to_string(),
                serde_json::Value::String(worker_operation),
            );
            payload.insert(
                "command_or_op_descriptor".to_string(),
                serde_json::Value::String(worker_descriptor),
            );
            payload.insert(
                "started_at".to_string(),
                serde_json::Value::String(worker_started_at),
            );
            payload.insert(
                "ended_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );
            payload.insert(
                "duration_ms".to_string(),
                serde_json::Value::Number((started.elapsed().as_millis() as u64).into()),
            );
            match result {
                Ok(value) => {
                    payload.insert(
                        "status".to_string(),
                        serde_json::Value::String("completed".to_string()),
                    );
                    payload.insert("result".to_string(), value);
                }
                Err(error) => {
                    payload.insert(
                        "status".to_string(),
                        serde_json::Value::String("failed".to_string()),
                    );
                    payload.insert("error".to_string(), serde_json::Value::String(error));
                }
            }
            let content = serde_json::to_string(&payload).unwrap_or_default();
            crate::llm::push_pending_feedback_global(&entry.session_id, "tool_result", &content);
        })
        .map_err(|error| {
            let entry = {
                let mut store = HANDLE_STORE
                    .lock()
                    .expect("stdlib long-running handle store poisoned");
                store.remove(&handle_id)
            };
            if let Some(entry) = entry {
                entry.cancel.store(true, Ordering::Release);
            }
            format!("failed to spawn stdlib long-running worker: {error}")
        })?;

    Ok(OperationHandleInfo {
        handle_id,
        started_at,
        operation,
        descriptor,
    })
}

pub fn cancel_handle(handle_id: &str) -> bool {
    let entry = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("stdlib long-running handle store poisoned");
        store.remove(handle_id)
    };
    match entry {
        Some(entry) => {
            entry.cancel.store(true, Ordering::Release);
            true
        }
        None => false,
    }
}

pub(crate) fn cancel_session_handles(session_id: &str) {
    let entries = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("stdlib long-running handle store poisoned");
        let matching = store
            .iter()
            .filter(|(_, entry)| entry.session_id == session_id)
            .map(|(handle_id, _)| handle_id.clone())
            .collect::<Vec<_>>();
        matching
            .into_iter()
            .filter_map(|handle_id| store.remove(&handle_id))
            .collect::<Vec<_>>()
    };
    for entry in entries {
        entry.cancel.store(true, Ordering::Release);
    }
}

pub(crate) fn register_cleanup_hook() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let hook: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(cancel_session_handles);
        crate::llm::register_session_end_hook(hook);
    });
}

pub(crate) fn reset_state() {
    let entries = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("stdlib long-running handle store poisoned");
        std::mem::take(&mut *store)
    };
    for (_, entry) in entries {
        entry.cancel.store(true, Ordering::Release);
    }
}
