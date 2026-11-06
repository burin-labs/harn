use crate::NativeBuiltinCtxExt;
use std::sync::{Arc, Mutex};

use super::host_agent_dispatch_tool_call;
struct SessionObservingBridge(Arc<Mutex<Vec<Option<String>>>>);

impl crate::HostCallBridge for SessionObservingBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a crate::value::DictMap,
    ) -> crate::HostCallDispatchFuture<'a> {
        assert_eq!((capability, operation), ("proof", "session"));
        assert!(params.is_empty());
        self.0
            .lock()
            .expect("observed-session mutex")
            .push(crate::agent_sessions::current_session_id());
        crate::host_call_ready(Ok(Some(crate::schema::json_to_vm_value(
            &serde_json::json!({"status": "ok"}),
        ))))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_dispatch_session_owns_host_execution_and_restores_the_caller() {
    crate::agent_sessions::reset_session_store();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let _bridge =
        crate::install_host_call_bridge(Arc::new(SessionObservingBridge(observed.clone())));
    let _ambient = crate::agent_sessions::enter_current_session("unrelated-ambient-session");
    let tools = crate::schema::json_to_vm_value(&serde_json::json!({
        "_type": "tool_registry",
        "tools": [{
            "name": "session_probe",
            "description": "Observe the dispatch session at the host execution boundary.",
            "executor": "host_bridge",
            "host_capability": "proof.session",
            "parameters": {},
        }],
    }));
    let call = crate::schema::json_to_vm_value(&serde_json::json!({
        "id": "session-scope-proof",
        "name": "session_probe",
        "arguments": {},
    }));
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::schema::json_to_vm_value(&serde_json::json!("resolved-dispatch-session")),
    );

    let mut vm = crate::new_vm();
    crate::register_vm_stdlib(&mut vm);
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(vm),
        call,
        Some(&tools),
        &options,
    )
    .await
    .expect("host dispatch succeeds");
    let result = crate::llm::helpers::vm_value_to_json(&result);

    assert_eq!(result["ok"], serde_json::json!(true));
    assert_eq!(result["result"]["status"], "ok");
    assert_eq!(
        *observed.lock().expect("observed-session mutex"),
        vec![Some("resolved-dispatch-session".to_string())]
    );
    assert_eq!(
        crate::agent_sessions::current_session_id().as_deref(),
        Some("unrelated-ambient-session")
    );
}
