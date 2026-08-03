//! Stable MCP `roots/list` input rounds for Harn-served MCP handlers.

use serde_json::{json, Value as JsonValue};

use crate::mcp_protocol::METHOD_ROOTS_LIST;
use crate::schema::json_to_vm_value;
use crate::value::{VmError, VmValue};

pub async fn request_client_roots() -> Result<VmValue, VmError> {
    let result = crate::mcp_input::request_input(METHOD_ROOTS_LIST, json!({}), "mcp_client_roots")?;
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
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "current_thread")]
    async fn client_roots_requires_active_served_connection() {
        let err = request_client_roots()
            .await
            .expect_err("outside served handler should fail");
        let message = match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => format!("{other:?}"),
        };
        assert!(message.contains("no active MCP request"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn client_roots_resolves_stable_input_response() {
        let result = crate::mcp_input::scope_input_context(
            &json!({"inputResponses": {"harn-input-0": {
                "roots": [
                    {"uri": "file:///tmp/project", "name": "project", "path": "/tmp/project"}
                ]
            }}}),
            json!({"roots": {}}),
            request_client_roots(),
        )
        .await
        .unwrap()
        .expect("roots request succeeds");
        let VmValue::List(roots) = result else {
            panic!("roots should be a list");
        };
        assert_eq!(roots.len(), 1);
        assert!(roots[0].display().contains("file:///tmp/project"));
    }
}
