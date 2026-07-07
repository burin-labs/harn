//! MCP `roots/list` plumbing for Harn-served MCP handlers.

use serde_json::{json, Value as JsonValue};

use crate::mcp_client_request::current_bus;
use crate::mcp_protocol::METHOD_ROOTS_LIST;
use crate::schema::json_to_vm_value;
use crate::value::{VmError, VmValue};

pub async fn request_client_roots() -> Result<VmValue, VmError> {
    let bus = current_bus().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "mcp_client_roots: no active MCP client connection — \
             mcp_client_roots can only be called from within a tool/resource/prompt handler \
             served via `harn serve mcp`",
        )))
    })?;

    let result = bus
        .request("roots", METHOD_ROOTS_LIST, json!({}), "mcp_client_roots")
        .await?;
    roots_from_response(&result)
}

fn roots_from_response(result: &JsonValue) -> Result<VmValue, VmError> {
    let roots = result.get("roots").ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "mcp_client_roots: client response missing 'roots'",
        )))
    })?;
    if !roots.is_array() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "mcp_client_roots: client response 'roots' must be a list",
        ))));
    }
    Ok(json_to_vm_value(roots))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    use crate::mcp_client_request::{install_bus, ClientRequestBus};

    #[tokio::test(flavor = "current_thread")]
    async fn client_roots_requires_active_served_connection() {
        let previous = install_bus(None);
        let err = request_client_roots()
            .await
            .expect_err("outside served handler should fail");
        install_bus(previous);
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => format!("{other:?}"),
        };
        assert!(message.contains("no active MCP client connection"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn client_roots_round_trips_over_request_bus() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bus = ClientRequestBus::new(tx);
        let bus_for_responder = bus.clone();
        tokio::spawn(async move {
            let outbound = rx.recv().await.expect("roots request emitted");
            let id = outbound["id"].clone();
            assert_eq!(outbound["method"], json!(METHOD_ROOTS_LIST));
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "roots": [
                        {"uri": "file:///tmp/project", "name": "project", "path": "/tmp/project"}
                    ]
                }
            });
            assert!(bus_for_responder.route_response(&response));
        });

        let previous = install_bus(Some(bus));
        let result = request_client_roots()
            .await
            .expect("roots request succeeds");
        install_bus(previous);
        let VmValue::List(roots) = result else {
            panic!("roots should be a list");
        };
        assert_eq!(roots.len(), 1);
        assert!(roots[0].display().contains("file:///tmp/project"));
    }
}
