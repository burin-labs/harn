//! Shared MCP protocol-version and feature-gap helpers.

use serde_json::{json, Value as JsonValue};

pub const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedMcpMethod {
    pub method: &'static str,
    pub feature: &'static str,
    pub role: &'static str,
    pub reason: &'static str,
}

pub const UNSUPPORTED_LATEST_SPEC_METHODS: &[UnsupportedMcpMethod] = &[
    UnsupportedMcpMethod {
        method: "completion/complete",
        feature: "completions",
        role: "server",
        reason: "Harn does not expose prompt or resource-template argument completion.",
    },
    UnsupportedMcpMethod {
        method: "resources/subscribe",
        feature: "resource subscriptions",
        role: "server",
        reason: "Harn resources are read on demand and are not backed by subscription state.",
    },
    UnsupportedMcpMethod {
        method: "resources/unsubscribe",
        feature: "resource subscriptions",
        role: "server",
        reason: "Harn resources are read on demand and are not backed by subscription state.",
    },
    UnsupportedMcpMethod {
        method: "roots/list",
        feature: "roots",
        role: "client",
        reason: "Harn does not currently expose host root discovery to MCP servers.",
    },
    UnsupportedMcpMethod {
        method: "sampling/createMessage",
        feature: "sampling",
        role: "client",
        reason: "Harn does not currently let MCP servers invoke a client-side model sampler.",
    },
    UnsupportedMcpMethod {
        method: "elicitation/create",
        feature: "elicitation",
        role: "client",
        reason: "Harn does not currently let MCP servers request client-side user elicitation.",
    },
    UnsupportedMcpMethod {
        method: "tasks/get",
        feature: "tasks",
        role: "server",
        reason: "Harn MCP tools execute inline; MCP task polling is not implemented.",
    },
    UnsupportedMcpMethod {
        method: "tasks/result",
        feature: "tasks",
        role: "server",
        reason: "Harn MCP tools execute inline; MCP task result retrieval is not implemented.",
    },
    UnsupportedMcpMethod {
        method: "tasks/list",
        feature: "tasks",
        role: "server",
        reason: "Harn MCP tools execute inline; MCP task listing is not implemented.",
    },
    UnsupportedMcpMethod {
        method: "tasks/cancel",
        feature: "tasks",
        role: "server",
        reason: "Harn MCP cancellation uses notifications/cancelled instead of MCP tasks.",
    },
];

pub fn unsupported_latest_spec_method(method: &str) -> Option<&'static UnsupportedMcpMethod> {
    UNSUPPORTED_LATEST_SPEC_METHODS
        .iter()
        .find(|entry| entry.method == method)
}

pub fn unsupported_latest_spec_method_response(
    id: impl Into<JsonValue>,
    method: &str,
) -> Option<JsonValue> {
    unsupported_latest_spec_method(method).map(|entry| {
        crate::jsonrpc::error_response_with_data(
            id,
            -32601,
            &format!("Unsupported MCP method: {method}"),
            unsupported_method_data(entry),
        )
    })
}

pub fn unsupported_task_augmentation_response(id: impl Into<JsonValue>, method: &str) -> JsonValue {
    crate::jsonrpc::error_response_with_data(
        id,
        -32602,
        "MCP task-augmented execution is not supported",
        json!({
            "type": "mcp.unsupportedFeature",
            "protocolVersion": PROTOCOL_VERSION,
            "method": method,
            "feature": "tasks",
            "status": "unsupported",
            "reason": "Harn MCP tools execute inline and do not advertise taskSupport.",
        }),
    )
}

pub fn requests_task_augmentation(params: &JsonValue) -> bool {
    params.get("task").is_some()
}

fn unsupported_method_data(entry: &UnsupportedMcpMethod) -> JsonValue {
    json!({
        "type": "mcp.unsupportedFeature",
        "protocolVersion": PROTOCOL_VERSION,
        "method": entry.method,
        "feature": entry.feature,
        "role": entry.role,
        "status": "unsupported",
        "reason": entry.reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_spec_gap_methods_are_explicit() {
        for method in [
            "completion/complete",
            "resources/subscribe",
            "resources/unsubscribe",
            "roots/list",
            "sampling/createMessage",
            "elicitation/create",
            "tasks/get",
            "tasks/result",
            "tasks/list",
            "tasks/cancel",
        ] {
            let response = unsupported_latest_spec_method_response(json!(1), method)
                .expect("expected explicit unsupported method");
            assert_eq!(response["error"]["code"], json!(-32601));
            assert_eq!(response["error"]["data"]["method"], json!(method));
            assert_eq!(response["error"]["data"]["status"], json!("unsupported"));
        }
    }

    #[test]
    fn task_augmentation_error_is_json_rpc_shaped() {
        let response = unsupported_task_augmentation_response(json!("call-1"), "tools/call");
        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!("call-1"));
        assert_eq!(response["error"]["code"], json!(-32602));
        assert_eq!(response["error"]["data"]["feature"], json!("tasks"));
    }
}
