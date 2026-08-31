//! Final ToolRegistry exposure guard for model-requested dispatch.
//!
//! Agent options are projected before prompt construction and discovery, but
//! a model can still forge a name it was never served. Once an explicit
//! registry exists, this module makes that projected registry the complete
//! callable surface before dispatch can fall through to a VM builtin or host
//! bridge. Calls without an explicit registry retain the legacy ambient
//! builtin surface.

use super::agent_tools::ToolDispatchOutcome;
use crate::value::{ErrorCategory, VmError, VmValue};

pub(super) fn registry_dispatch_rejection(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<ToolDispatchOutcome> {
    require_registry_membership(tools_val, tool_name)
        .err()
        .map(|message| ToolDispatchOutcome {
            result: Err(VmError::CategorizedError {
                message,
                category: ErrorCategory::ToolRejected,
            }),
            executor: None,
        })
}

fn require_registry_membership(tools_val: Option<&VmValue>, tool_name: &str) -> Result<(), String> {
    let Some(tools_val) = tools_val else {
        return Ok(());
    };
    let Some(dict) = tools_val.as_dict() else {
        return Err(malformed_registry_message(tool_name));
    };
    let Some(VmValue::List(tools)) = dict.get("tools") else {
        return Err(malformed_registry_message(tool_name));
    };
    let Some(entry) = tools.iter().find_map(|tool| {
        let VmValue::Dict(entry) = tool else {
            return None;
        };
        (entry.get("name").map(VmValue::display).as_deref() == Some(tool_name)).then_some(entry)
    }) else {
        return Err(format!(
            "tool '{tool_name}' is not present in the active agent tool registry"
        ));
    };
    match crate::tool_registry::tool_entry_allows_audience(
        entry,
        crate::tool_registry::ToolAudience::Agent,
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "tool '{tool_name}' is not exposed to the agent/model adapter"
        )),
        Err(error) => Err(format!(
            "tool '{tool_name}' has invalid agent/model governance: {error}"
        )),
    }
}

fn malformed_registry_message(tool_name: &str) -> String {
    format!(
        "tool '{tool_name}' cannot be dispatched because the active agent tool registry is malformed"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use tokio::sync::Mutex;

    use super::*;
    use crate::agent_events::ToolExecutor;
    use crate::value::{DictMap, VmDictExt};

    fn literal_registry(name: &str, mut entry: DictMap) -> VmValue {
        entry.put_str("name", name);
        let mut registry = DictMap::new();
        registry.insert(
            crate::value::intern_key("tools"),
            VmValue::List(Arc::new(vec![VmValue::dict(entry)])),
        );
        VmValue::dict(registry)
    }

    fn excluded_registry(name: &str, mut entry: DictMap) -> VmValue {
        let mut governance = DictMap::new();
        governance.insert(
            crate::value::intern_key("audiences"),
            VmValue::List(Arc::new(vec![VmValue::String("cli".into())])),
        );
        entry.insert(
            crate::value::intern_key("governance"),
            VmValue::dict(governance),
        );
        crate::tool_registry::project_tools_for_audience(
            &literal_registry(name, entry),
            crate::tool_registry::ToolAudience::Agent,
        )
        .expect("project registry")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projected_registry_blocks_excluded_host_bridge_fallthrough() {
        let bridge_called = Arc::new(AtomicBool::new(false));
        let writer_called = Arc::clone(&bridge_called);
        let bridge = Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |_| {
                writer_called.store(true, Ordering::SeqCst);
                Err("excluded tool reached host bridge".to_string())
            }),
            1,
        ));
        let mut entry = DictMap::new();
        entry.put_str("executor", "host_bridge");
        entry.put_str("host_capability", "operator.inspect");
        let tools = excluded_registry("operator_inspect", entry);

        let outcome = super::super::agent_tools::dispatch_tool_execution(
            "operator_inspect",
            &serde_json::json!({}),
            Some(&tools),
            Some(&bridge),
            0,
            0,
        )
        .await;

        assert!(!bridge_called.load(Ordering::SeqCst));
        assert!(outcome.executor.is_none());
        let error = outcome.result.unwrap_err().to_string();
        assert!(error.contains("not present in the active agent tool registry"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projected_registry_blocks_excluded_local_short_circuit_fallthrough() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("private.txt");
        std::fs::write(&path, "LOCAL_SHORT_CIRCUIT_RAN").expect("write fixture");
        let tools = excluded_registry("read_file", DictMap::new());

        let outcome = super::super::agent_tools::dispatch_tool_execution(
            "read_file",
            &serde_json::json!({"path": path}),
            Some(&tools),
            None,
            0,
            0,
        )
        .await;

        assert!(outcome.executor.is_none());
        let error = outcome.result.unwrap_err().to_string();
        assert!(error.contains("not present in the active agent tool registry"));
        assert!(!error.contains("LOCAL_SHORT_CIRCUIT_RAN"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forged_literal_operator_cannot_dispatch_to_the_agent() {
        let bridge_called = Arc::new(AtomicBool::new(false));
        let writer_called = Arc::clone(&bridge_called);
        let bridge = Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |_| {
                writer_called.store(true, Ordering::SeqCst);
                Err("forged operator reached host bridge".to_string())
            }),
            1,
        ));
        let mut governance = DictMap::new();
        governance.insert(
            crate::value::intern_key("audiences"),
            VmValue::List(Arc::new(vec![VmValue::String("agent".into())])),
        );
        let mut entry = DictMap::new();
        entry.put_str("executor", "host_bridge");
        entry.put_str("host_capability", "operator.inspect");
        entry.insert(
            crate::value::intern_key("governance"),
            VmValue::dict(governance),
        );
        let tools = literal_registry("operator.inspect", entry);

        let outcome = super::super::agent_tools::dispatch_tool_execution(
            "operator.inspect",
            &serde_json::json!({}),
            Some(&tools),
            Some(&bridge),
            0,
            0,
        )
        .await;

        assert!(!bridge_called.load(Ordering::SeqCst));
        assert!(outcome.executor.is_none());
        let error = outcome.result.unwrap_err().to_string();
        assert!(error.contains("invalid agent/model governance"));
        assert!(error.contains("cannot be exposed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn literal_non_operator_keeps_compatibility_dispatch() {
        let bridge_called = Arc::new(AtomicBool::new(false));
        let writer_called = Arc::clone(&bridge_called);
        let bridge = Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |_| {
                writer_called.store(true, Ordering::SeqCst);
                Err("expected compatibility dispatch".to_string())
            }),
            1,
        ));
        let mut entry = DictMap::new();
        entry.put_str("executor", "host_bridge");
        let tools = literal_registry("legacy.inspect", entry);

        let outcome = super::super::agent_tools::dispatch_tool_execution(
            "legacy.inspect",
            &serde_json::json!({}),
            Some(&tools),
            Some(&bridge),
            0,
            0,
        )
        .await;

        assert!(bridge_called.load(Ordering::SeqCst));
        assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
    }
}
