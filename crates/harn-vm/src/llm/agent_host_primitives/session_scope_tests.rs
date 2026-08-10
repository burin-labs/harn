use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use super::host_agent_dispatch_tool_call;
use crate::bridge::HostBridge;

struct HostBridgeGuard {
    previous: Option<Arc<HostBridge>>,
}

impl HostBridgeGuard {
    fn replace(bridge: Arc<HostBridge>) -> Self {
        Self {
            previous: crate::llm::swap_current_host_bridge(Some(bridge)),
        }
    }
}

impl Drop for HostBridgeGuard {
    fn drop(&mut self) {
        let _ = crate::llm::swap_current_host_bridge(self.previous.take());
    }
}

fn session_observing_bridge(observed: Arc<StdMutex<Option<String>>>) -> Arc<HostBridge> {
    let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let response_pending = pending.clone();
    let writer = Arc::new(move |line: &str| {
        *observed
            .lock()
            .map_err(|_| "observed-session mutex poisoned".to_string())? =
            crate::agent_sessions::current_session_id();
        let request: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid bridge request: {error}"))?;
        let id = request
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "bridge request missing numeric id".to_string())?;
        let sender = response_pending
            .try_lock()
            .map_err(|_| "bridge pending map unexpectedly locked".to_string())?
            .remove(&id)
            .ok_or_else(|| "bridge request was not pending".to_string())?;
        sender
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"status": "ok"},
            }))
            .map_err(|_| "bridge caller dropped before response".to_string())
    });
    Arc::new(HostBridge::from_parts_with_writer(
        pending,
        Arc::new(AtomicBool::new(false)),
        writer,
        1,
    ))
}

#[tokio::test]
async fn resolved_dispatch_session_owns_host_execution_and_restores_the_caller() {
    crate::agent_sessions::reset_session_store();
    let observed = Arc::new(StdMutex::new(None));
    let _bridge = HostBridgeGuard::replace(session_observing_bridge(observed.clone()));
    let _ambient = crate::agent_sessions::enter_current_session("unrelated-ambient-session");
    let tools = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "_type": "tool_registry",
        "tools": [{
            "name": "session_probe",
            "description": "Observe the dispatch session at the host execution boundary.",
            "executor": "host_bridge",
            "parameters": {},
        }],
    }));
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "session-scope-proof",
        "name": "session_probe",
        "arguments": {},
    }));
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::stdlib::json_to_vm_value(&serde_json::json!("resolved-dispatch-session")),
    );

    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        Some(&tools),
        &options,
    )
    .await
    .expect("host dispatch succeeds");
    let result = crate::llm::helpers::vm_value_to_json(&result);

    assert_eq!(result["ok"], serde_json::json!(true));
    assert_eq!(
        observed.lock().expect("observed-session mutex").as_deref(),
        Some("resolved-dispatch-session")
    );
    assert_eq!(
        crate::agent_sessions::current_session_id().as_deref(),
        Some("unrelated-ambient-session")
    );
}
