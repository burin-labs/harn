//! Final ToolRegistry exposure guard for model-requested dispatch.
//!
//! Agent options are projected before prompt construction and discovery, but
//! a model can still forge a name it was never served. Once an explicit
//! registry exists, this module makes that projected registry the complete
//! callable surface before dispatch can fall through to a VM builtin or host
//! bridge. Calls without an explicit registry retain the legacy ambient
//! builtin surface.

use super::agent_tools::ToolDispatchOutcome;
use crate::stdlib::macros::harn_builtin;
use crate::value::{ErrorCategory, VmError, VmResourceHandle, VmValue};

const REGISTRY_PROVENANCE_KEY: &str = "_agent_registry_provenance";
const AMBIENT_PROVENANCE_LABEL: &str = "agent_registry_ambient_host";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentRegistryOrigin {
    Explicit,
    AmbientHost,
}

impl AgentRegistryOrigin {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "ambient_host" => Some(Self::AmbientHost),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct AmbientHostRegistryProvenance;

pub(super) fn own_lifecycle_registry(
    registry: &VmValue,
    origin: AgentRegistryOrigin,
) -> Result<VmValue, VmError> {
    crate::tool_registry::tool_registry_catalog(registry)?;
    let mut owned = registry
        .as_dict()
        .expect("validated tool registry must be a dictionary")
        .as_ref()
        .clone();
    match origin {
        AgentRegistryOrigin::Explicit => {
            owned.remove(REGISTRY_PROVENANCE_KEY);
        }
        AgentRegistryOrigin::AmbientHost => {
            owned.insert(
                REGISTRY_PROVENANCE_KEY.into(),
                VmValue::resource(VmResourceHandle::new(
                    AMBIENT_PROVENANCE_LABEL,
                    AmbientHostRegistryProvenance,
                )),
            );
        }
    }
    Ok(VmValue::dict(owned))
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_own_lifecycle_registry(registry: dict, origin: \"explicit\" | \"ambient_host\") -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_own_lifecycle_registry_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let registry = args.first().ok_or_else(|| {
        VmError::Runtime("__host_agent_own_lifecycle_registry: missing registry".into())
    })?;
    let origin = args
        .get(1)
        .and_then(|value| match value {
            VmValue::String(value) => Some(value.as_str()),
            _ => None,
        })
        .and_then(AgentRegistryOrigin::parse)
        .ok_or_else(|| {
            VmError::Runtime(
                "__host_agent_own_lifecycle_registry: origin must be explicit or ambient_host"
                    .into(),
            )
        })?;
    own_lifecycle_registry(registry, origin)
}

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
            declared_failure: None,
        })
}

fn require_registry_membership(tools_val: Option<&VmValue>, tool_name: &str) -> Result<(), String> {
    let Some(tools_val) = tools_val else {
        return Ok(());
    };
    let Some(dict) = tools_val.as_dict() else {
        return Err(malformed_registry_message(tool_name));
    };
    let origin = match dict.get(REGISTRY_PROVENANCE_KEY) {
        None => AgentRegistryOrigin::Explicit,
        Some(VmValue::Resource(handle))
            if handle.downcast::<AmbientHostRegistryProvenance>().is_some() =>
        {
            AgentRegistryOrigin::AmbientHost
        }
        Some(_) => return Err(malformed_registry_message(tool_name)),
    };
    let Some(VmValue::List(tools)) = dict.get("tools") else {
        return Err(malformed_registry_message(tool_name));
    };
    let entry = tools.iter().find_map(|tool| {
        let VmValue::Dict(entry) = tool else {
            return None;
        };
        (entry.get("name").map(VmValue::display).as_deref() == Some(tool_name)).then_some(entry)
    });
    let Some(entry) = entry else {
        if origin == AgentRegistryOrigin::AmbientHost {
            // `agent_loop` adds lifecycle controls to an otherwise absent
            // registry. That synthesized registry is not a declaration that
            // the host bridge has no other tools. Explicit caller registries
            // remain closed and cannot take this compatibility path.
            return Ok(());
        }
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
        Arc, Mutex as StdMutex,
    };

    use tokio::sync::Mutex;

    use super::*;
    use crate::value::{DictMap, VmDictExt};

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
        entry.put_str("name", name);
        let mut registry = DictMap::new();
        registry.insert(
            crate::value::intern_key("tools"),
            VmValue::List(Arc::new(vec![VmValue::dict(entry)])),
        );
        let raw = VmValue::dict(registry);
        crate::tool_registry::project_tools_for_audience(
            &raw,
            crate::tool_registry::ToolAudience::Agent,
        )
        .expect("project registry")
    }

    fn registry(entries: Vec<VmValue>) -> VmValue {
        let mut registry = DictMap::new();
        registry.put_str("_type", "tool_registry");
        registry.insert("tools".into(), VmValue::List(Arc::new(entries)));
        VmValue::dict(registry)
    }

    fn responding_bridge(
        requests: Arc<StdMutex<Vec<serde_json::Value>>>,
    ) -> Arc<crate::bridge::HostBridge> {
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let response_pending = Arc::clone(&pending);
        let writer = Arc::new(move |line: &str| {
            let request: serde_json::Value = serde_json::from_str(line)
                .map_err(|error| format!("invalid bridge request: {error}"))?;
            requests
                .lock()
                .map_err(|_| "request lock poisoned".to_string())?
                .push(request.clone());
            let id = request["id"]
                .as_u64()
                .ok_or_else(|| "missing request id".to_string())?;
            let sender: tokio::sync::oneshot::Sender<serde_json::Value> = response_pending
                .try_lock()
                .map_err(|_| "pending lock busy".to_string())?
                .remove(&id)
                .ok_or_else(|| "request was not pending".to_string())?;
            sender
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"status": "stopped"},
                }))
                .map_err(|_| "bridge caller dropped".to_string())
        });
        Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
            pending,
            Arc::new(AtomicBool::new(false)),
            writer,
            1,
        ))
    }

    fn tool_entry(name: &str) -> VmValue {
        let mut entry = DictMap::new();
        entry.put_str("name", name);
        VmValue::dict(entry)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_owned_ambient_registry_reaches_host_but_explicit_empty_stays_closed() {
        let ambient = own_lifecycle_registry(
            &registry(vec![tool_entry("agent_await_resumption")]),
            AgentRegistryOrigin::AmbientHost,
        )
        .expect("own ambient lifecycle registry");
        let explicit_empty =
            own_lifecycle_registry(&registry(Vec::new()), AgentRegistryOrigin::Explicit)
                .expect("own explicit empty registry");
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let bridge = responding_bridge(Arc::clone(&requests));

        let reached = super::super::agent_tools::dispatch_tool_execution_with_mcp(
            None,
            "session_stop",
            &serde_json::json!({"reason": "operator stop"}),
            Some(&ambient),
            None,
            Some(&bridge),
            0,
            0,
        )
        .await;
        assert!(
            reached.result.is_ok(),
            "ambient host call failed: {:?}",
            reached.result
        );
        assert_eq!(
            reached.executor,
            Some(crate::agent_events::ToolExecutor::HostBridge)
        );
        let reached_count = requests.lock().expect("request lock").len();
        assert_eq!(
            reached_count, 1,
            "known-positive ambient call did not reach host"
        );

        let denied = super::super::agent_tools::dispatch_tool_execution_with_mcp(
            None,
            "session_stop",
            &serde_json::json!({"reason": "operator stop"}),
            Some(&explicit_empty),
            None,
            Some(&bridge),
            0,
            0,
        )
        .await;
        assert!(denied.result.is_err());
        assert!(denied.executor.is_none());
        assert_eq!(
            requests.lock().expect("request lock").len(),
            reached_count,
            "explicit empty registry reached the host after the known-positive control"
        );
    }

    #[test]
    fn script_visible_marker_shapes_cannot_downgrade_an_explicit_registry() {
        for forged in [VmValue::Bool(false), VmValue::String("ambient_host".into())] {
            let registry = registry(Vec::new());
            let mut forged_registry = registry.as_dict().expect("registry dict").as_ref().clone();
            forged_registry.insert(REGISTRY_PROVENANCE_KEY.into(), forged);
            let error =
                require_registry_membership(Some(&VmValue::dict(forged_registry)), "session_stop")
                    .expect_err("script-visible marker must fail closed");
            assert!(error.contains("malformed"));
        }
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
}
